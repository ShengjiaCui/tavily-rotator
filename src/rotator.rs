//! 轮换状态机。
//!
//! 职责:
//! 1. 定时(每 30 min)查当前 active key 的 /usage
//! 2. 若剩余 < rotate_threshold,触发轮换到下一个 key
//! 3. 月初(UTC 1 号)重置 active 指针回 keys[0]
//! 4. 每次状态变化写 SQLite + 推 launchctl setenv
//!
//! 关键:daemon 的轮换逻辑对 keys.toml 只读(§4.4),永不改 key 池。

use std::sync::Arc;
use std::time::Duration;

use tokio::time::sleep;

use crate::tavily;
use crate::AppState;

/// 提前轮换阈值:剩余 <50 credits 时主动切,给长寿进程留缓冲(§8.3)。
const EARLY_ROTATE_THRESHOLD: u32 = 50;

/// 从 config 读 poll_interval_minutes(运行时可改,改了下个 tick 生效)。
async fn current_poll_interval(state: &Arc<AppState>) -> Duration {
    let cfg = state.config.read().await;
    let mins = if cfg.poll_interval_minutes >= 1 && cfg.poll_interval_minutes <= 1440 {
        cfg.poll_interval_minutes
    } else {
        30
    };
    Duration::from_secs((mins as u64) * 60)
}

/// 启动轮换循环。阻塞当前 task,通常 spawn 到独立 task。
/// 每 tick 重新读 config 的 poll_interval_minutes,改了间隔下个 tick 生效。
pub async fn run(state: Arc<AppState>) {
    tracing::info!("轮换循环启动");

    loop {
        // 当前 tick:先 sleep(首次启动立即跑,不 sleep)
        let interval = current_poll_interval(&state).await;
        tracing::debug!("下次查询在 {} 分钟后", interval.as_secs() / 60);
        sleep(interval).await;

        if let Err(e) = poll_once(&state).await {
            tracing::error!("轮询周期出错(非致命,下个周期重试): {e:#}");
        }
    }
}

/// 单次轮询:查 active key /usage → 判断是否轮换 → 判断是否月初重置。
async fn poll_once(state: &Arc<AppState>) -> anyhow::Result<()> {
    // 0. 月初重置检查(在查 /usage 之前,让指针先回到 keys[0])
    check_monthly_reset(state).await?;

    // 1. 读当前 active key
    let cfg = state.config.read().await.clone();
    let active_idx = state.active_idx.load(std::sync::atomic::Ordering::Relaxed);

    if active_idx == usize::MAX {
        // 全部耗尽状态:只查 /usage 看 key 有没有月初重置(变成有额度)
        return check_recovery(state, &cfg).await;
    }

    if active_idx >= cfg.keys.len() {
        // keys.toml 被改短了,active_idx 越界,回退到 0
        tracing::warn!("active_idx={active_idx} 越界(keys.len()={}),重置为 0", cfg.keys.len());
        set_active(state, 0, &cfg, "active_idx_out_of_bounds").await?;
        return Ok(());
    }

    let active_key = &cfg.keys[active_idx];

    // 2. 查 /usage
    let snapshot = match tavily::query_usage_snapshot(
        &state.http,
        &active_key.secret,
        active_idx,
        &active_key.label,
        now_ts(),
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("查 /usage 失败 (key[{}] \"{}\"): {e:#}", active_idx, active_key.label);
            // /usage 查失败不轮换(key 可能还可用,只是查询端点抖动)
            return Ok(());
        }
    };

    let remaining = snapshot.plan_limit.saturating_sub(snapshot.plan_usage);
    state
        .last_remaining
        .store(remaining, std::sync::atomic::Ordering::Relaxed);

    tracing::info!(
        "key[{}] \"{}\" 用量: {}/{} (剩余 {}, 阈值 {})",
        active_idx,
        active_key.label,
        snapshot.plan_usage,
        snapshot.plan_limit,
        remaining,
        cfg.rotate_threshold
    );

    // 3. 存快照
    if let Err(e) = state.db.insert_usage_snapshot(&snapshot).await {
        tracing::warn!("存 usage_snapshot 失败(非致命): {e}");
    }

    // 4. 判断是否轮换
    if remaining < cfg.rotate_threshold {
        tracing::warn!(
            "key[{}] \"{}\" 剩余 {} < 阈值 {},触发轮换",
            active_idx,
            active_key.label,
            remaining,
            cfg.rotate_threshold
        );
        rotate_to_next(state, &cfg, "threshold_reached").await?;
    } else if remaining < EARLY_ROTATE_THRESHOLD {
        // 提前预警(不轮换,只提示)
        tracing::warn!(
            "⚠ key[{}] \"{}\" 剩余 {} < {EARLY_ROTATE_THRESHOLD},即将轮换,长会话请准备重启",
            active_idx,
            active_key.label,
            remaining
        );
    }

    Ok(())
}

