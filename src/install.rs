//! 环境探测 + 一键安装。
//!
//! 安全模型:
//! - 命令硬编码在代码里,UI 不接受任意命令
//! - 绝不执行 `tvly login`(会覆盖环境变量注入,轮换失效)
//! - 安装日志实时回显(Phase 3 后续接 SSE)
//! - 落 install_events 表审计
//!
//! Tavily 官方推荐(2026-07):
//!   Step 1: curl -fsSL https://cli.tavily.com/install.sh | bash
//!   Step 2: npx skills add tavily-ai/skills --all
//! `--all` 一次装全套 8 个 skill,比逐个装更简洁更全。

use std::process::Command;

use serde::Serialize;

/// `--all` 装的 8 个 skill 列表(用于探测)。
/// 顺序按 tavily-ai/skills 仓库的约定。
/// 如果 Tavily 以后加新 skill,这里追加即可。
const ALL_SKILLS: &[&str] = &[
    "tavily-best-practices",
    "tavily-cli",
    "tavily-crawl",
    "tavily-dynamic-search",
    "tavily-extract",
    "tavily-map",
    "tavily-research",
    "tavily-search",
];

/// 环境探测结果。GET /api/environment 返回。
#[derive(Debug, Serialize)]
pub struct Environment {
    pub tvly_cli: ComponentStatus,
    /// 8 个 skill 的探测结果(skill 名 → status)。
    pub skills: Vec<(String, ComponentStatus)>,
    /// 已装 skill 数 / 总数,方便 UI 显示 "6/8"。
    pub skills_installed: usize,
    pub skills_total: usize,
    pub pool_size: usize,
    pub days_to_reset: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ComponentStatus {
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub auth_source: Option<String>, // tvly 专用,必须保持 null
}

/// 探测本机环境。
pub async fn detect() -> Environment {
    let mut skills = Vec::with_capacity(ALL_SKILLS.len());
    let mut installed = 0;
    for name in ALL_SKILLS {
        let s = detect_skill(name);
        if s.installed {
            installed += 1;
        }
        skills.push((name.to_string(), s));
    }

    Environment {
        tvly_cli: detect_tvly(),
        skills,
        skills_installed: installed,
        skills_total: ALL_SKILLS.len(),
        pool_size: 0, // 由 API handler 填
        days_to_reset: None,
    }
}

fn detect_tvly() -> ComponentStatus {
    let path = which("tvly");
    if path.is_none() {
        return ComponentStatus {
            installed: false,
            path: None,
            version: None,
            auth_source: None,
        };
    }

    let path_str = path.clone().unwrap();
    let version = Command::new(&path_str)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());

    // 查 auth source(关键:必须保持 null,login 了会变)
    let auth_source = Command::new(&path_str)
        .args(["auth", "--json"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("source")
                .and_then(|s| s.as_str())
                .map(String::from)
        });

    ComponentStatus {
        installed: true,
        path: Some(path_str),
        version,
        auth_source, // None = 未 login(正确状态)
    }
}

fn detect_skill(name: &str) -> ComponentStatus {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!("{home}/.agents/skills/{name}"),
        format!("{home}/.zcode/skills/{name}"),
        format!("{home}/.claude/skills/{name}"),
    ];

    for c in &candidates {
        let p = std::path::Path::new(c);
        if p.exists() && p.is_dir() {
            if p.join("SKILL.md").exists() {
                return ComponentStatus {
                    installed: true,
                    path: Some(c.clone()),
                    version: None,
                    auth_source: None,
                };
            }
        }
    }

    ComponentStatus {
        installed: false,
        path: None,
        version: None,
        auth_source: None,
    }
}

/// 查找命令位置。先查 PATH,再查常见安装位置(launchd 环境的 PATH 很短,
/// 不含 ~/.local/bin / ~/.cargo/bin,需要显式补查)。
fn which(cmd: &str) -> Option<String> {
    // 1. 标准 PATH 查找
    if let Some(p) = which_in_path(cmd) {
        return Some(p);
    }

    // 2. launchd 环境常见的"用户级"安装位置
    let home = std::env::var("HOME").unwrap_or_default();
    let fallbacks = [
        format!("{home}/.local/bin/{cmd}"),
        format!("{home}/.cargo/bin/{cmd}"),
        format!("/opt/homebrew/bin/{cmd}"),
        format!("/usr/local/bin/{cmd}"),
    ];
    for f in &fallbacks {
        let p = std::path::Path::new(f);
        if p.exists() {
            return Some(f.clone());
        }
    }

    None
}

fn which_in_path(cmd: &str) -> Option<String> {
    let out = Command::new("/usr/bin/which").arg(cmd).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed.contains("no ") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 安装命令定义(硬编码,绝不 tvly login)。
/// (component_name, command_vec)
///
/// 官方推荐两条命令(2026-07):
///   1. curl -fsSL https://cli.tavily.com/install.sh | bash
///   2. npx skills add tavily-ai/skills --all   ← --all 一次装全套
pub fn install_commands(components: &[String]) -> Vec<(String, Vec<String>)> {
    let mut cmds = Vec::new();

    for c in components {
        match c.as_str() {
            "tvly-cli" => {
                cmds.push((
                    "tvly-cli".into(),
                    vec![
                        "sh".into(),
                        "-c".into(),
                        "curl -fsSL https://cli.tavily.com/install.sh | bash".into(),
                    ],
                ));
                // ⚠ 绝不执行 tvly login(会覆盖环境变量注入)
            }
            "tavily-skills-all" => {
                // 官方推荐:--all 一次装全套 8 个 skill
                cmds.push((
                    "tavily-skills-all".into(),
                    vec![
                        "npx".into(),
                        "skills".into(),
                        "add".into(),
                        "tavily-ai/skills".into(),
                        "--all".into(),
                        "-g".into(),
                        "-y".into(),
                    ],
                ));
            }
            // 兼容旧组件名(老 UI 可能还发这两个)
            "tavily-search-skill" | "tavily-research-skill" => {
                // 重定向到 --all(更全,幂等)
                if !cmds.iter().any(|(n, _)| n == "tavily-skills-all") {
                    cmds.push((
                        "tavily-skills-all".into(),
                        vec![
                            "npx".into(),
                            "skills".into(),
                            "add".into(),
                            "tavily-ai/skills".into(),
                            "--all".into(),
                            "-g".into(),
                            "-y".into(),
                        ],
                    ));
                }
            }
            _ => {} // 未知组件,忽略(命令白名单)
        }
    }

    cmds
}

/// 执行单条安装命令,返回 (success, output_log)。
pub fn run_install_command(cmd: &[String]) -> (bool, String) {
    let prog = &cmd[0];
    let args = &cmd[1..];

    let output = Command::new(prog).args(args).output();

    match output {
        Ok(o) => {
            let mut log = String::new();
            if !o.stdout.is_empty() {
                log.push_str(&String::from_utf8_lossy(&o.stdout));
            }
            if !o.stderr.is_empty() {
                if !log.is_empty() {
                    log.push('\n');
                }
                log.push_str(&String::from_utf8_lossy(&o.stderr));
            }
            (o.status.success(), log)
        }
        Err(e) => (false, format!("启动命令失败: {e}")),
    }
}
