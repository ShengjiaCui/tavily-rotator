//! SQLite 持久化层。
//!
//! 三张表:
//! - usage_snapshots:每次 /usage 查询结果(画用量曲线用)
//! - rotations:每次轮换记录(切换历史,审计资产,永久保留)
//! - active_pointer:当前 active key 索引(单行表)
//!
//! 保留策略:usage_snapshots 90 天 / rotations 永久。
//! 清理由 Phase 4 的定时任务做,这里只提供清理函数。

use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

/// 数据库默认路径。
pub fn default_db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/tavily-rotator/state.db")
}

/// /usage 查询结果(对应 Tavily /usage 响应的 account 部分)。
#[derive(Debug, Clone)]
pub struct UsageSnapshot {
    pub key_idx: usize,
    pub key_label: String,
    pub ts: u64,
    pub plan_usage: u32,
    pub plan_limit: u32,
    pub search: u32,
    pub crawl: u32,
    pub extract: u32,
    pub map: u32,
    pub research: u32,
}

/// 轮换记录。
#[derive(Debug, Clone)]
pub struct Rotation {
    pub ts: u64,
    pub from_idx: Option<usize>,
    pub from_label: Option<String>,
    pub to_idx: usize,
    pub to_label: String,
    pub reason: String,
}

/// SQLite 连接包装。用 Mutex 串行化访问(写少读多,Mutex 够用)。
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// 打开/创建数据库,初始化表结构。
    pub fn open(path: &PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;

        // 性能优化:WAL 模式 + 合理的 busy timeout
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = NORMAL;",
        )?;

        // 建表(幂等)
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS usage_snapshots (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                key_idx     INTEGER NOT NULL,
                key_label   TEXT NOT NULL,
                ts          INTEGER NOT NULL,
                plan_usage  INTEGER NOT NULL,
                plan_limit  INTEGER NOT NULL,
                search      INTEGER DEFAULT 0,
                crawl       INTEGER DEFAULT 0,
                extract     INTEGER DEFAULT 0,
                map         INTEGER DEFAULT 0,
                research    INTEGER DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_usage_key_ts
                ON usage_snapshots(key_idx, ts DESC);

            CREATE TABLE IF NOT EXISTS rotations (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                ts          INTEGER NOT NULL,
                from_idx    INTEGER,
                from_label  TEXT,
                to_idx      INTEGER NOT NULL,
                to_label    TEXT NOT NULL,
                reason      TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS active_pointer (
                id          INTEGER PRIMARY KEY CHECK (id = 1),
                key_idx     INTEGER NOT NULL,
                since_ts    INTEGER NOT NULL,
                updated_ts  INTEGER NOT NULL
            );
            INSERT OR IGNORE INTO active_pointer (id, key_idx, since_ts, updated_ts)
            VALUES (1, 0, 0, 0);

            CREATE TABLE IF NOT EXISTS install_events (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                ts          INTEGER NOT NULL,
                component   TEXT NOT NULL,
                success     INTEGER NOT NULL,
                log_excerpt TEXT
            );",
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 写一条 /usage 快照。
    pub async fn insert_usage_snapshot(&self, s: &UsageSnapshot) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO usage_snapshots
             (key_idx, key_label, ts, plan_usage, plan_limit, search, crawl, extract, map, research)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                s.key_idx as i64,
                s.key_label,
                s.ts as i64,
                s.plan_usage as i64,
                s.plan_limit as i64,
                s.search as i64,
                s.crawl as i64,
                s.extract as i64,
                s.map as i64,
                s.research as i64,
            ],
        )?;
        Ok(())
    }

    /// 写一条轮换记录。
    pub async fn insert_rotation(&self, r: &Rotation) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO rotations (ts, from_idx, from_label, to_idx, to_label, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                r.ts as i64,
                r.from_idx.map(|i| i as i64),
                r.from_label,
                r.to_idx as i64,
                r.to_label,
                r.reason,
            ],
        )?;
        Ok(())
    }

    /// 读当前 active 指针。None 表示从未设置(首次启动)。
    pub async fn get_active_pointer(&self) -> anyhow::Result<Option<(usize, u64, u64)>> {
        let conn = self.conn.lock().await;
        let mut stmt =
            conn.prepare("SELECT key_idx, since_ts, updated_ts FROM active_pointer WHERE id=1")?;
        let row = stmt
            .query_row([], |r| {
                Ok((
                    r.get::<_, i64>(0)? as usize,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, i64>(2)? as u64,
                ))
            })
            .ok();
        Ok(row)
    }

    /// 更新 active 指针。
    pub async fn set_active_pointer(&self, key_idx: usize, now_ts: u64) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE active_pointer SET key_idx = ?1, since_ts = ?2, updated_ts = ?3 WHERE id = 1",
            params![key_idx as i64, now_ts as i64, now_ts as i64],
        )?;
        Ok(())
    }

    /// 查最近 N 条轮换记录(切换历史)。
    pub async fn recent_rotations(&self, limit: u32) -> anyhow::Result<Vec<Rotation>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT ts, from_idx, from_label, to_idx, to_label, reason
             FROM rotations ORDER BY ts DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(Rotation {
                ts: r.get::<_, i64>(0)? as u64,
                from_idx: r.get::<_, Option<i64>>(1)?.map(|i| i as usize),
                from_label: r.get::<_, Option<String>>(2)?,
                to_idx: r.get::<_, i64>(3)? as usize,
                to_label: r.get::<_, String>(4)?,
                reason: r.get::<_, String>(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 清理 90 天前的 usage_snapshots 和 install_events(rotations 永久保留)。
    pub async fn cleanup_old(&self, now_ts: u64) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        let cutoff = (now_ts as i64) - (90 * 86400);
        conn.execute("DELETE FROM usage_snapshots WHERE ts < ?1", params![cutoff])?;
        conn.execute("DELETE FROM install_events WHERE ts < ?1", params![cutoff])?;
        Ok(())
    }

    /// 写一条安装事件记录。
    pub async fn insert_install_event(
        &self,
        now: u64,
        component: &str,
        success: bool,
        log_excerpt: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO install_events (ts, component, success, log_excerpt)
             VALUES (?1, ?2, ?3, ?4)",
            params![now as i64, component, if success { 1 } else { 0 }, log_excerpt],
        )?;
        Ok(())
    }
}
