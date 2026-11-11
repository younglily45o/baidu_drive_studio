use crate::model::{Credentials, DeviceCode, Error, RemoteFile, Result, now, remote_directory};
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::Deserialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const PAGE_SIZE: usize = 200;

#[derive(Clone)]
pub struct Endpoints {
    pub pan: String,
    pub oauth: String,
    pub pcs: String,
}
impl Default for Endpoints {
    fn default() -> Self {
        Self {
            pan: "https://pan.baidu.com".into(),
            oauth: "https://openapi.baidu.com".into(),
            pcs: "https://d.pcs.baidu.com".into(),
        }
    }
}

#[derive(Clone)]
pub struct Api {
    pub(crate) http: Client,
    credentials: Arc<Mutex<Credentials>>,
    refresh_lock: Arc<Mutex<()>>,
    pub(crate) endpoints: Endpoints,
    pub(crate) cookie_session: Option<crate::cookies::CookieSession>,
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    expires_in: u64,
}

impl Api {
    pub fn new(credentials: Credentials) -> Result<Self> {
        Self::with_endpoints(credentials, Endpoints::default())
    }
    pub fn with_endpoints(mut credentials: Credentials, endpoints: Endpoints) -> Result<Self> {
        let cookie_session = if credentials.cookies.is_empty() {
            None
        } else {
            credentials.cookies = crate::cookies::normalize_cookie(&credentials.cookies)?;
            Some(crate::cookies::CookieSession::new(&credentials.cookies)?)
        };
        let local_origins: Vec<_> = [&endpoints.pan, &endpoints.pcs, &endpoints.oauth]
            .iter()
            .filter_map(|s| reqwest::Url::parse(s).ok())
            .filter(|u| u.host_str() == Some("127.0.0.1"))
            .map(|u| u.origin())
            .collect();
        let mut builder = Client::builder()
            .user_agent("pan.baidu.com")
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .https_only(endpoints.pan.starts_with("https://"))
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 5 {
                    return attempt.error("redirect limit");
                }
                let url = attempt.url();
                if (url.scheme() == "https"
                    && crate::cookies::baidu_host(url.host_str().unwrap_or("")))
                    || local_origins.contains(&url.origin())
                {
                    attempt.follow()
                } else {
                    attempt.error("untrusted redirect")
                }
            }));
        if endpoints.pan.starts_with("http://127.0.0.1:") {
            builder = builder.no_proxy();
        }
        let http = builder.build()?;
        Ok(Self {
            http,
            credentials: Arc::new(Mutex::new(credentials)),
            refresh_lock: Arc::new(Mutex::new(())),
            endpoints,
            cookie_session,
        })
    }
    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }
    pub fn credentials(&self) -> Credentials {
        self.credentials
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn install_credentials(&self, credentials: Credentials) {
        *self.credentials.lock().unwrap_or_else(|e| e.into_inner()) = credentials;
    }
    pub fn has_token(&self) -> bool {
        !self.credentials().access_token.is_empty()
    }
    pub fn token(&self) -> Result<String> {
        if self.is_cookie() {
            return Err(Error::new("当前使用 Cookie 会话，无需 OAuth Token"));
        }
        // Only background API callers wait here. GUI credential snapshots never take this lock.
        let _refresh = self
            .refresh_lock
            .lock()
            .map_err(|_| Error::new("登录状态不可用，请重新登录"))?;
        let snapshot = self.credentials();
        let mut c = snapshot.clone();
        if c.access_token.trim().is_empty() {
            return Err(Error::new("请先登录百度网盘"));
        }
        if c.expires_at != 0 && c.expires_at <= now() + 60 {
            if c.refresh_token.is_empty() || c.app_key.is_empty() || c.secret_key.is_empty() {
                return Err(Error::new("授权已到期，请重新登录"));
            }
            let value = self.oauth(
                "/oauth/2.0/token",
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &c.refresh_token),
                    ("client_id", &c.app_key),
                    ("client_secret", &c.secret_key),
                ],
            )?;
            let token: Token = serde_json::from_value(value)?;
            if token.access_token.is_empty() {
                return Err(Error::new("刷新授权未返回有效令牌"));
            }
            c.access_token = token.access_token;
            c.refresh_token = token.refresh_token;
            c.expires_at = now().saturating_add(token.expires_in);
            let mut current = self
                .credentials
                .lock()
                .map_err(|_| Error::new("登录状态不可用"))?;
            if *current == snapshot {
                *current = c.clone();
            } else {
                return Ok(current.access_token.clone());
            }
        }
        Ok(c.access_token.clone())
    }
    pub fn force_refresh(&self) -> Result<()> {
        {
            let mut c = self
                .credentials
                .lock()
                .map_err(|_| Error::new("登录状态不可用"))?;
            if c.refresh_token.is_empty() {
                return Err(Error::new("当前登录没有 Refresh Token，请重新授权"));
            }
            c.expires_at = 1;
        }
        self.token().map(|_| ())
    }
    fn oauth(&self, path: &str, params: &[(&str, &str)]) -> Result<Value> {
        self.json(
            self.http
                .get(format!("{}{path}", self.endpoints.oauth))
                .query(params),
        )
    }
    pub fn device_code(&self, app_key: &str) -> Result<DeviceCode> {
        if app_key.trim().is_empty() {
            return Err(Error::new("请填写开放平台 AppKey"));
        }
        let v = self.oauth(
            "/oauth/2.0/device/code",
            &[
                ("response_type", "device_code"),
                ("client_id", app_key),
                ("scope", "basic,netdisk"),
            ],
        )?;
        let code: DeviceCode = serde_json::from_value(v)?;
        if code.device_code.is_empty() || code.user_code.is_empty() || code.expires_in == 0 {
            return Err(Error::new("未获取到有效设备码"));
        }
        Ok(code)
    }
    pub fn device_token(
        &self,
        app_key: &str,
        secret_key: &str,
        device: &str,
    ) -> Result<Credentials> {
        let v = self.oauth(
            "/oauth/2.0/token",
            &[
                ("grant_type", "device_token"),
                ("code", device),
                ("client_id", app_key),
                ("client_secret", secret_key),
            ],
        )?;
        let t: Token = serde_json::from_value(v)?;
        if t.access_token.is_empty() {
            return Err(Error::new("授权未返回有效令牌"));
        }
        Ok(Credentials {
            app_key: app_key.into(),
            secret_key: secret_key.into(),
            access_token: t.access_token,
            refresh_token: t.refresh_token,
            expires_at: now().saturating_add(t.expires_in),
            cookies: String::new(),
        })
    }
    pub fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Value> {
        self.json(
            self.http
                .get(format!("{}{path}", self.endpoints.pan))
                .query(&[("access_token", self.token()?)])
                .query(params),
        )
    }
    pub fn post(&self, method: &str, fields: &[(&str, String)]) -> Result<Value> {
        if self.is_cookie() {
            return self.cookie_post(method, fields);
        }
        self.json(
            self.http
                .post(format!("{}/rest/2.0/xpan/file", self.endpoints.pan))
                .query(&[
                    ("method", method.to_owned()),
                    ("access_token", self.token()?),
                ])
                .form(fields),
        )
    }
    pub fn list(&self, directory: &str, start: usize) -> Result<Vec<RemoteFile>> {
        if self.is_cookie() {
            return self.cookie_list(directory, start);
        }
        let v = self.get(
            "/rest/2.0/xpan/file",
            &[
                ("method", "list".into()),
                ("dir", remote_directory(directory)?),
                ("start", start.to_string()),
                ("limit", PAGE_SIZE.to_string()),
                ("order", "name".into()),
                ("folder", "0".into()),
            ],
        )?;
        let mut list: Vec<RemoteFile> = serde_json::from_value(
            v.get("list")
                .cloned()
                .ok_or_else(|| Error::new("文件列表响应缺少 list"))?,
        )?;
        list.sort_by_cached_key(|f| (f.isdir == 0, f.server_filename.to_lowercase()));
        Ok(list)
    }
    pub fn quota(&self) -> Result<(u64, u64)> {
        let v = if self.is_cookie() {
            self.cookie_get("/api/quota", &[("checkfree", "1".into())])?
        } else {
            self.get("/api/quota", &[("checkfree", "1".into())])?
        };
        Ok((
            v["used"].as_u64().unwrap_or(0),
            v["total"].as_u64().unwrap_or(0),
        ))
    }
    pub fn metadata(&self, fs_id: u64) -> Result<RemoteFile> {
        let v = self.get(
            "/rest/2.0/xpan/multimedia",
            &[
                ("method", "filemetas".into()),
                ("fsids", format!("[{fs_id}]")),
                ("dlink", "1".into()),
            ],
        )?;
        let file: RemoteFile = serde_json::from_value(
            v["list"]
                .get(0)
                .cloned()
                .ok_or_else(|| Error::new("未找到文件或没有下载权限"))?,
        )?;
        if file.fs_id != fs_id || file.isdir != 0 || file.dlink.is_empty() {
            return Err(Error::new("服务端没有返回可下载文件"));
        }
        Ok(file)
    }
    pub fn upload_block(
        &self,
        path: &str,
        upload_id: &str,
        index: usize,
        data: Vec<u8>,
    ) -> Result<String> {
        if self.is_cookie() {
            return self.cookie_upload_block(path, upload_id, index, data);
        }
        let form = reqwest::blocking::multipart::Form::new().part(
            "file",
            reqwest::blocking::multipart::Part::bytes(data).file_name("chunk"),
        );
        let v = self.json(
            self.http
                .post(format!("{}/rest/2.0/pcs/superfile2", self.endpoints.pcs))
                .query(&[
                    ("method", "upload".into()),
                    ("type", "tmpfile".into()),
                    ("path", path.to_owned()),
                    ("uploadid", upload_id.to_owned()),
                    ("partseq", index.to_string()),
                    ("access_token", self.token()?),
                ])
                .multipart(form),
        )?;
        v["md5"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| Error::new("分片上传响应缺少 MD5"))
    }
    pub fn download_response(&self, link: &str, offset: u64) -> Result<Response> {
        let url = self.download_url(link)?;
        let mut request = self.download_request(url, true)?;
        if offset > 0 {
            request = request.header("Range", format!("bytes={offset}-"));
        }
        Ok(request.send()?)
    }
    pub fn json(&self, request: RequestBuilder) -> Result<Value> {
        let response = request.send()?;
        // OAuth may use HTTP 400 with a useful structured error; parse it before status handling.
        let status = response.status();
        let v: Value = response.json()?;
        let code = v
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                v.get("errno")
                    .and_then(Value::as_i64)
                    .filter(|n| *n != 0)
                    .map(|n| n.to_string())
            })
            .or_else(|| {
                v.get("error_code")
                    .and_then(Value::as_i64)
                    .filter(|n| *n != 0)
                    .map(|n| n.to_string())
            });
        if let Some(code) = code {
            let message = match code.as_str() {
                "authorization_pending" => "等待浏览器授权",
                "slow_down" => "授权检查过于频繁，正在降低频率",
                "expired_token" => "设备码已过期，请重新登录",
                "access_denied" => "用户拒绝了授权",
                "invalid_client" => "AppKey 或 SecretKey 无效，请检查应用配置",
                "invalid_grant" => "授权已失效，请重新登录",
                "-6" | "31045" if self.is_cookie() => "Cookie 无效或已到期，请重新从网盘首页导入",
                "110" | "111" | "-6" => "登录令牌无效或已到期，请重新授权",
                "6" | "31023" => "没有操作权限，请检查应用授权范围及目标目录",
                "31066" | "-9" => "远端文件或目录不存在",
                "31061" => "远端文件已存在",
                "31064" => "网盘空间不足",
                "31030" => "PCS 会话令牌不存在；请更新客户端后重试，仍失败时重新导入网盘 Cookies",
                _ => "百度接口返回错误，请查看官方错误码文档",
            };
            // Only show a bounded machine code, never echo error_description/errmsg.
            let safe_code: String = code
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .take(48)
                .collect();
            return Err(Error {
                code: Some(code),
                message: format!("{message}（{safe_code}）"),
            });
        }
        if !status.is_success() {
            return Err(Error::new(format!("服务器返回 HTTP {}", status.as_u16())));
        }
        Ok(v)
    }
}
