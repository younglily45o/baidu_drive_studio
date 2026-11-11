mod support;
use baidu_drive_studio::{
    cookies::{LinkAccess, normalize_cookie},
    model::{Credentials, RemoteFile},
    transfer::upload,
};
use std::sync::atomic::AtomicBool;
use support::{json, response, server};

#[test]
fn contradictory_partial_response_is_not_reported_as_a_verified_link() {
    let bad = response(
        "206 Partial Content",
        "Content-Range: bytes 0-0/6\r\n",
        "abcdef",
    );
    let (api, _, task) = server(vec![bad.clone(), bad], credentials());
    let report = api
        .probe_link(&format!("{}/cdn", api.endpoints().pan), 6)
        .unwrap();
    assert_eq!(report.access, LinkAccess::Unavailable);
    task.join().unwrap();
}
fn credentials() -> Credentials {
    Credentials {
        cookies: "BDUSS=test-bduss; STOKEN=test-stoken; BAIDUID=test-id".into(),
        ..Default::default()
    }
}
fn csrf() -> String {
    json(r#"{"errno":0,"result":{"bdstoken":"csrf-test"}}"#)
}
fn remote() -> RemoteFile {
    RemoteFile {
        fs_id: 42,
        path: "/资料/a.txt".into(),
        server_filename: "a.txt".into(),
        size: 6,
        ..Default::default()
    }
}

#[test]
fn cookie_parser_handles_prefix_duplicates_quotes_and_equals() {
    let c =
        normalize_cookie(" Cookie: BDUSS=old; STOKEN=\"value==\"; BDUSS=\"new==\"; BAIDUID=id ")
            .unwrap();
    assert!(c.contains("BDUSS=new=="));
    assert!(c.contains("STOKEN=value=="));
    assert!(!c.contains("old"));
}
#[test]
fn cookie_parser_ignores_standalone_undefined_and_keeps_valid_values() {
    let input = "Cookie: undefined; ppfuid=fake-id; undefined; XFT=fake-value==; BDUSS=fake-bduss; STOKEN=fake-stoken; RT=\"z=1&dm=baidu.com&bcn=https%3A%2F%2Fexample.invalid%2Ftest\"; undefined;";
    assert_eq!(
        normalize_cookie(input).unwrap(),
        "ppfuid=fake-id; XFT=fake-value==; BDUSS=fake-bduss; STOKEN=fake-stoken; RT=z=1&dm=baidu.com&bcn=https%3A%2F%2Fexample.invalid%2Ftest"
    );
    assert_eq!(
        normalize_cookie("BDUSS=fake; undefined=valid-cookie").unwrap(),
        "BDUSS=fake; undefined=valid-cookie"
    );
}
#[test]
fn undefined_residue_does_not_replace_required_login_cookie() {
    assert!(
        normalize_cookie("undefined; STOKEN=fake; undefined;")
            .unwrap_err()
            .to_string()
            .contains("BDUSS")
    );
}
#[test]
fn cookie_parser_rejects_missing_bduss_and_header_injection_without_echoing_input() {
    for bad in [
        "",
        "STOKEN=private",
        "BDUSS=secret\r\nX-Foo: bad",
        "BDUSS=",
        "BDUSS=\"unclosed",
        "BAD NAME=x; BDUSS=private",
        "BDUSS=secret; unexpected",
        "BDUSS=secret; undefined\r\nX-Foo: bad",
    ] {
        let error = normalize_cookie(bad).expect_err("invalid cookie accepted");
        assert!(!error.to_string().contains("secret"));
        assert!(!error.to_string().contains("private"));
    }
}
#[test]
fn cookie_login_and_paging_use_web_session_without_oauth_token() {
    let (api, requests, task) = server(
        vec![
            csrf(),
            json(
                r#"{"errno":0,"list":[{"fs_id":42,"path":"/资料/a.txt","server_filename":"a.txt","size":6,"isdir":0}]}"#,
            ),
        ],
        credentials(),
    );
    api.validate_cookie().unwrap();
    assert_eq!(api.list("/资料 &备份", 200).unwrap()[0].fs_id, 42);
    task.join().unwrap();
    let requests = requests.lock().unwrap();
    let login = String::from_utf8_lossy(&requests[0]);
    assert!(login.starts_with("GET /api/gettemplatevariable?"));
    assert!(login.to_lowercase().contains("cookie: bduss=test-bduss;"));
    let list = String::from_utf8_lossy(&requests[1]);
    assert!(list.starts_with("GET /api/list?"));
    assert!(list.contains("page=2"));
    assert!(list.contains("num=200"));
    assert!(!list.contains("access_token"));
    assert!(list.contains("bdstoken=csrf-test"));
}
#[test]
fn expired_cookie_reports_reimport_without_echoing_response_secret() {
    let (api, _, task) = server(
        vec![json(r#"{"errno":-6,"errmsg":"BDUSS=test-bduss"}"#)],
        credentials(),
    );
    let error = api.validate_cookie().unwrap_err();
    assert!(error.to_string().contains("Cookie"));
    assert!(!error.to_string().contains("test-bduss"));
    task.join().unwrap();
}
#[test]
fn login_requires_a_nonempty_server_bdstoken() {
    let (api, _, task) = server(
        vec![json(r#"{"errno":0,"result":{"bdstoken":""}}"#)],
        credentials(),
    );
    assert!(api.validate_cookie().is_err());
    task.join().unwrap();
}
#[test]
fn cookie_download_resolves_metadata_and_generates_web_signature() {
    let (api, requests, task) = server(
        vec![
            csrf(),
            json(r#"{"errno":0,"info":[{"fs_id":42,"path":"/资料/a.txt","size":6,"isdir":0}]}"#),
            response(
                "200 OK",
                "Content-Type: text/html\r\n",
                r#"<script>var metrics={"timestamp":1};var yunData={"sign1":"Plaintext","sign3":"Key","timestamp":123456};</script>"#,
            ),
            json(r#"{"errno":0,"dlink":[{"fs_id":"42","dlink":"{{BASE}}/cdn"}]}"#),
            response("200 OK", "", "abcdef"),
        ],
        credentials(),
    );
    api.validate_cookie().unwrap();
    let file = api.resolve_download(&remote()).unwrap();
    assert!(file.dlink.ends_with("/cdn"));
    assert_eq!(file.size, 6);
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("a.txt");
    baidu_drive_studio::transfer::download(
        &api,
        &file,
        &file.dlink,
        &target,
        &AtomicBool::new(false),
        |_, _, _| {},
    )
    .unwrap();
    assert_eq!(std::fs::read(target).unwrap(), b"abcdef");
    task.join().unwrap();
    let requests = requests.lock().unwrap();
    let metadata = String::from_utf8_lossy(&requests[1]);
    assert!(metadata.starts_with("GET /api/filemetas?"));
    assert!(metadata.contains("target=%5B%22%2F%E8%B5%84%E6%96%99%2Fa.txt%22%5D"));
    assert!(!metadata.contains("/rest/2.0/pcs/file"));
    let request = String::from_utf8_lossy(&requests[3]);
    assert!(request.starts_with("POST /api/download?"));
    assert!(request.contains("timestamp=123456"));
    assert!(request.contains("fidlist=%5B42%5D"));
    assert!(request.contains("sign=u%2FMW6NlArwrT"));
    let download = String::from_utf8_lossy(&requests[4]);
    assert!(
        download
            .to_ascii_lowercase()
            .contains("cookie: bduss=test-bduss;")
    );
    assert!(!download.contains("access_token"));
}

#[test]
fn cookie_web_metadata_can_supply_the_download_url_without_pcs_token_or_homepage() {
    let (api, requests, task) = server(
        vec![
            csrf(),
            json(
                r#"{"errno":0,"info":[{"fs_id":42,"path":"/资料/a.txt","server_filename":"a.txt","size":6,"isdir":0,"dlink":"{{BASE}}/cdn"}]}"#,
            ),
            response("200 OK", "", "abcdef"),
        ],
        credentials(),
    );
    let file = api.resolve_download(&remote()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("a.txt");
    baidu_drive_studio::transfer::download(
        &api,
        &file,
        &file.dlink,
        &target,
        &AtomicBool::new(false),
        |_, _, _| {},
    )
    .unwrap();
    assert_eq!(std::fs::read(target).unwrap(), b"abcdef");
    task.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(String::from_utf8_lossy(&requests[1]).starts_with("GET /api/filemetas?"));
    assert!(
        requests
            .iter()
            .all(|r| !String::from_utf8_lossy(r).contains("/rest/2.0/pcs/file"))
    );
}

#[test]
fn cookie_metadata_failure_names_the_stage_and_preserves_error_code_without_secret() {
    let (api, _, task) = server(
        vec![
            csrf(),
            json(r#"{"errno":31030,"errmsg":"pcs token not exist; test-bduss"}"#),
        ],
        credentials(),
    );
    let error = api.resolve_download(&remote()).unwrap_err();
    assert_eq!(error.code.as_deref(), Some("31030"));
    assert!(error.to_string().contains("获取文件信息"));
    assert!(error.to_string().contains("PCS"));
    assert!(!error.to_string().contains("test-bduss"));
    task.join().unwrap();
}

#[test]
fn web_metadata_rejects_missing_size_changed_identity_and_per_file_errors() {
    for info in [
        r#"{"fs_id":42,"path":"/资料/a.txt","isdir":0,"dlink":"{{BASE}}/cdn"}"#,
        r#"{"fs_id":99,"path":"/资料/a.txt","size":6,"isdir":0,"dlink":"{{BASE}}/cdn"}"#,
        r#"{"fs_id":42,"path":"/other/a.txt","size":6,"isdir":0,"dlink":"{{BASE}}/cdn"}"#,
        r#"{"errno":-9}"#,
        r#"{"fs_id":42,"path":"/资料/a.txt","size":6,"isdir":0,"dlink":"https://example.invalid/untrusted"}"#,
    ] {
        let (api, requests, task) = server(
            vec![csrf(), json(&format!(r#"{{"errno":0,"info":[{info}]}}"#))],
            credentials(),
        );
        assert!(api.resolve_download(&remote()).is_err());
        task.join().unwrap();
        assert_eq!(requests.lock().unwrap().len(), 2);
    }
}

#[test]
fn foreign_redirect_cannot_receive_cookie_credentials() {
    let redirect = response(
        "302 Found",
        "Location: https://example.invalid/untrusted\r\n",
        "",
    );
    let (api, requests, task) = server(vec![redirect.clone(), redirect], credentials());
    let report = api
        .probe_link(&format!("{}/cdn", api.endpoints().pan), 6)
        .unwrap();
    assert_eq!(report.access, LinkAccess::Unavailable);
    assert!(!report.detail.contains("test-bduss"));
    task.join().unwrap();
    assert_eq!(requests.lock().unwrap().len(), 2);
}
#[test]
fn raw_signed_link_probe_does_not_send_cookies_or_require_session() {
    let (api, requests, task) = server(
        vec![response(
            "206 Partial Content",
            "Content-Range: bytes 0-0/6\r\n",
            "a",
        )],
        credentials(),
    );
    let report = api
        .probe_link(&format!("{}/cdn", api.endpoints().pan), 6)
        .unwrap();
    assert_eq!(report.access, LinkAccess::Standalone);
    task.join().unwrap();
    let request = String::from_utf8_lossy(&requests.lock().unwrap()[0]).to_lowercase();
    assert!(!request.contains("cookie:"));
    assert!(!request.contains("access_token"));
    assert!(request.contains("range: bytes=0-0"));
}
#[test]
fn probe_distinguishes_session_required_links() {
    let (api, requests, task) = server(
        vec![
            response("403 Forbidden", "", "forbidden"),
            response("206 Partial Content", "Content-Range: bytes 0-0/6\r\n", "a"),
        ],
        credentials(),
    );
    let report = api
        .probe_link(&format!("{}/cdn", api.endpoints().pan), 6)
        .unwrap();
    assert_eq!(report.access, LinkAccess::SessionRequired);
    task.join().unwrap();
    let requests = requests.lock().unwrap();
    assert!(
        !String::from_utf8_lossy(&requests[0])
            .to_lowercase()
            .contains("cookie:")
    );
    assert!(
        String::from_utf8_lossy(&requests[1])
            .to_lowercase()
            .contains("cookie: bduss=test-bduss;")
    );
}
#[test]
fn html_error_is_not_mistaken_for_a_valid_download() {
    let (api, _, task) = server(
        vec![
            response("200 OK", "Content-Type: text/html\r\n", "login!"),
            response("200 OK", "Content-Type: text/html\r\n", "login!"),
        ],
        credentials(),
    );
    let report = api
        .probe_link(&format!("{}/cdn", api.endpoints().pan), 6)
        .unwrap();
    assert_eq!(report.access, LinkAccess::Unavailable);
    task.join().unwrap();
}
#[test]
fn cookie_upload_uses_web_precreate_complete_hashes_and_no_overwrite() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("a.txt");
    std::fs::write(&path, "abcdef").unwrap();
    let hash = format!("{:x}", md5::compute(b"abcdef"));
    let (api, requests, task) = server(
        vec![
            csrf(),
            json(r#"{"errno":0,"return_type":1,"uploadid":"u","block_list":[0]}"#),
            json(&format!(r#"{{"md5":"{hash}"}}"#)),
            json(r#"{"errno":0,"path":"/a.txt"}"#),
        ],
        credentials(),
    );
    api.validate_cookie().unwrap();
    assert_eq!(
        upload(&api, &path, "/a.txt", &AtomicBool::new(false), |_, _, _| {}).unwrap(),
        "/a.txt"
    );
    task.join().unwrap();
    let requests = requests.lock().unwrap();
    let pre = String::from_utf8_lossy(&requests[1]);
    assert!(pre.starts_with("POST /api/precreate?"));
    assert!(pre.contains(&format!("content-md5={hash}")));
    assert!(pre.contains(&format!("slice-md5={hash}")));
    assert!(pre.contains("contentCrc32=1267612143"));
    assert!(pre.contains("rtype=1"));
    assert!(!pre.contains("rtype=3"));
    assert!(!pre.contains("access_token"));
    let part = String::from_utf8_lossy(&requests[2]);
    assert!(part.contains("partoffset=0"));
    assert!(part.contains("app_id=250528"));
    assert!(String::from_utf8_lossy(&requests[3]).starts_with("POST /api/create?"));
    assert!(String::from_utf8_lossy(&requests[3]).contains("rtype=1"));
}
