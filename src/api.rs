//! HTTP API + Web 面板。
//!
//! 路由(全部只绑 127.0.0.1):
//! - GET  /                    Web 面板主页面(HTML)
//! - GET  /api/state           完整状态
//! - GET  /api/active          当前 active key
//! - POST /api/keys            添加 key(先验证 /usage,再原子写 keys.toml)
//! - PUT  /api/keys/:idx       改 label/note
//! - DELETE /api/keys/:idx     删除 key
//! - POST /api/keys/refresh-all  立即查所有 key /usage
//! - POST /api/rotate          手动触发轮换
//! - GET  /api/rotations       切换历史
//! - GET  /health              健康检查

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::config::Key;
use crate::AppState;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/api/state", get(api_state))
        .route("/api/active", get(api_active))
        .route("/api/keys", post(add_key))
        .route("/api/keys/{idx}", axum::routing::put(update_key).delete(delete_key))
        .route("/api/keys/refresh-all", post(refresh_all))
        .route("/api/rotate", post(rotate_now))
        .route("/api/activate/{idx}", post(activate_key))
        .route("/api/config", axum::routing::put(update_config))
        .route("/api/rotations", get(get_rotations))
        .route("/api/environment", get(get_environment))
        .route("/api/install", post(install))
        .with_state(state)
}

// ===================================================================
// Web 面板主页面(HTML 内联,Phase 3 后续可换 askama 模板)
// ===================================================================

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

// ===================================================================
// /api/state — 完整状态(面板刷新用)
// ===================================================================

#[derive(Serialize)]
struct StateResponse {
    active_label: Option<String>,
    active_idx: usize,
    active_remaining: Option<u32>,
    pool_size: usize,
    pool_remaining: u32,
    rotate_threshold: u32,
    poll_interval_minutes: u32,
    env_pushed: bool,
    keys: Vec<KeyView>,
}

#[derive(Serialize)]
struct KeyView {
    idx: usize,
    label: String,
    note: String,
    secret_masked: String,
}

async fn api_state(State(state): State<Arc<AppState>>) -> Json<StateResponse> {
    let cfg = state.config.read().await.clone();
    let active_idx = state.active_idx.load(std::sync::atomic::Ordering::Relaxed);
    let remaining = state.last_remaining.load(std::sync::atomic::Ordering::Relaxed);

    Json(StateResponse {
        active_label: cfg.keys.get(active_idx).map(|k| k.label.clone()),
        active_idx,
        active_remaining: if remaining == 0 { None } else { Some(remaining) },
        pool_size: cfg.keys.len(),
        pool_remaining: 0, // Phase 3 后续算(需要查所有 key,refresh-all 时更新)
        rotate_threshold: cfg.rotate_threshold,
        poll_interval_minutes: cfg.poll_interval_minutes,
        env_pushed: state.env_pushed.load(std::sync::atomic::Ordering::Relaxed),
        keys: cfg
            .keys
            .iter()
            .enumerate()
            .map(|(i, k)| KeyView {
                idx: i,
                label: k.label.clone(),
                note: k.note.clone(),
                secret_masked: mask_secret(&k.secret),
            })
            .collect(),
    })
}

fn mask_secret(s: &str) -> String {
    if s.len() <= 20 {
        "***".into()
    } else {
        format!("{}...{}", &s[..16], &s[s.len() - 4..])
    }
}

// ===================================================================
// /api/active — 当前 active key
// ===================================================================

#[derive(Serialize)]
struct ActiveResponse {
    label: String,
    idx: usize,
    remaining: Option<u32>,
    env_pushed: bool,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

async fn api_active(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = state.config.read().await;
    let active_idx = state.active_idx.load(std::sync::atomic::Ordering::Relaxed);

    if active_idx == usize::MAX || active_idx >= cfg.keys.len() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: if cfg.keys.is_empty() {
                    "pool_empty".into()
                } else {
                    "all_keys_exhausted".into()
                },
            }),
        )
            .into_response();
    }

    let key = &cfg.keys[active_idx];
    let remaining_val = state.last_remaining.load(std::sync::atomic::Ordering::Relaxed);
    Json(ActiveResponse {
        label: key.label.clone(),
        idx: active_idx,
        remaining: if remaining_val == 0 { None } else { Some(remaining_val) },
        env_pushed: state.env_pushed.load(std::sync::atomic::Ordering::Relaxed),
    })
    .into_response()
}