/// 轮换到下一个有效 key(公开版本,供 /api/rotate 调用)。
pub async fn rotate_to_next_public(
    state: &Arc<AppState>,
    cfg: &crate::config::Config,
    reason: &str,
) -> anyhow::Result<()> {
    rotate_to_next(state, cfg, reason).await
}

/// 轮换到下一个有效 key。
/// 顺序:从当前 active_idx 往后找,绕一圈回不到自己(否则说明全耗尽)。
async fn rotate_to_next(
    state: &Arc<AppState>,
    cfg: &crate::config::Config,
    reason: &str,
) -> anyhow::Result<()> {
    let current = state.active_idx.load(std::sync::atomic::Ordering::Relaxed);
    let n = cfg.keys.len();
    if n == 0 {
        return Ok(());
    }

    // 从 (current+1) 开始找,绕一圈
    let mut next = current;
    for offset in 1..=n {
        let candidate = (current + offset) % n;
        // 节流:连续查多个 key 会触发 Tavily rate limit(429),间隔 1.5 秒
        if offset > 1 {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }
        // 验证候选 key 有效(查 /usage 看有没有额度)
        match tavily::query_usage(&state.http, &cfg.keys[candidate].secret).await {
            Ok((plan_usage, plan_limit, _, _, _, _, _)) => {
                let remaining = plan_limit.saturating_sub(plan_usage);
                if remaining >= cfg.rotate_threshold {
                    next = candidate;
                    tracing::info!(
                        "找到下一个有效 key[{}] \"{}\" (剩余 {})",
                        next,
                        cfg.keys[next].label,
                        remaining
                    );
                    break;
                } else {
                    tracing::info!(
                        "key[{}] \"{}\" 也快耗尽 (剩余 {}),跳过",
                        candidate,
                        cfg.keys[candidate].label,
                        remaining
                    );
                    next = usize::MAX; // 标记:暂时没找到,继续找
                }
            }
            Err(e) => {
                tracing::warn!(
                    "key[{}] \"{}\" /usage 查询失败,跳过: {e:#}",
                    candidate,
                    cfg.keys[candidate].label
                );
                next = usize::MAX;
            }
        }
    }

    if next == usize::MAX || next == current {
        // 全部耗尽
        tracing::error!("🚨 所有 key 已耗尽或失效,launchctl unsetenv,等月初重置");
        state
            .active_idx
            .store(usize::MAX, std::sync::atomic::Ordering::Relaxed);
        state
            .env_pushed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        *state.env_pushed_at.write().await = None;
        let _ = crate::env_push::unset_env();

        // 记一条轮换(到"耗尽"状态)
        let _ = state
            .db
            .insert_rotation(&crate::db::Rotation {
                ts: now_ts(),
                from_idx: if current == usize::MAX { None } else { Some(current) },
                from_label: cfg.keys.get(current).map(|k| k.label.clone()),
                to_idx: 0,
                to_label: "(all exhausted)".into(),
                reason: "all_keys_exhausted".into(),
            })
            .await;
        return Ok(());
    }

    // 切到 next
    set_active(state, next, cfg, reason).await
}

/// 设置 active key + 推环境变量 + 记 rotation(公开版本)。
pub async fn set_active_public(
    state: &Arc<AppState>,
    idx: usize,
    cfg: &crate::config::Config,
    reason: &str,
) -> anyhow::Result<()> {
    set_active(state, idx, cfg, reason).await
}

