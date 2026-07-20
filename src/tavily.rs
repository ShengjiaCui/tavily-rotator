//! Tavily /usage 端点查询(ADR-0018 §2 已验证免费)。
//!
//! 实测响应结构(2026-07-20):
//! {
//!   "key": { "usage": 4, "limit": null, "search_usage": 4, ... },
//!   "account": {
//!     "current_plan": "Researcher",
//!     "plan_usage": 4,
//!     "plan_limit": 1000,
//!     "search_usage": 4, "crawl_usage": 0, "extract_usage": 0,
//!     "map_usage": 0, "research_usage": 0,
//!     "paygo_usage": 0, "paygo_limit": null
//!   }
//! }

use serde::Deserialize;

const USAGE_URL: &str = "https://api.tavily.com/usage";

#[derive(Debug, Deserialize)]
struct UsageResponse {
    account: AccountUsage,
}

#[derive(Debug, Deserialize)]
struct AccountUsage {
    plan_usage: u32,
    plan_limit: u32,
    search_usage: u32,
    crawl_usage: u32,
    extract_usage: u32,
    map_usage: u32,
    research_usage: u32,
}

/// 查询单个 key 的 /usage。
///
/// 成功返回 (plan_usage, plan_limit, search, crawl, extract, map, research)。
/// 失败(key 无效/网络错误)返回 Err。
pub async fn query_usage(
    client: &reqwest::Client,
    secret: &str,
) -> anyhow::Result<(u32, u32, u32, u32, u32, u32, u32)> {
    let resp = client
        .get(USAGE_URL)
        .bearer_auth(secret)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Tavily /usage 返回 {status}: {body}");
    }

    let parsed: UsageResponse = resp.json().await?;
    let a = parsed.account;
    Ok((
        a.plan_usage,
        a.plan_limit,
        a.search_usage,
        a.crawl_usage,
        a.extract_usage,
        a.map_usage,
        a.research_usage,
    ))
}

/// 查询并返回带 label 的快照(方便直接存 SQLite)。
pub async fn query_usage_snapshot(
    client: &reqwest::Client,
    secret: &str,
    key_idx: usize,
    key_label: &str,
    ts: u64,
) -> anyhow::Result<crate::db::UsageSnapshot> {
    let (plan_usage, plan_limit, search, crawl, extract, map, research) =
        query_usage(client, secret).await?;

    Ok(crate::db::UsageSnapshot {
        key_idx,
        key_label: key_label.to_string(),
        ts,
        plan_usage,
        plan_limit,
        search,
        crawl,
        extract,
        map,
        research,
    })
}