// ===================================================================
// /api/keys POST — 添加 key
// ===================================================================

#[derive(Deserialize)]
struct AddKeyRequest {
    secret: String,
    label: String,
    #[serde(default)]
    note: String,
}

#[derive(Serialize)]
struct AddKeyResponse {
    ok: bool,
    idx: usize,
}

async fn add_key(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddKeyRequest>,
) -> impl IntoResponse {
    // 1. 校验 note ≤100 字符
    if req.note.chars().count() > 100 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "error": "note_too_long",
                "max": 100,
                "actual": req.note.chars().count()
            })),
        )
            .into_response();
    }

    // 2. 校验 label 非空
    if req.label.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": "label_empty"})),
        )
            .into_response();
    }

    // 3. 校验 secret 格式
    if !req.secret.starts_with("tvly-") {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": "invalid_secret_format"})),
        )
            .into_response();
    }

    let mut cfg = state.config.read().await.clone();

    // 4. 校验 label 唯一
    if cfg.keys.iter().any(|k| k.label == req.label) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"ok": false, "error": "duplicate_label"})),
        )
            .into_response();
    }

    // 5. 验证 key 有效(查 /usage)
    match crate::tavily::query_usage(&state.http, &req.secret).await {
        Ok(_) => {}
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false, "error": "invalid_key",
                    "detail": e.to_string()
                })),
            )
                .into_response();
        }
    }

    // 6. 追加 + 原子写
    cfg.keys.push(Key {
        label: req.label.clone(),
        secret: req.secret,
        note: req.note,
    });
    let idx = cfg.keys.len() - 1;

    if let Err(e) = cfg.save_atomic(&crate::config::default_config_path()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": "save_failed", "detail": e.to_string()})),
        )
            .into_response();
    }

    // 7. 更新内存状态
    *state.config.write().await = cfg;

    tracing::info!("添加 key[{}] \"{}\"", idx, req.label);
    Json(AddKeyResponse { ok: true, idx }).into_response()
}

// ===================================================================
// /api/keys/:idx PUT — 改 label/note(不改 secret)
// ===================================================================

#[derive(Deserialize)]
struct UpdateKeyRequest {
    label: Option<String>,
    note: Option<String>,
}

async fn update_key(
    State(state): State<Arc<AppState>>,
    Path(idx): Path<usize>,
    Json(req): Json<UpdateKeyRequest>,
) -> impl IntoResponse {
    let mut cfg = state.config.read().await.clone();

    if idx >= cfg.keys.len() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"ok": false, "error": "idx_out_of_range"})),
        )
            .into_response();
    }

    if let Some(label) = req.label {
        if label.trim().is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"ok": false, "error": "label_empty"})),
            )
                .into_response();
        }
        // 唯一性(排除自己)
        if cfg.keys.iter().enumerate().any(|(i, k)| i != idx && k.label == label) {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"ok": false, "error": "duplicate_label"})),
            )
                .into_response();
        }
        cfg.keys[idx].label = label;
    }

    if let Some(note) = req.note {
        if note.chars().count() > 100 {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false, "error": "note_too_long",
                    "max": 100, "actual": note.chars().count()
                })),
            )
                .into_response();
        }
        cfg.keys[idx].note = note;
    }

    if let Err(e) = cfg.save_atomic(&crate::config::default_config_path()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": "save_failed", "detail": e.to_string()})),
        )
            .into_response();
    }

    *state.config.write().await = cfg;
    Json(serde_json::json!({"ok": true})).into_response()
}

// ===================================================================
// /api/keys/:idx DELETE — 删除 key
// ===================================================================