/// 设置 active key + 推环境变量 + 记 rotation。
async fn set_active(
    state: &Arc<AppState>,
    idx: usize,
    cfg: &crate::config::Config,
    reason: &str,
) -> anyhow::Result<()> {
    let old_idx = state.active_idx.load(std::sync::atomic::Ordering::Relaxed);

    if idx >= cfg.keys.len() {
        anyhow::bail!("set_active: idx {idx} 越界");
    }

    // 推环境变量
    let secret = &cfg.keys[idx].secret;
    crate::env_push::push_env(secret)?;

    // 更新内存状态
    state
        .active_idx
        .store(idx, std::sync::atomic::Ordering::Relaxed);
    state
        .env_pushed
        .store(true, std::sync::atomic::Ordering::Relaxed);
    *state.env_pushed_at.write().await = Some(format!("@{}", now_ts()));

    // 更新 SQLite active_pointer
    state.db.set_active_pointer(idx, now_ts()).await?;

    // 记 rotation(首次启动 old_idx == idx 时不记)
    if old_idx != idx {
        let _ = state
            .db
            .insert_rotation(&crate::db::Rotation {
                ts: now_ts(),
                from_idx: if old_idx == usize::MAX {
                    None
                } else {
                    Some(old_idx)
                },
                from_label: cfg.keys.get(old_idx).map(|k| k.label.clone()),
                to_idx: idx,
                to_label: cfg.keys[idx].label.clone(),
                reason: reason.to_string(),
            })
            .await;
    }

    tracing::info!(
        "✓ active = key[{}] \"{}\" (env pushed, reason={})",
        idx,
        cfg.keys[idx].label,
        reason
    );

    // 立刻查一次新 key 的 /usage 更新 remaining(修复 Phase 2 已知小瑕疵)
    // 失败不致命(下个 tick 会查)
    if let Ok((plan_usage, plan_limit, _, _, _, _, _)) =
        crate::tavily::query_usage(&state.http, secret).await
    {
        let remaining = plan_limit.saturating_sub(plan_usage);
        state
            .last_remaining
            .store(remaining, std::sync::atomic::Ordering::Relaxed);
        tracing::info!(
            "新 active key[{}] \"{}\" /usage: {}/{} (剩余 {})",
            idx,
            cfg.keys[idx].label,
            plan_usage,
            plan_limit,
            remaining
        );
    }

    Ok(())
}

/// 月初重置检查:UTC 日期 = 1 号 且 上次检查不是 1 号。
/// 重置动作:active 指针回 keys[0],推环境变量。
/// daemon 不改 keys.toml,只改自己的 pointer(§4.3)。
async fn check_monthly_reset(state: &Arc<AppState>) -> anyhow::Result<()> {
    let (year, month, day, _hour) = utc_now_ymd();
    let cfg = state.config.read().await.clone();

    let last_reset = state
        .last_reset_check
        .load(std::sync::atomic::Ordering::Relaxed);

    // 编码上次检查的 yyyymm,判断月份是否变了
    let this_month = year * 12 + (month as i64);
    let day_is_first = day == 1;

    if day_is_first && this_month != last_reset {
        tracing::info!("📅 月初重置(UTC {year}-{month:02}-01),active 指针回 keys[0]");
        state
            .last_reset_check
            .store(this_month, std::sync::atomic::Ordering::Relaxed);

        if !cfg.keys.is_empty() {
            set_active(state, 0, &cfg, "monthly_reset").await?;
        }
    } else if !day_is_first {
        // 非 1 号,更新 last_reset 为本月(防止跨月后第一次到 1 号时误判)
        state
            .last_reset_check
            .store(this_month, std::sync::atomic::Ordering::Relaxed);
    }

    Ok(())
}

/// 全部耗尽状态下,查 keys[0] 看 Tavily 是不是月初重置了。
async fn check_recovery(
    state: &Arc<AppState>,
    cfg: &crate::config::Config,
) -> anyhow::Result<()> {
    if cfg.keys.is_empty() {
        return Ok(());
    }
    match tavily::query_usage(&state.http, &cfg.keys[0].secret).await {
        Ok((plan_usage, plan_limit, _, _, _, _, _)) => {
            let remaining = plan_limit.saturating_sub(plan_usage);
            if remaining >= cfg.rotate_threshold {
                tracing::info!("🎉 keys[0] 已恢复额度(剩余 {}),月初重置生效,重新激活", remaining);
                set_active(state, 0, cfg, "monthly_recovery").await?;
            }
        }
        Err(e) => {
            tracing::debug!("recovery 检查 /usage 失败(正常,可能还没重置): {e:#}");
        }
    }
    Ok(())
}

/// 当前 unix 时间戳。
fn now_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 当前 UTC (year, month, day, hour)。
/// 不引 chrono,用系统 date 命令(简单可靠)。
fn utc_now_ymd() -> (i64, i64, i64, i64) {
    let out = std::process::Command::new("date")
        .args(["-u", "+%Y %m %d %H"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout);
            let parts: Vec<i64> = s.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            if parts.len() >= 4 {
                (parts[0], parts[1], parts[2], parts[3])
            } else {
                (2026, 1, 1, 0) // fallback,不会正确触发月初重置但不会崩
            }
        }
        _ => (2026, 1, 1, 0),
    }
}
