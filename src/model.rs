use serde::{Deserialize, Serialize};
use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Error {
    pub code: Option<String>,
    pub message: String,
}
impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
        }
    }
    pub fn cancelled() -> Self {
        Self::new("任务已取消；下载临时文件已保留")
    }
    pub fn context(mut self, stage: &str) -> Self {
        self.message = format!("{stage}：{}", self.message);
        self
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(format!("本地文件操作失败：{e}"))
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::new("服务响应或配置格式无效")
    }
}
impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        // reqwest errors carry the full URL, including access_token. Never display them.
        Self::new(if e.is_timeout() {
            "网络请求超时，请重试"
        } else if e.is_connect() {
            "无法连接服务器，请检查网络或系统代理"
        } else {
            "网络请求失败或连接中断，请重试"
        })
    }
}

#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Credentials {
    pub app_key: String,
    pub secret_key: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub cookies: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RemoteFile {
    pub fs_id: u64,
    pub path: String,
    #[serde(alias = "filename")]
    pub server_filename: String,
    pub size: u64,
    pub isdir: u8,
    pub server_mtime: u64,
    pub md5: String,
    pub dlink: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}
fn default_interval() -> u64 {
    5
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn valid_filename(name: &str) -> bool {
    if name.is_empty()
        || name.ends_with(['.', ' '])
        || name.encode_utf16().count() > 255
        || name.chars().any(|c| c < ' ' || "<>:\"/\\|?*".contains(c))
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_uppercase();
    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str()) {
        return false;
    }
    !["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|n| {
            ["1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"].contains(&n)
        })
    })
}

pub fn remote_directory(path: &str) -> Result<String> {
    let path = path.trim();
    if !path.starts_with('/')
        || path.contains(['\\', '\0'])
        || path.split('/').any(|p| p == "." || p == "..")
    {
        return Err(Error::new(
            "网盘路径必须以 / 开头，且不能包含 . 或 .. 路径段",
        ));
    }
    let parts: Vec<_> = path.split('/').filter(|p| !p.is_empty()).collect();
    Ok(format!("/{}", parts.join("/")))
}
pub fn remote_child(directory: &str, name: &str) -> Result<String> {
    if !valid_filename(name) {
        return Err(Error::new(
            "文件名不能在 Windows 中安全保存，请先在网盘中改名",
        ));
    }
    Ok(format!(
        "{}/{}",
        remote_directory(directory)?.trim_end_matches('/'),
        name
    ))
}
pub fn parent(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map(|(p, _)| if p.is_empty() { "/" } else { p })
        .unwrap_or("/")
        .into()
}
pub fn size(bytes: u64) -> String {
    let mut value = bytes as f64;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 || unit == "TiB" {
            return if unit == "B" {
                format!("{bytes} B")
            } else {
                format!("{value:.1} {unit}")
            };
        }
        value /= 1024.0;
    }
    unreachable!()
}
