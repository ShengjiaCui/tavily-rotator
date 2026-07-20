//! 环境探测 + 一键安装(ADR-0018 §7)。
//!
//! 安全模型:
//! - 命令硬编码在代码里,UI 不接受任意命令
//! - 绝不执行 `tvly login`(会覆盖环境变量注入,轮换失效)
//! - 安装日志实时回显(Phase 3 后续接 SSE)
//! - 落 install_events 表审计

use std::process::Command;

use serde::Serialize;

/// 环境探测结果。GET /api/environment 返回。
#[derive(Debug, Serialize)]
pub struct Environment {
    pub tvly_cli: ComponentStatus,
    pub tavily_search_skill: ComponentStatus,
    pub tavily_research_skill: ComponentStatus,
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
    Environment {
        tvly_cli: detect_tvly(),
        tavily_search_skill: detect_skill("tavily-search"),
        tavily_research_skill: detect_skill("tavily-research"),
        pool_size: 0, // 由 API handler 填
        days_to_reset: None,
    }
}

fn detect_tvly() -> ComponentStatus {
    // 找 tvly 二进制
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
        .and_then(|v| v.get("source").and_then(|s| s.as_str()).map(String::from));

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
            // 找 SKILL.md 确认是真 skill 不是空目录
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

fn which(cmd: &str) -> Option<String> {
    let out = Command::new("/usr/bin/which").arg(cmd).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 安装命令定义(硬编码,绝不 tvly login)。
/// (component_name, command_vec)
/// 返回 Vec 而不是固定数量,方便以后加组件。
pub fn install_commands(components: &[String]) -> Vec<(String, Vec<String>)> {
    let mut cmds = Vec::new();

    for c in components {
        match c.as_str() {
            "tvly-cli" => {
                // 注意:用 sh -c 因为是 curl|bash 管道
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
            "tavily-search-skill" => {
                cmds.push((
                    "tavily-search-skill".into(),
                    vec![
                        "npx".into(),
                        "skills".into(),
                        "add".into(),
                        "tavily-ai/skills@tavily-search".into(),
                        "-g".into(),
                        "-y".into(),
                    ],
                ));
            }
            "tavily-research-skill" => {
                cmds.push((
                    "tavily-research-skill".into(),
                    vec![
                        "npx".into(),
                        "skills".into(),
                        "add".into(),
                        "tavily-ai/skills@tavily-research".into(),
                        "-g".into(),
                        "-y".into(),
                    ],
                ));
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
