//! keys.toml 配置加载与原子写入。
//!
//! 设计约束:key 数量是运行时变量。
//! 这里的 Vec<Key> 大小 = keys.toml 里 [[keys]] 条目数,
//! 不存在任何硬编码的 key 数量上限。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 顶层配置文件结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 剩余 credit 低于此值触发切换到下一个 key。
    #[serde(default = "default_threshold")]
    pub rotate_threshold: u32,

    /// 轮询间隔(分钟)。daemon 每隔这么久查一次 active key 的 /usage。
    /// 默认 30。允许 1-1440(1 分钟到 24 小时)。
    #[serde(default = "default_poll_interval")]
    pub poll_interval_minutes: u32,

    /// key 池,顺序即轮换顺序。可为任意长度(0 也允许,daemon 会报 pool_empty)。
    #[serde(default)]
    pub keys: Vec<Key>,
}

fn default_threshold() -> u32 {
    20
}

fn default_poll_interval() -> u32 {
    30
}

/// 单个 key 条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Key {
    /// 短标识,列表内唯一,建议 ≤30 字符。
    pub label: String,
    /// Tavily API key,格式 tvly-dev-...。
    pub secret: String,
    /// 自由备注,硬限制 100 字符(写入时校验)。
    #[serde(default)]
    pub note: String,
}

impl Config {
    /// 从 TOML 文件加载。文件不存在返回错误,调用方决定是否用空配置启动。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Read(path.to_path_buf(), e))?;
        let cfg: Config = toml::from_str(&content).map_err(ConfigError::Parse)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// 校验:label 非空且唯一,note ≤100 字符,secret 非空,poll_interval 范围。
    fn validate(&self) -> Result<(), ConfigError> {
        // poll_interval_minutes 范围校验(1-1440 分钟)
        if self.poll_interval_minutes < 1 || self.poll_interval_minutes > 1440 {
            return Err(ConfigError::Invalid(format!(
                "poll_interval_minutes 必须在 1-1440 之间(实际 {})",
                self.poll_interval_minutes
            )));
        }

        let mut seen_labels = std::collections::HashSet::new();
        for (i, k) in self.keys.iter().enumerate() {
            if k.label.trim().is_empty() {
                return Err(ConfigError::Invalid(format!("keys[{i}].label 为空")));
            }
            if !seen_labels.insert(k.label.clone()) {
                return Err(ConfigError::Invalid(format!(
                    "keys[{i}].label \"{}\" 重复",
                    k.label
                )));
            }
            if k.secret.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "keys[{i}].secret 为空"
                )));
            }
            if k.note.chars().count() > 100 {
                return Err(ConfigError::Invalid(format!(
                    "keys[{i}].note 超过 100 字符(实际 {})",
                    k.note.chars().count()
                )));
            }
        }
        Ok(())
    }

    /// 原子写:序列化 → 写临时文件 → fsync → rename → chmod 0600。
    /// 用于 Web UI 增删 key 时。daemon 后台轮换逻辑永远不调用此函数。
    pub fn save_atomic(&self, path: &Path) -> Result<(), ConfigError> {
        let content = toml::to_string_pretty(self).map_err(ConfigError::Serialize)?;

        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let tmp = dir.join(format!(
            "keys.toml.tmp.{}",
            std::process::id()
        ));

        // 1. 写临时文件
        std::fs::write(&tmp, &content).map_err(|e| ConfigError::Write(tmp.clone(), e))?;

        // 2. fsync 临时文件(确保数据落盘)— Unix 专属
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let f = std::fs::File::open(&tmp).map_err(|e| ConfigError::Write(tmp.clone(), e))?;
            unsafe {
                libc::fsync(f.as_raw_fd());
            }
        }

        // 3. rename → 目标
        //   Unix: POSIX 原子
        //   Windows: std::fs::rename 在目标存在时会失败,用 MoveFileEx + REPLACE_EXISTING
        #[cfg(unix)]
        {
            std::fs::rename(&tmp, path)
                .map_err(|e| ConfigError::Rename(tmp, path.to_path_buf(), e))?;
        }
        #[cfg(windows)]
        {
            windows_rename(&tmp, path)
                .map_err(|e| ConfigError::Rename(tmp, path.to_path_buf(), e))?;
        }

        // 4. chmod 0600(保险)— Unix 专属
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| ConfigError::Chmod(path.to_path_buf(), e))?;
        }

        Ok(())
    }
}

/// 默认配置文件路径。
pub fn default_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".config/tavily-rotator/keys.toml")
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("读取 {0} 失败: {1}")]
    Read(PathBuf, #[source] std::io::Error),
    #[error("TOML 解析失败: {0}")]
    Parse(#[source] toml::de::Error),
    #[error("配置无效: {0}")]
    Invalid(String),
    #[error("序列化失败: {0}")]
    Serialize(#[source] toml::ser::Error),
    #[error("写临时文件 {0} 失败: {1}")]
    Write(PathBuf, #[source] std::io::Error),
    #[error("rename {0} → {1} 失败: {2}")]
    Rename(PathBuf, PathBuf, #[source] std::io::Error),
    #[error("chmod {0} 失败: {1}")]
    Chmod(PathBuf, #[source] std::io::Error),
}

/// Windows 原子 rename:用 MoveFileExW + MOVEFILE_REPLACE_EXISTING + MOVEFILE_WRITE_THROUGH。
#[cfg(windows)]
fn windows_rename(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use winapi::um::winbase::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};

    fn to_wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }

    let from_w = to_wide(from);
    let to_w = to_wide(to);

    let ok = unsafe {
        MoveFileExW(
            from_w.as_ptr(),
            to_w.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