async fn delete_key(
    State(state): State<Arc<AppState>>,
    Path(idx): Path<usize>,
) -> impl IntoResponse {
    let mut cfg = state.config.read().await.clone();

    if idx >= cfg.keys.len() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"ok": false, "error": "idx_out_of_range"})),
        )
            .into_response();
    }

    let was_active = state.active_idx.load(std::sync::atomic::Ordering::Relaxed) == idx;
    let removed_label = cfg.keys[idx].label.clone();
    cfg.keys.remove(idx);

    if let Err(e) = cfg.save_atomic(&crate::config::default_config_path()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": "save_failed", "detail": e.to_string()})),
        )
            .into_response();
    }

    *state.config.write().await = cfg;

    // 如果删的是 active,重新指向 keys[0] 并推环境变量
    if was_active {
        let new_cfg = state.config.read().await;
        if new_cfg.keys.is_empty() {
            state
                .active_idx
                .store(usize::MAX, std::sync::atomic::Ordering::Relaxed);
            let _ = crate::env_push::unset_env();
            tracing::warn!("删除了 active key \"{}\",池为空,unsetenv", removed_label);
        } else {
            // 通过触发轮换逻辑重新激活 keys[0]
            drop(new_cfg);
            let cfg_for_rotate = state.config.read().await.clone();
            let _ = crate::rotator::set_active_public(&state, 0, &cfg_for_rotate, "active_removed").await;
        }
    } else {
        // 删的非 active key,可能影响 active_idx(如果删在 active 前面,索引要前移)
        let current = state.active_idx.load(std::sync::atomic::Ordering::Relaxed);
        if current != usize::MAX && idx < current {
            state
                .active_idx
                .store(current - 1, std::sync::atomic::Ordering::Relaxed);
            // SQLite 也同步
            let _ = state
                .db
                .set_active_pointer(current - 1, std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs())
                .await;
        }
    }

    tracing::info!("删除 key[{}] \"{}\"", idx, removed_label);
    Json(serde_json::json!({"ok": true})).into_response()
}

// ===================================================================
// /api/keys/refresh-all POST — 立即查所有 key /usage
// ===================================================================

#[derive(Serialize)]
struct RefreshAllResponse {
    results: Vec<RefreshResult>,
}

#[derive(Serialize)]
struct RefreshResult {
    idx: usize,
    label: String,
    remaining: Option<u32>,
    plan_usage: Option<u32>,
    plan_limit: Option<u32>,
    error: Option<String>,
}

async fn refresh_all(State(state): State<Arc<AppState>>) -> Json<RefreshAllResponse> {
    let cfg = state.config.read().await.clone();
    let mut results = Vec::with_capacity(cfg.keys.len());

    for (i, k) in cfg.keys.iter().enumerate() {
        // 节流:Tavily /usage 有 rate limit,连续查多个 key 会 429。
        // 每个 key 之间间隔 1.5 秒(实测足够避开限流)。
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }
        match crate::tavily::query_usage(&state.http, &k.secret).await {
            Ok((plan_usage, plan_limit, _, _, _, _, _)) => {
                let remaining = plan_limit.saturating_sub(plan_usage);
                // 存快照
                let snap = crate::db::UsageSnapshot {
                    key_idx: i,
                    key_label: k.label.clone(),
                    ts: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    plan_usage,
                    plan_limit,
                    search: 0,
                    crawl: 0,
                    extract: 0,
                    map: 0,
                    research: 0,
                };
                let _ = state.db.insert_usage_snapshot(&snap).await;

                // 如果是 active key,更新 last_remaining
                if state.active_idx.load(std::sync::atomic::Ordering::Relaxed) == i {
                    state
                        .last_remaining
                        .store(remaining, std::sync::atomic::Ordering::Relaxed);
                }

                results.push(RefreshResult {
                    idx: i,
                    label: k.label.clone(),
                    remaining: Some(remaining),
                    plan_usage: Some(plan_usage),
                    plan_limit: Some(plan_limit),
                    error: None,
                });
            }
            Err(e) => {
                results.push(RefreshResult {
                    idx: i,
                    label: k.label.clone(),
                    remaining: None,
                    plan_usage: None,
                    plan_limit: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    Json(RefreshAllResponse { results })
}

// ===================================================================
// /api/rotate POST — 手动触发轮换
// ===================================================================

async fn rotate_now(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = state.config.read().await.clone();
    match crate::rotator::rotate_to_next_public(&state, &cfg, "manual").await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

// ===================================================================
// /api/activate/{idx} POST — 立刻切换到指定 key(手动指定,不按轮换顺序)
// ===================================================================

async fn activate_key(
    State(state): State<Arc<AppState>>,
    Path(idx): Path<usize>,
) -> impl IntoResponse {
    let cfg = state.config.read().await.clone();

    if idx >= cfg.keys.len() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"ok": false, "error": "idx_out_of_range"})),
        )
            .into_response();
    }

    // 已经是 active,无需切换
    let current = state.active_idx.load(std::sync::atomic::Ordering::Relaxed);
    if current == idx {
        return Json(serde_json::json!({"ok": true, "already_active": true})).into_response();
    }

    match crate::rotator::set_active_public(&state, idx, &cfg, "manual_activate").await {
        Ok(()) => {
            tracing::info!("手动激活 key[{}] \"{}\"", idx, cfg.keys[idx].label);
            Json(serde_json::json!({"ok": true, "label": cfg.keys[idx].label})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

// ===================================================================
// /api/config PUT — 改全局配置(目前支持 poll_interval_minutes)
// ===================================================================

#[derive(Deserialize)]
struct UpdateConfigRequest {
    poll_interval_minutes: Option<u32>,
    rotate_threshold: Option<u32>,
}

async fn update_config(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateConfigRequest>,
) -> impl IntoResponse {
    let mut cfg = state.config.read().await.clone();
    let mut changed = Vec::new();

    if let Some(mins) = req.poll_interval_minutes {
        if !(1..=1440).contains(&mins) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false, "error": "poll_interval_out_of_range",
                    "min": 1, "max": 1440, "actual": mins
                })),
            )
                .into_response();
        }
        cfg.poll_interval_minutes = mins;
        changed.push(format!("poll_interval_minutes={mins}"));
    }

    if let Some(threshold) = req.rotate_threshold {
        cfg.rotate_threshold = threshold;
        changed.push(format!("rotate_threshold={threshold}"));
    }

    if changed.is_empty() {
        return Json(serde_json::json!({"ok": true, "changed": false})).into_response();
    }

    // 原子写 keys.toml
    if let Err(e) = cfg.save_atomic(&crate::config::default_config_path()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": "save_failed", "detail": e.to_string()})),
        )
            .into_response();
    }

    // 更新内存状态
    *state.config.write().await = cfg;

    tracing::info!("配置更新: {}", changed.join(", "));
    Json(serde_json::json!({"ok": true, "changed": true, "fields": changed})).into_response()
}

