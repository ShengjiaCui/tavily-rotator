//! tavily-rotator daemon — Tavily API key 轮换守护进程。
//!
//! 在多个 Tavily 免费 API key 之间顺序轮换,通过 launchctl setenv 推送
//! 当前 active key 到系统环境。配 Web 面板(127.0.0.1:8731)管 key 和看用量。
//!
//! Phase 1 范围:读 keys.toml → 启动时 launchctl setenv 推 active key
//!              → 启动最小 HTTP 服务(/api/active, /api/state, /health)。
//! Phase 2 加:轮换逻辑 / /usage 查询 / SQLite。
//! Phase 3 加:Web 面板 / CRUD / install。

mod api;
mod config;
mod db;
mod env_push;
mod install;
mod rotator;
mod tavily;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicUsize};
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

    /// 环境变量推送时间。
    pub env_pushed_at: RwLock<Option<String>>,

    /// HTTP 客户端(复用连接池)。
    pub http: reqwest::Client,

    /// SQLite 数据库。
    pub db: db::Db,

    /// 上次月初重置检查的月份(year*12+month),用于判断跨月。
    pub last_reset_check: AtomicI64,
}

impl AppState {
    /// 距下月 1 号(UTC)还有几天。Phase 3 精确实现,先返回占位。
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
    let db_path = db::default_db_path();
    tracing::info!("配置文件: {}", config_path.display());
    tracing::info!("SQLite: {}", db_path.display());

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
            tracing::error!("请创建 {} (格式见 README)", config_path.display());
            std::process::exit(1);
        }
    };

    if cfg.keys.is_empty() {
        tracing::warn!("key 池为空!daemon 会启动但 /api/active 返回 503");
    }

    // 打开 SQLite
    let database = db::Db::open(&db_path)?;

    // 从 SQLite 读 active_pointer(Phase 2:持久化)
    // None = 首次启动,用 keys[0]
    let active_idx = match database.get_active_pointer().await? {
        Some((idx, _, _)) if idx < cfg.keys.len() => idx,
        _ => 0,
    };
    tracing::info!("active 指针(SQLite)= key[{active_idx}]");

    // HTTP 客户端(复用连接池)
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let state = Arc::new(AppState {
        config: RwLock::new(cfg.clone()),
        active_idx: AtomicUsize::new(if cfg.keys.is_empty() { usize::MAX } else { active_idx }),
        last_remaining: AtomicU32::new(0),
        env_pushed: AtomicBool::new(false),
        env_pushed_at: RwLock::new(None),
        http,
        db: database,
        last_reset_check: AtomicI64::new(0),
    });

    // 启动时立即推送 active key 到环境变量(§8.5 启动时序)
    if active_idx < cfg.keys.len() {
        let key = &cfg.keys[active_idx].secret;
        match env_push::push_env(key) {
            Ok(()) => {
                state
                    .env_pushed
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                let ts = now_unix();
                *state.env_pushed_at.write().await = Some(format!("@{ts}"));
                tracing::info!(
                    "启动推送 TAVILY_API_KEY = keys[{active_idx}] \"{}\"",
                    cfg.keys[active_idx].label
                );
            }
            Err(e) => {
                tracing::error!("启动推送环境变量失败: {e}");
            }
        }
    }

    // 启动轮换循环(后台 task)
    let rotator_state = state.clone();
    tokio::spawn(async move {
        rotator::run(rotator_state).await;
    });

    // 启动 HTTP 服务
    let addr = SocketAddr::from(([127, 0, 0, 1], 8731));
    tracing::info!("HTTP 服务监听 http://{addr}");

    let app = api::router(state.clone());
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
