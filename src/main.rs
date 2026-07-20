//! tavily-rotator daemon — Tavily API key 轮换守护进程(ADR-0018)。
//!
//! 设计文档: opdev/docs/designs/2026-07-20-tavily-key-rotator.md
//!
//! Phase 1 范围:读 keys.toml → 启动时 launchctl setenv 推 active key
//!              → 启动最小 HTTP 服务(/api/active, /api/state, /health)。
//! Phase 2 加:轮换逻辑 / /usage 查询 / SQLite。
//! Phase 3 加:Web 面板 / CRUD / install。

mod api;
mod config;
mod env_push;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize};
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

/// 全局共享状态。所有模块通过 Arc<AppState> 访问。
pub struct AppState {
    /// keys.toml 配置(只读快照,reload 时整体替换)。
    pub config: RwLock<config::Config>,

    /// 当前 active key 在 keys 数组里的索引。
    /// usize::MAX 表示"全部耗尽"。
    pub active_idx: AtomicUsize,

    /// 最近一次 /usage 查到的剩余 credit(0 表示还没查过)。
    pub last_remaining: AtomicU32,

    /// 环境变量是否已推送。
    pub env_pushed: AtomicBool,

    /// 环境变量推送时间(ISO 8601)。
    pub env_pushed_at: RwLock<Option<String>>,
}

impl AppState {
    /// 计算距下月 1 号(UTC)还有几天。Phase 2 精确实现。
    pub fn days_to_reset(&self) -> Option<u32> {
        None
    }

    pub fn next_reset_iso(&self) -> Option<String> {
        None
    }
}

/// 获取当前 unix 时间戳。
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config_path = config::default_config_path();
    tracing::info!("配置文件路径: {}", config_path.display());

    // 加载配置
    let cfg = match config::Config::load(&config_path) {
        Ok(c) => {
            tracing::info!(
                "加载 {} 个 key, rotate_threshold={}",
                c.keys.len(),
                c.rotate_threshold
            );
            c
        }
        Err(e) => {
            tracing::error!("加载配置失败: {e}");
            tracing::error!("请创建 {} (示例见 ADR-0018)", config_path.display());
            std::process::exit(1);
        }
    };

    if cfg.keys.is_empty() {
        tracing::warn!("key 池为空!daemon 会启动但 /api/active 返回 503");
    }

    // 初始化 active key(Phase 1:固定用 keys[0];Phase 2 从 SQLite 读 active_pointer)
    let active_idx = if cfg.keys.is_empty() {
        usize::MAX
    } else {
        0
    };

    let state = Arc::new(AppState {
        config: RwLock::new(cfg.clone()),
        active_idx: AtomicUsize::new(active_idx),
        last_remaining: AtomicU32::new(0),
        env_pushed: AtomicBool::new(false),
        env_pushed_at: RwLock::new(None),
    });

    // 启动时立即推送 active key 到环境变量(§8.5 启动时序)
    if active_idx != usize::MAX {
        let key = &cfg.keys[active_idx].secret;
        match env_push::push_env(key) {
            Ok(()) => {
                state
                    .env_pushed
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                let ts = now_unix();
                *state.env_pushed_at.write().await = Some(format!("@{ts}"));
                tracing::info!(
                    "启动推送 TAVILY_API_KEY = keys[{}] \"{}\" ({} bytes)",
                    active_idx,
                    cfg.keys[active_idx].label,
                    key.len()
                );
            }
            Err(e) => {
                tracing::error!("启动推送环境变量失败: {e}");
                tracing::error!("新开的终端可能拿不到 TAVILY_API_KEY");
            }
        }
    }

    // 启动 HTTP 服务
    let addr = SocketAddr::from(([127, 0, 0, 1], 8731));
    tracing::info!("HTTP 服务监听 http://{addr}");

    let app = api::router(state.clone());
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
