//! Cookie session protocol, based on the public BaiduPCS-Go protocol reference.
//! Signature interoperability follows its Apache-2.0 licensed Sign2 algorithm.
use crate::{
    api::{Api, PAGE_SIZE},
    model::{Error, RemoteFile, Result, parent, remote_directory},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{
    Url,
    blocking::RequestBuilder,
    header::{COOKIE, HeaderValue, REFERER, USER_AGENT},
};
use serde_json::Value;
use std::{
    io::Read,
    sync::{Arc, Mutex},
    time::Duration,
};

const APP_ID: &str = "250528";
pub const WEB_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36";

pub fn normalize_cookie(input: &str) -> Result<String> {
    let invalid = || Error::new("Cookie 格式无效，请粘贴网盘首页请求中的完整 Cookie 值");
    if input.len() > 65536 || input.chars().any(|c| c.is_control()) {
        return Err(invalid());
    }
    let mut input = input.trim();
    if input
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("cookie:"))
    {
        input = input[7..].trim();
    }
    let mut pairs: Vec<(String, String)> = vec![];
    for item in input.split(';').map(str::trim).filter(|v| !v.is_empty()) {
        // Some browser Cookie strings contain this stray JavaScript value without a name.
        if item == "undefined" {
            continue;
        }
        let (name, value) = item.split_once('=').ok_or_else(invalid)?;
        let name = name.trim();
        let mut value = value.trim();
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        {
            return Err(invalid());
        }
        if value.starts_with('"') || value.ends_with('"') {
            value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .ok_or_else(invalid)?;
        }
        if !value
            .bytes()
            .all(|b| matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
        {
            return Err(invalid());
        }
        if let Some(existing) = pairs.iter_mut().find(|(n, _)| n == name) {
            existing.1 = value.into();
        } else {
            pairs.push((name.into(), value.into()));
        }
    }
    if !pairs
        .iter()
        .any(|(name, value)| name == "BDUSS" && !value.is_empty())
    {
        return Err(Error::new(
            "Cookie 中缺少有效 BDUSS，请复制网盘首页的完整 Cookies",
        ));
    }
    Ok(pairs
        .into_iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; "))
}

#[derive(Clone)]
pub(crate) struct CookieSession {
    header: HeaderValue,
    csrf: Arc<Mutex<Option<String>>>,
}
impl CookieSession {
    pub(crate) fn new(raw: &str) -> Result<Self> {
        let mut header = HeaderValue::from_str(raw).map_err(|_| Error::new("Cookie 头格式无效"))?;
        header.set_sensitive(true);
        Ok(Self {
            header,
            csrf: Arc::new(Mutex::new(None)),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkAccess {
    Standalone,
    SessionRequired,
    Unavailable,
}
pub struct LinkReport {
    pub url: String,
    pub user_agent: String,
    pub referer: String,
    pub access: LinkAccess,
    pub detail: String,
}

pub(crate) fn baidu_host(host: &str) -> bool {
    ["baidu.com", "baidupcs.com", "baidubce.com"]
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

impl Api {
    pub fn is_cookie(&self) -> bool {
        self.cookie_session.is_some()
    }
    fn web_headers(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        let session = self
            .cookie_session
            .as_ref()
            .ok_or_else(|| Error::new("当前不是 Cookie 会话"))?;
        Ok(request
            .header(COOKIE, session.header.clone())
            .header(USER_AGENT, WEB_UA)
            .header(REFERER, format!("{}/disk/home", self.endpoints.pan)))
    }
    pub fn validate_cookie(&self) -> Result<()> {
        self.csrf_token().map(|_| ())
    }
    fn csrf_token(&self) -> Result<String> {
        let session = self
            .cookie_session
            .as_ref()
            .ok_or_else(|| Error::new("当前不是 Cookie 会话"))?;
        // UI never takes this mutex; serialize only background CSRF discovery.
        let mut csrf = session
            .csrf
            .lock()
            .map_err(|_| Error::new("Cookie 会话不可用，请重新登录"))?;
        if let Some(value) = csrf.as_ref() {
            return Ok(value.clone());
        }
        let v = self.json(
            self.web_headers(
                self.http
                    .get(format!("{}/api/gettemplatevariable", self.endpoints.pan))
                    .query(&[
                        ("clienttype", "0"),
                        ("app_id", APP_ID),
                        ("web", "1"),
                        ("fields", "[\"bdstoken\"]"),
                    ]),
            )?,
        )?;
        let token = v["result"]["bdstoken"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::new("Cookie 登录验证失败，未取得 bdstoken；请重新复制网盘首页 Cookies")
            })?;
        *csrf = Some(token.into());
        Ok(token.into())
    }
    pub(crate) fn cookie_get(&self, path: &str, params: &[(&str, String)]) -> Result<Value> {
        let token = self.csrf_token()?;
        self.json(
            self.web_headers(
                self.http
                    .get(format!("{}{path}", self.endpoints.pan))
                    .query(&[
                        ("app_id", APP_ID),
                        ("clienttype", "0"),
                        ("web", "1"),
                        ("bdstoken", &token),
                    ])
                    .query(params),
            )?,
        )
    }
    pub(crate) fn cookie_post(&self, method: &str, fields: &[(&str, String)]) -> Result<Value> {
        if !["precreate", "create"].contains(&method) {
            return Err(Error::new("不支持的 Cookie 写入操作"));
        }
        let token = self.csrf_token()?;
        let mut fields = fields.to_vec();
        if let Some((_, path)) = fields.iter().find(|(k, _)| *k == "path") {
            fields.push(("target_path", parent(path)));
        }
        self.json(
            self.web_headers(
                self.http
                    .post(format!("{}/api/{method}", self.endpoints.pan))
                    .query(&[
                        ("app_id", APP_ID),
                        ("clienttype", "0"),
                        ("web", "1"),
                        ("bdstoken", &token),
                    ])
                    .form(&fields),
            )?,
        )
    }
    pub(crate) fn cookie_list(&self, directory: &str, start: usize) -> Result<Vec<RemoteFile>> {
        let v = self.cookie_get(
            "/api/list",
            &[
                ("dir", remote_directory(directory)?),
                ("num", PAGE_SIZE.to_string()),
                ("page", (start / PAGE_SIZE + 1).to_string()),
                ("order", "name".into()),
                ("desc", "0".into()),
            ],
        )?;
        let mut list: Vec<RemoteFile> = serde_json::from_value(
            v.get("list")
                .cloned()
                .ok_or_else(|| Error::new("网盘列表缺少 list，请确认 Cookie 是否失效"))?,
        )?;
        list.sort_by_cached_key(|f| (f.isdir == 0, f.server_filename.to_lowercase()));
        Ok(list)
    }
    pub(crate) fn cookie_upload_block(
        &self,
        path: &str,
        upload_id: &str,
        index: usize,
        data: Vec<u8>,
    ) -> Result<String> {
        let form = reqwest::blocking::multipart::Form::new().part(
            "file",
            reqwest::blocking::multipart::Part::bytes(data).file_name("chunk"),
        );
        let v = self.json(
            self.web_headers(
                self.http
                    .post(format!("{}/rest/2.0/pcs/superfile2", self.endpoints.pcs))
                    .query(&[
                        ("method", "upload".into()),
                        ("type", "tmpfile".into()),
                        ("app_id", APP_ID.into()),
                        ("path", path.into()),
                        ("uploadid", upload_id.into()),
                        ("partseq", index.to_string()),
                        (
                            "partoffset",
                            (index as u64 * crate::transfer::BLOCK_SIZE as u64).to_string(),
                        ),
                    ])
                    .multipart(form),
            )?,
        )?;
        v["md5"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| Error::new("分片响应缺少 MD5"))
    }
    pub fn resolve_download(&self, source: &RemoteFile) -> Result<RemoteFile> {
        if !self.is_cookie() {
            return self
                .metadata(source.fs_id)
                .map_err(|e| e.context("获取下载信息"));
        }
        // Cookie sessions may lack the separate PCS token. Keep metadata on the web API.
        let target = serde_json::to_string(&[&source.path])?;
        let value = self
            .cookie_get(
                "/api/filemetas",
                &[
                    ("target", target),
                    ("dlink", "1".into()),
                    ("blocks", "0".into()),
                ],
            )
            .map_err(|e| e.context("获取文件信息"))?;
        let info = value["info"]
            .get(0)
            .cloned()
            .ok_or_else(|| Error::new("获取文件信息：网页接口未返回文件信息"))?;
        if let Some(code) = info["errno"].as_i64().filter(|n| *n != 0) {
            return Err(Error {
                code: Some(code.to_string()),
                message: format!("获取文件信息失败，请刷新目录并确认文件仍存在（{code}）"),
            });
        }
        if info["size"].as_u64().is_none() {
            return Err(Error::new("获取文件信息：响应缺少有效文件大小，已停止下载"));
        }
        let mut file: RemoteFile =
            serde_json::from_value(info).map_err(|e| Error::from(e).context("解析文件信息"))?;
        if file.fs_id != source.fs_id || file.isdir != 0 || file.path != source.path {
            return Err(Error::new("远端文件身份已改变，请刷新列表后重试"));
        }
        file.server_filename = source.server_filename.clone();
        if !file.dlink.is_empty() {
            file.dlink = self.secure_download_url(&file.dlink)?.to_string();
            return Ok(file);
        }
        let response = self
            .web_headers(self.http.get(format!("{}/disk/home", self.endpoints.pan)))?
            .send()?;
        if !response.status().is_success() {
            return Err(Error::new("Cookie 已失效或网盘首页不可用，请重新登录"));
        }
        let mut page = String::new();
        response
            .take(2 * 1024 * 1024 + 1)
            .read_to_string(&mut page)
            .map_err(|_| Error::new("无法读取网盘签名页面"))?;
        if page.len() > 2 * 1024 * 1024 {
            return Err(Error::new("网盘签名页面过大"));
        }
        let Some((sign1, sign3, timestamp)) = signature_values(&page) else {
            return Err(Error::new(
                "网盘下载签名未找到；Cookie 可能失效或网页接口已改变",
            ));
        };
        let sign = signature(&sign3, &sign1);
        let token = self.csrf_token()?;
        let v = self
            .json(
                self.web_headers(
                    self.http
                        .post(format!("{}/api/download", self.endpoints.pan))
                        .query(&[
                            ("app_id", APP_ID),
                            ("clienttype", "0"),
                            ("web", "1"),
                            ("bdstoken", &token),
                        ])
                        .form(&[
                            ("sign", sign),
                            ("timestamp", timestamp.to_string()),
                            ("fidlist", format!("[{}]", file.fs_id)),
                            ("type", "dlink".into()),
                        ]),
                )?,
            )
            .map_err(|e| e.context("获取下载直链"))?;
        let links = v["dlink"]
            .as_array()
            .ok_or_else(|| Error::new("服务端没有返回下载直链"))?;
        let link = links
            .iter()
            .find(|v| {
                v["fs_id"]
                    .as_u64()
                    .or_else(|| v["fs_id"].as_str().and_then(|s| s.parse().ok()))
                    == Some(file.fs_id)
            })
            .and_then(|v| v["dlink"].as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::new("没有取得所选文件的下载直链"))?;
        file.dlink = self.secure_download_url(link)?.to_string();
        file.server_filename = source.server_filename.clone();
        Ok(file)
    }
    fn secure_download_url(&self, link: &str) -> Result<Url> {
        let mut url = Url::parse(link).map_err(|_| Error::new("下载链接格式无效"))?;
        let local = self.endpoints.pan.starts_with("http://127.0.0.1:")
            && Url::parse(&self.endpoints.pan).is_ok_and(|p| p.origin() == url.origin());
        if !local {
            if !baidu_host(url.host_str().unwrap_or(""))
                || !["http", "https"].contains(&url.scheme())
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(Error::new("下载地址不是百度 HTTPS 地址，已停止请求"));
            }
            url.set_scheme("https")
                .map_err(|_| Error::new("无法使用 HTTPS 下载地址"))?;
        }
        url.set_fragment(None);
        Ok(url)
    }
    pub(crate) fn download_url(&self, link: &str) -> Result<Url> {
        let mut url = self.secure_download_url(link)?;
        if !self.is_cookie() {
            let pairs: Vec<_> = url
                .query_pairs()
                .filter(|(k, _)| k != "access_token")
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            url.set_query(None);
            url.query_pairs_mut()
                .extend_pairs(pairs)
                .append_pair("access_token", &self.token()?);
        }
        Ok(url)
    }
    pub(crate) fn download_request(&self, url: Url, session: bool) -> Result<RequestBuilder> {
        let mut request = self
            .http
            .get(url.clone())
            .header("Accept-Encoding", "identity")
            .header(
                USER_AGENT,
                if self.is_cookie() {
                    WEB_UA
                } else {
                    "pan.baidu.com"
                },
            )
            .header(REFERER, format!("{}/disk/home", self.endpoints.pan));
        let cookie_host = url
            .host_str()
            .is_some_and(|h| h == "baidu.com" || h.ends_with(".baidu.com"));
        let local = self.endpoints.pan.starts_with("http://127.0.0.1:")
            && Url::parse(&self.endpoints.pan).is_ok_and(|p| p.origin() == url.origin());
        if session
            && (cookie_host || local)
            && let Some(cookie) = &self.cookie_session
        {
            request = request.header(COOKIE, cookie.header.clone());
        }
        Ok(request)
    }
    pub fn inspect_direct_link(&self, file: &RemoteFile) -> Result<LinkReport> {
        let metadata = self.resolve_download(file)?;
        let link = self.download_url(&metadata.dlink)?;
        self.probe_link(link.as_str(), metadata.size)
    }
    pub fn probe_link(&self, link: &str, expected: u64) -> Result<LinkReport> {
        let url = self.secure_download_url(link)?;
        let first = self.probe_once(url.clone(), expected, false);
        let (access, detail) = match first {
            Ok(()) => (
                LinkAccess::Standalone,
                "已验证首字节：链接配合下方 UA / Referer 可用，不需要 Cookie。".into(),
            ),
            Err(first) if self.is_cookie() => match self.probe_once(url.clone(), expected, true) {
                Ok(()) => (
                    LinkAccess::SessionRequired,
                    "已验证首字节：需要当前 Cookie 会话；单独复制 URL 不保证可用。".into(),
                ),
                Err(second) => (
                    LinkAccess::Unavailable,
                    format!("无 Cookie 验证：{first}；会话验证：{second}"),
                ),
            },
            Err(e) => (LinkAccess::Unavailable, e.to_string()),
        };
        Ok(LinkReport {
            url: url.into(),
            user_agent: if self.is_cookie() {
                WEB_UA
            } else {
                "pan.baidu.com"
            }
            .into(),
            referer: format!("{}/disk/home", self.endpoints.pan),
            access,
            detail,
        })
    }
    fn probe_once(&self, url: Url, expected: u64, session: bool) -> Result<()> {
        let mut response = self
            .download_request(url, session)?
            .timeout(Duration::from_secs(15))
            .header("Range", "bytes=0-0")
            .send()?;
        let status = response.status();
        if response
            .headers()
            .get("content-encoding")
            .is_some_and(|v| v != "identity")
        {
            return Err(Error::new("服务器返回了压缩内容，无法确认首字节"));
        }
        if status == reqwest::StatusCode::PARTIAL_CONTENT {
            let wanted = format!("bytes 0-0/{expected}");
            if response
                .headers()
                .get("content-range")
                .and_then(|h| h.to_str().ok())
                != Some(wanted.as_str())
            {
                return Err(Error::new("返回的首字节范围或文件大小不匹配"));
            }
            if expected == 0 || response.content_length().is_some_and(|n| n != 1) {
                return Err(Error::new("首字节响应长度与 Range 不一致"));
            }
        } else if status == reqwest::StatusCode::OK {
            let content = response
                .headers()
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("");
            let attachment = response
                .headers()
                .get("content-disposition")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|s| s.to_ascii_lowercase().contains("attachment"));
            if response.content_length() != Some(expected)
                || (!attachment
                    && (content.contains("text/html") || content.contains("application/json")))
            {
                return Err(Error::new("返回内容不是可确认的文件响应"));
            }
        } else {
            return Err(Error::new(format!("HTTP {}", status.as_u16())));
        }
        if expected > 0 {
            let mut byte = [0];
            response
                .read_exact(&mut byte)
                .map_err(|_| Error::new("未能读取文件首字节"))?;
            if status == reqwest::StatusCode::PARTIAL_CONTENT
                && response
                    .read(&mut byte)
                    .map_err(|_| Error::new("首字节响应被截断"))?
                    != 0
            {
                return Err(Error::new("首字节响应包含多余数据"));
            }
        }
        Ok(())
    }
}

fn signature_values(page: &str) -> Option<(String, String, u64)> {
    // Extract one JSON object, avoiding unrelated timestamps in preceding performance scripts.
    for (key_at, _) in page.match_indices("\"sign1\"").take(128) {
        for (start, _) in page[..key_at].rmatch_indices('{').take(32) {
            let Some(Ok(Value::Object(value))) = serde_json::Deserializer::from_str(&page[start..])
                .into_iter::<Value>()
                .next()
            else {
                continue;
            };
            let sign1 = value
                .get("sign1")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            let sign3 = value
                .get("sign3")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            let timestamp = value
                .get("timestamp")
                .and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })
                .filter(|n| *n > 0);
            if let (Some(a), Some(b), Some(t)) = (sign1, sign3, timestamp) {
                return Some((a.into(), b.into(), t));
            }
        }
    }
    None
}

fn signature(key: &str, text: &str) -> String {
    // RC4-compatible stream transform required by the web download protocol.
    let key: Vec<u32> = key.chars().map(u32::from).collect();
    let mut table: Vec<u32> = (0..256).collect();
    let mut j = 0;
    for i in 0..256 {
        j = (j + table[i] as usize + key[i % key.len()] as usize) % 256;
        table.swap(i, j);
    }
    let (mut i, mut j) = (0, 0);
    let bytes: Vec<u8> = text
        .chars()
        .map(|c| {
            i = (i + 1) % 256;
            j = (j + table[i] as usize) % 256;
            table.swap(i, j);
            (u32::from(c) ^ table[((table[i] + table[j]) % 256) as usize]) as u8
        })
        .collect();
    STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signature_data_can_follow_a_non_json_key_occurrence_in_page_scripts() {
        let page = r#"<script>var fields=["sign1"]; var data={"sign1":"a\"b","sign3":"key","timestamp":10};</script>"#;
        assert_eq!(
            signature_values(page),
            Some(("a\"b".into(), "key".into(), 10))
        );
    }
    #[test]
    fn download_urls_upgrade_https_and_keep_cookie_auth_out_of_urls() {
        let api = Api::new(crate::model::Credentials {
            cookies: "BDUSS=private-bduss".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            api.download_url("http://d.pcs.baidu.com/a?sign=value")
                .unwrap()
                .as_str(),
            "https://d.pcs.baidu.com/a?sign=value"
        );
        assert!(
            api.download_url("https://baidu.com.attacker.invalid/file")
                .is_err()
        );
        assert!(
            api.download_url("https://user:password@d.pcs.baidu.com/file")
                .is_err()
        );
        let request = api
            .download_request(Url::parse("https://cdn.baidupcs.com/a").unwrap(), true)
            .unwrap()
            .build()
            .unwrap();
        assert!(request.headers().get(COOKIE).is_none());
        let request = api
            .download_request(Url::parse("https://d.pcs.baidu.com/a").unwrap(), true)
            .unwrap()
            .build()
            .unwrap();
        assert!(request.headers().get(COOKIE).is_some());
    }
}
