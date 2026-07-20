//! HTTP API。Phase 1 只实现 /api/active 和 /api/state 的最小版本。
//! Phase 3 会加 CRUD / refresh-all / install 等。

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Serialize;

use crate::AppState;

pub fn router(state: std::sync::Arc<AppState>) -> Router {
    Router::new()
        .route("/api/active", get(api_active))
        .route("/api/state", get(api_state))
        .route("/health", get(health))
        .with_state(state)
}

#[derive(Serialize)]
struct ActiveResponse {
    label: String,
    idx: usize,
    remaining: Option<u32>,
    env_pushed: bool,
    env_pushed_at: Option<String>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
    reset_at: Option<String>,
    days_to_reset: Option<u32>,
}

async fn api_active(State(state): State<std::sync::Arc<AppState>>) -> impl IntoResponse {
    let cfg = state.config.read().await;

    if cfg.keys.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "pool_empty".into(),
                reset_at: None,
                days_to_reset: None,
            }),
        )
            .into_response();
    }

    let idx = state.active_idx.load(std::sync::atomic::Ordering::Relaxed);
    if idx >= cfg.keys.len() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "all_keys_exhausted".into(),
                reset_at: state.next_reset_iso(),
                days_to_reset: state.days_to_reset(),
            }),
        )
            .into_response();
    }

    let key = &cfg.keys[idx];
    let remaining_val = state.last_remaining.load(std::sync::atomic::Ordering::Relaxed);
    Json(ActiveResponse {
        label: key.label.clone(),
        idx,
        // Phase 1 还没查 /usage,0 表示未知;Phase 2 改成 Option<u32>
        remaining: if remaining_val == 0 { None } else { Some(remaining_val) },
        env_pushed: state.env_pushed.load(std::sync::atomic::Ordering::Relaxed),
        env_pushed_at: state.env_pushed_at.read().await.clone(),
    })
    .into_response()
}

#[derive(Serialize)]
struct StateResponse {
    active_label: Option<String>,
    active_idx: usize,
    pool_size: usize,
    rotate_threshold: u32,
    keys: Vec<KeyView>,
}

#[derive(Serialize)]
struct KeyView {
    idx: usize,
    label: String,
    note: String,
}

async fn api_state(State(state): State<std::sync::Arc<AppState>>) -> Json<StateResponse> {
    let cfg = state.config.read().await;
    let active_idx = state.active_idx.load(std::sync::atomic::Ordering::Relaxed);

    Json(StateResponse {
        active_label: cfg.keys.get(active_idx).map(|k| k.label.clone()),
        active_idx,
        pool_size: cfg.keys.len(),
        rotate_threshold: cfg.rotate_threshold,
        keys: cfg
            .keys
            .iter()
            .enumerate()
            .map(|(i, k)| KeyView {
                idx: i,
                label: k.label.clone(),
                note: k.note.clone(),
            })
            .collect(),
    })
}

async fn health() -> &'static str {
    "ok\n"
}
