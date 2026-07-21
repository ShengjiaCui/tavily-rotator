//! 环境变量推送机制(跨平台)。
//!
//! daemon 决定 active key 后,把它推到系统环境,让新开的 shell/进程拿到。
//! 每个平台用不同机制:
//!
//! - **macOS**: `launchctl setenv`(GUI 域 + asuser 域)。持久,daemon 崩了环境变量还在。
//! - **Linux**: 写 `~/.config/tavily-rotator/active-env.sh`,shell profile(bash/zsh)
//!   source 它。新开的 shell 拿到最新 key。已知限制:已开着的 shell 拿不到。
//! - **Windows**: 注册表 `HKCU\Environment\TAVILY_API_KEY` + 广播 `WM_SETTINGCHANGE`。
//!   新开的进程从注册表读到。

use std::path::PathBuf;

/// active-env.sh 的路径(Linux 用)。
fn active_env_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".config/tavily-rotator/active-env.sh")
}

// ===================================================================
// 公共接口(调用方用这三个,签名跨平台一致)
// ===================================================================

/// 把 key 推到系统环境变量 TAVILY_API_KEY。
pub fn push_env(key: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        push_env_macos(key)
    }
    #[cfg(target_os = "linux")]
    {
        push_env_linux(key)
    }
    #[cfg(windows)]
    {
        push_env_windows(key)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = key;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "不支持的平台(仅 macOS/Linux/Windows)",
        ))
    }
}

/// 清除环境变量(全部 key 耗尽时调用)。
pub fn unset_env() -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        unset_env_macos()
    }
    #[cfg(target_os = "linux")]
    {
        unset_env_linux()
    }
    #[cfg(windows)]
    {
        unset_env_windows()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "不支持的平台",
        ))
    }
}

/// 读取当前系统环境里的 TAVILY_API_KEY(自检/对比用)。
pub fn read_env() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        read_env_macos()
    }
    #[cfg(target_os = "linux")]
    {
        read_env_linux()
    }
    #[cfg(windows)]
    {
        read_env_windows()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        None
    }
}

// ===================================================================
// macOS: launchctl setenv
// ===================================================================

#[cfg(target_os = "macos")]
fn push_env_macos(key: &str) -> std::io::Result<()> {
    use std::process::Command;

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

    // 2. 推到当前用户会话域
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
        tracing::warn!("launchctl asuser setenv 失败 exit={}, GUI 域已设置,继续", s2);
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn unset_env_macos() -> std::io::Result<()> {
    use std::process::Command;

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

#[cfg(target_os = "macos")]
fn read_env_macos() -> Option<String> {
    use std::process::Command;

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

// ===================================================================
// Linux: 写 active-env.sh + shell profile source
// ===================================================================

#[cfg(target_os = "linux")]
fn push_env_linux(key: &str) -> std::io::Result<()> {
    use std::io::Write;

    let path = active_env_path();

    // 1. 原子写 active-env.sh
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("sh.tmp");
    let content = format!("# 由 tavily-rotator 自动生成,勿手改\nexport TAVILY_API_KEY={:?}\n", key);
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(content.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, &path)?;

    // 2. 确保 shell profile 有 source 这行(幂等)
    ensure_shell_profile_sources()?;

    tracing::info!("Linux: active-env.sh 已更新 → {}", path.display());
    Ok(())
}

#[cfg(target_os = "linux")]
fn unset_env_linux() -> std::io::Result<()> {
    let path = active_env_path();
    // 写一个注释掉的文件(让 source 它不报错)
    let content = "# 所有 key 已耗尽\n# export TAVILY_API_KEY=\n";
    std::fs::write(&path, content)?;
    tracing::warn!("Linux: active-env.sh 已清空(所有 key 耗尽)");
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_env_linux() -> Option<String> {
    let path = active_env_path();
    let content = std::fs::read_to_string(&path).ok()?;
    // 解析 export TAVILY_API_KEY="..."
    for line in content.lines() {
        if line.starts_with("export TAVILY_API_KEY=") {
            let val = line.trim_start_matches("export TAVILY_API_KEY=");
            let val = val.trim_matches('"');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Linux: 确保 .bashrc/.zshrc 有 source active-env.sh 的行。
/// 幂等:已有就不重复加。
#[cfg(target_os = "linux")]
fn ensure_shell_profile_sources() -> std::io::Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let source_line = format!(
        "[ -f {}/.config/tavily-rotator/active-env.sh ] && source {}/.config/tavily-rotator/active-env.sh  # tavily-rotator",
        home, home
    );
    let marker = "tavily-rotator";

    // 检查常见的 shell profile 文件
    let profiles = [".bashrc", ".zshrc"];
    for p in &profiles {
        let path = std::path::Path::new(&home).join(p);
        if !path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&path)?;
        if content.contains(marker) {
            continue; // 已有,跳过
        }
        // 追加
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)?;
        use std::io::Write;
        writeln!(f, "\n{}", source_line)?;
        tracing::info!("Linux: {} 已加 source active-env.sh", p);
    }
    Ok(())
}

// ===================================================================
// Windows: 注册表 HKCU\Environment + 广播 WM_SETTINGCHANGE
// ===================================================================

#[cfg(windows)]
fn push_env_windows(key: &str) -> std::io::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let env = hkcu.open_subkey_with_flags("Environment", KEY_SET_VALUE)?;
    env.set_value("TAVILY_API_KEY", &key)?;

    // 广播 WM_SETTINGCHANGE 让 explorer/新进程读到新值
    broadcast_setting_change();

    tracing::info!("Windows: 注册表 HKCU\\Environment\\TAVILY_API_KEY 已设置");
    Ok(())
}

#[cfg(windows)]
fn unset_env_windows() -> std::io::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let env = hkcu.open_subkey_with_flags("Environment", KEY_SET_VALUE)?;
    let _ = env.delete_value("TAVILY_API_KEY");
    broadcast_setting_change();

    tracing::warn!("Windows: 注册表 TAVILY_API_KEY 已删除");
    Ok(())
}

#[cfg(windows)]
fn read_env_windows() -> Option<String> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let env = hkcu.open_subkey("Environment").ok()?;
    let val: String = env.get_value("TAVILY_API_KEY").ok()?;
    if val.is_empty() {
        None
    } else {
        Some(val)
    }
}

/// Windows: 广播 WM_SETTINGCHANGE,让 explorer 更新环境块。
#[cfg(windows)]
fn broadcast_setting_change() {
    // 用 PowerShell 广播(简单,不引 winapi crate 的 SendMessage)
    let ps_script = r#"
Add-Type -TypeDefinition '
using System;
using System.Runtime.InteropServices;
public class Win32 {
    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
    public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
    public const uint HWND_BROADCAST = 0xFFFF;
    public const uint WM_SETTINGCHANGE = 0x001A;
}' -ErrorAction SilentlyContinue
$ propagated = [uintptr]::Zero
[Win32]::SendMessageTimeout([IntPtr][Win32]::HWND_BROADCAST, [Win32]::WM_SETTINGCHANGE, [uintptr]::Zero, 'Environment', 2, 5000, [ref]$propagated) | Out-Null
"#;
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", ps_script])
        .spawn();
}
