use crate::model::{Credentials, Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub app_key: String,
    pub download_dir: PathBuf,
    pub upload_dir: String,
    pub remember_login: bool,
}
impl Default for Settings {
    fn default() -> Self {
        let base = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        Self {
            app_key: String::new(),
            download_dir: base.join("Downloads/Baidu-Drive-Studio"),
            upload_dir: "/apps/你的应用名称".into(),
            remember_login: false,
        }
    }
}
pub fn config_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("Baidu-Drive-Studio")
}
pub fn load_settings(directory: &Path) -> Result<Settings> {
    match fs::read(directory.join("settings.json")) {
        Ok(data) => Ok(serde_json::from_slice(&data)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(e.into()),
    }
}
pub fn save_settings(directory: &Path, settings: &Settings) -> Result<()> {
    fs::create_dir_all(directory)?;
    fs::write(
        directory.join("settings.json"),
        serde_json::to_vec_pretty(settings)?,
    )?;
    Ok(())
}
pub fn forget_credentials(directory: &Path) -> Result<()> {
    match fs::remove_file(directory.join("login.credentials")) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(windows)]
fn crypt(data: &[u8], protect: bool) -> Result<Vec<u8>> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).map_err(|_| Error::new("凭据数据过大"))?,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    // DPAPI owns output; copy it before LocalFree. No machine scope: current Windows user only.
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &input,
                null(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                null_mut(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(Error::new("Windows DPAPI 加密/解密失败，请重新登录"));
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData.cast());
    }
    Ok(bytes)
}
#[cfg(windows)]
pub fn save_credentials(directory: &Path, credentials: &Credentials) -> Result<()> {
    fs::create_dir_all(directory)?;
    fs::write(
        directory.join("login.credentials"),
        crypt(&serde_json::to_vec(credentials)?, true)?,
    )?;
    Ok(())
}
#[cfg(windows)]
pub fn load_credentials(directory: &Path) -> Result<Option<Credentials>> {
    match fs::read(directory.join("login.credentials")) {
        Ok(data) => Ok(Some(serde_json::from_slice(&crypt(&data, false)?)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
#[cfg(not(windows))]
pub fn save_credentials(_: &Path, _: &Credentials) -> Result<()> {
    Err(Error::new(
        "开发环境不保存登录凭据；Windows 版本支持 DPAPI 加密保存",
    ))
}
#[cfg(not(windows))]
pub fn load_credentials(_: &Path) -> Result<Option<Credentials>> {
    Ok(None)
}