// ===================================================================
// /api/rotations GET — 切换历史
// ===================================================================

async fn get_rotations(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.db.recent_rotations(50).await {
        Ok(rotations) => {
            let views: Vec<serde_json::Value> = rotations
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "ts": r.ts,
                        "from_idx": r.from_idx,
                        "from_label": r.from_label,
                        "to_idx": r.to_idx,
                        "to_label": r.to_label,
                        "reason": r.reason,
                    })
                })
                .collect();
            Json(views).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ===================================================================
// /api/environment GET — 环境探测
// ===================================================================

async fn get_environment(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut env = crate::install::detect().await;
    let cfg = state.config.read().await;
    env.pool_size = cfg.keys.len();
    Json(env)
}

// ===================================================================
// /api/install POST — 一键安装(命令硬编码,绝不 tvly login)
// ===================================================================

#[derive(Deserialize)]
struct InstallRequest {
    components: Vec<String>,
}

async fn install(
    State(state): State<Arc<AppState>>,
    Json(req): Json<InstallRequest>,
) -> impl IntoResponse {
    let cmds = crate::install::install_commands(&req.components);
    if cmds.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": "no_valid_components"})),
        )
            .into_response();
    }

    let mut results = Vec::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    for (name, cmd) in &cmds {
        tracing::info!("安装 {} : {:?}", name, cmd);
        let (success, log) = crate::install::run_install_command(cmd);

        // 截取最后 2000 字符存审计(防 log 过大)
        let excerpt = if log.len() > 2000 {
            format!("...(truncated)\n{}", &log[log.len() - 2000..])
        } else {
            log.clone()
        };

        // 落 install_events 表
        let _ = state.db.insert_install_event(now, name, success, &excerpt).await;

        results.push(serde_json::json!({
            "component": name,
            "success": success,
            "log_tail": excerpt.lines().last().unwrap_or(""),
        }));

        tracing::info!(
            "安装 {} {}",
            name,
            if success { "✓ 成功" } else { "✗ 失败" }
        );
    }

    Json(serde_json::json!({"ok": true, "results": results})).into_response()
}

// ===================================================================
// /health
// ===================================================================

async fn health() -> &'static str {
    "ok\n"
}

// ===================================================================
// 主页面 HTML(内联,Phase 3 后续可拆 askama 模板)
// ===================================================================

const INDEX_HTML: &str = include_str!("../templates/index.html");
