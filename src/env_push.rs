//! launchctl setenv 推送机制(ADR-0018 §8)。
//!
//! daemon 决定 active key 后,通过 launchctl setenv 把它推到系统环境。
//! 推到两个域:GUI 域(新开 Terminal 拿到)和当前用户会话域。
//!
//! 关键性质:launchctl setenv 是持久的。daemon 推一次后,即使 daemon 崩了,
//! 环境变量也保留在 launchctl 里,新 shell 照样能拿到(§8.4 优雅降级)。

use std::process::Command;

/// 把 key 推到系统环境变量 TAVILY_API_KEY。
pub fn push_env(key: &str) -> std::io::Result<()> {
    let uid = unsafe { libc::getuid() };
    let uid_str = uid.to_string();

    // 1. 推到 GUI 域(新开的 Terminal/iTerm 拿到)
    let s1 = Command::new("launchctl")
        .args(["setenv", "TAVILY_API_KEY", key])
        .status()?;
    if !s1.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("launchctl setenv (GUI 域) 失败, exit={}", s1),
        ));
    }

    // 2. 推到当前用户会话域(兼容某些只读这个域的进程)
    let s2 = Command::new("launchctl")
        .args([
            "asuser",
            &uid_str,
            "launchctl",
            "setenv",
            "TAVILY_API_KEY",
            key,
        ])
        .status()?;
    if !s2.success() {
        // asuser 失败不致命,GUI 域已设。只记日志。
        tracing::warn!(
            "launchctl asuser setenv 失败 exit={}, GUI 域已设置,继续",
            s2
        );
    }

    Ok(())
}

/// 清除环境变量(全部 key 耗尽时调用)。
pub fn unset_env() -> std::io::Result<()> {
    let uid = unsafe { libc::getuid() };
    let uid_str = uid.to_string();

    let _ = Command::new("launchctl")
        .args(["unsetenv", "TAVILY_API_KEY"])
        .status();
    let _ = Command::new("launchctl")
        .args(["asuser", &uid_str, "launchctl", "unsetenv", "TAVILY_API_KEY"])
        .status();

    Ok(())
}

/// 读取当前 launchctl 环境里的 TAVILY_API_KEY(用于自检/对比)。
pub fn read_env() -> Option<String> {
    let out = Command::new("launchctl")
        .args(["getenv", "TAVILY_API_KEY"])
        .output()
        .ok()?;
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
