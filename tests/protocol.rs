use baidu_drive_studio::{
    api::{Api, Endpoints},
    model::{Credentials, RemoteFile, valid_filename},
    transfer::{download, upload},
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex, atomic::AtomicBool},
    thread,
};

type Requests = Arc<Mutex<Vec<Vec<u8>>>>;

fn server(replies: Vec<String>) -> (Api, Requests, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests: Requests = Default::default();
    let captured = requests.clone();
    listener.set_nonblocking(true).unwrap();
    let task = thread::spawn(move || {
        for reply in replies {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(stream) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(2))
                    }
                    Err(e) => panic!("expected HTTP request did not arrive: {e}"),
                }
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = vec![];
            let mut buf = [0; 8192];
            loop {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let size = header
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + size {
                        break;
                    }
                }
            }
            captured.lock().unwrap().push(bytes);
            stream.write_all(reply.as_bytes()).unwrap();
        }
    });
    let api = Api::with_endpoints(
        Credentials {
            access_token: "test-secret-token".into(),
            ..Default::default()
        },
        Endpoints {
            pan: base.clone(),
            oauth: base.clone(),
            pcs: base,
        },
    )
    .unwrap();
    (api, requests, task)
}

fn response(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
}
fn json(body: &str) -> String {
    response("200 OK", "Content-Type: application/json\r\n", body)
}
fn file() -> RemoteFile {
    RemoteFile {
        fs_id: 42,
        path: "/资料/test.txt".into(),
        server_filename: "test.txt".into(),
        size: 6,
        ..Default::default()
    }
}

#[test]
fn windows_names_cannot_escape_or_use_devices() {
    for name in [
        "../a", "..", "a/b", "a\\b", "NUL.txt", "COM1", "a:", "a.", "a ", "",
    ] {
        assert!(!valid_filename(name), "{name}");
    }
    assert!(valid_filename("中文 文件.txt"));
}

#[test]
#[cfg(target_os = "linux")]
fn download_resumes_beyond_seven_gib_with_a_64_bit_range() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("large.bin");
    let offset = 7_u64 * 1024 * 1024 * 1024 + 123;
    let mut remote = file();
    remote.size = offset + 3;
    let (partial, metadata) =
        baidu_drive_studio::transfer::partial_paths(&target, remote.fs_id).unwrap();
    std::fs::File::create(&partial)
        .unwrap()
        .set_len(offset)
        .unwrap();
    std::fs::write(metadata, serde_json::to_vec(&serde_json::json!({"fs_id":remote.fs_id,"path":remote.path,"size":remote.size,"md5":remote.md5})).unwrap()).unwrap();
    let range = format!(
        "Content-Range: bytes {offset}-{}/{}\r\n",
        offset + 2,
        remote.size
    );
    let (api, requests, task) = server(vec![response("206 Partial Content", &range, "end")]);
    download(
        &api,
        &remote,
        &api.endpoints().pan,
        &target,
        &AtomicBool::new(false),
        |_, _, _| {},
    )
    .unwrap();
    assert_eq!(std::fs::metadata(&target).unwrap().len(), remote.size);
    let mut input = std::fs::File::open(target).unwrap();
    std::io::Seek::seek(&mut input, std::io::SeekFrom::Start(offset)).unwrap();
    let mut tail = [0; 3];
    input.read_exact(&mut tail).unwrap();
    assert_eq!(&tail, b"end");
    task.join().unwrap();
    assert!(
        String::from_utf8_lossy(&requests.lock().unwrap()[0])
            .to_ascii_lowercase()
            .contains(&format!("range: bytes={offset}-"))
    );
}

#[test]
fn download_request_keeps_a_64_bit_resume_offset() {
    let offset = 7_u64 * 1024 * 1024 * 1024 + 123;
    let range = format!(
        "Content-Range: bytes {offset}-{}/{}\r\n",
        offset + 2,
        offset + 3
    );
    let (api, requests, task) = server(vec![response("206 Partial Content", &range, "end")]);
    let response = api.download_response(&api.endpoints().pan, offset).unwrap();
    assert_eq!(response.text().unwrap(), "end");
    task.join().unwrap();
    assert!(
        String::from_utf8_lossy(&requests.lock().unwrap()[0])
            .to_ascii_lowercase()
            .contains(&format!("range: bytes={offset}-"))
    );
}

#[test]
fn list_encodes_chinese_path_and_paginates() {
    let (api, requests, task) = server(vec![json(
        r#"{"errno":0,"list":[{"fs_id":42,"path":"/资料/test.txt","server_filename":"test.txt","size":6,"isdir":0}]}"#,
    )]);
    let files = api.list("/资料 &备份", 200).unwrap();
    assert_eq!(files[0].server_filename, "test.txt");
    task.join().unwrap();
    let reqs = requests.lock().unwrap();
    let first = String::from_utf8_lossy(&reqs[0]);
    assert!(first.contains("start=200"));
    assert!(first.contains("%E8%B5%84%E6%96%99+%26%E5%A4%87%E4%BB%BD"));
}

#[test]
fn oauth_error_does_not_echo_server_secrets() {
    let (api, _, task) = server(vec![json(
        r#"{"error":"invalid_client","error_description":"secret=test-secret-token"}"#,
    )]);
    let error = api.device_code("key").unwrap_err();
    assert_eq!(error.code.as_deref(), Some("invalid_client"));
    assert!(!error.to_string().contains("test-secret-token"));
    task.join().unwrap();
}

#[test]
fn downloads_publish_only_complete_files_and_never_overwrite() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("test.txt");
    let (api, _, task) = server(vec![response("200 OK", "", "abcdef")]);
    download(
        &api,
        &file(),
        &api.endpoints().pan,
        &target,
        &AtomicBool::new(false),
        |_, _, _| {},
    )
    .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"abcdef");
    assert!(
        download(
            &api,
            &file(),
            &api.endpoints().pan,
            &target,
            &AtomicBool::new(false),
            |_, _, _| {}
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"abcdef");
    task.join().unwrap();
}

#[test]
fn truncated_download_can_resume_with_validated_range() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("test.txt");
    let (api, requests, task) = server(vec![
        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 6\r\n\r\nabc".into(),
        response(
            "206 Partial Content",
            "Content-Range: bytes 3-5/6\r\n",
            "def",
        ),
    ]);
    assert!(
        download(
            &api,
            &file(),
            &api.endpoints().pan,
            &target,
            &AtomicBool::new(false),
            |_, _, _| {}
        )
        .is_err()
    );
    assert!(!target.exists());
    download(
        &api,
        &file(),
        &api.endpoints().pan,
        &target,
        &AtomicBool::new(false),
        |_, _, _| {},
    )
    .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"abcdef");
    task.join().unwrap();
    assert!(
        String::from_utf8_lossy(&requests.lock().unwrap()[1])
            .to_lowercase()
            .contains("range: bytes=3-")
    );
}

#[test]
fn ignored_range_restarts_instead_of_appending() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("test.txt");
    let (api, _, task) = server(vec![
        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 6\r\n\r\nabc".into(),
        response("200 OK", "", "abcdef"),
    ]);
    assert!(
        download(
            &api,
            &file(),
            &api.endpoints().pan,
            &target,
            &AtomicBool::new(false),
            |_, _, _| {}
        )
        .is_err()
    );
    download(
        &api,
        &file(),
        &api.endpoints().pan,
        &target,
        &AtomicBool::new(false),
        |_, _, _| {},
    )
    .unwrap();
    assert_eq!(std::fs::read(target).unwrap(), b"abcdef");
    task.join().unwrap();
}

#[test]
fn wrong_range_is_rejected_without_publishing() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("test.txt");
    let (api, _, task) = server(vec![response(
        "206 Partial Content",
        "Content-Range: bytes 1-5/6\r\n",
        "bcdef",
    )]);
    assert!(
        download(
            &api,
            &file(),
            &api.endpoints().pan,
            &target,
            &AtomicBool::new(false),
            |_, _, _| {}
        )
        .is_err()
    );
    assert!(!target.exists());
    task.join().unwrap();
}

#[test]
fn cancellation_before_start_never_creates_final_file() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("test.txt");
    let api = Api::new(Credentials::default()).unwrap();
    assert!(
        download(
            &api,
            &file(),
            "https://example.invalid",
            &target,
            &AtomicBool::new(true),
            |_, _, _| {}
        )
        .is_err()
    );
    assert!(!target.exists());
}

#[test]
fn upload_honors_missing_block_indices_and_submits_complete_hash_list() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("upload.bin");
    let mut data = vec![b'a'; 4 * 1024 * 1024];
    data.extend_from_slice(b"end");
    std::fs::write(&path, &data).unwrap();
    let md5_first = format!("{:x}", md5::compute(&data[..4 * 1024 * 1024]));
    let md5_last = format!("{:x}", md5::compute(b"end"));
    let (api, requests, task) = server(vec![
        json(r#"{"errno":0,"uploadid":"upload-test","return_type":1,"block_list":[1]}"#),
        json(&format!(r#"{{"errno":0,"md5":"{md5_last}"}}"#)),
        json(r#"{"errno":0,"path":"/apps/demo/upload.bin","fs_id":100}"#),
    ]);
    let remote = upload(
        &api,
        &path,
        "/apps/demo/upload.bin",
        &AtomicBool::new(false),
        |_, _, _| {},
    )
    .unwrap();
    assert_eq!(remote, "/apps/demo/upload.bin");
    task.join().unwrap();
    let reqs = requests.lock().unwrap();
    assert!(String::from_utf8_lossy(&reqs[1]).contains("partseq=1"));
    let create = String::from_utf8_lossy(&reqs[2]);
    assert!(create.contains(&md5_first));
    assert!(create.contains(&md5_last));
    assert!(create.contains("rtype=1"));
    assert!(!create.contains("rtype=3"));
}

#[test]
fn expired_access_token_refreshes_once_and_rotates_shared_credentials() {
    let (api, requests, task) = server(vec![
        json(r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#),
        json(r#"{"errno":0,"list":[]}"#),
    ]);
    api.install_credentials(Credentials {
        app_key: "app-key".into(),
        secret_key: "app-secret".into(),
        access_token: "old-access".into(),
        refresh_token: "old-refresh".into(),
        expires_at: 1,
        ..Default::default()
    });
    let shared = api.clone();
    assert!(api.list("/", 0).unwrap().is_empty());
    assert_eq!(shared.credentials().refresh_token, "new-refresh");
    assert_eq!(shared.token().unwrap(), "new-access");
    task.join().unwrap();
    let requests = requests.lock().unwrap();
    assert!(String::from_utf8_lossy(&requests[0]).contains("grant_type=refresh_token"));
    assert!(String::from_utf8_lossy(&requests[1]).contains("access_token=new-access"));
}

#[test]
fn pending_device_authorization_parses_http_400_without_exposing_description() {
    let (api, _, task) = server(vec![response(
        "400 Bad Request",
        "Content-Type: application/json\r\n",
        r#"{"error":"authorization_pending","error_description":"test-secret-token"}"#,
    )]);
    let error = api.device_token("key", "secret", "device").err().unwrap();
    assert_eq!(error.code.as_deref(), Some("authorization_pending"));
    assert!(!error.to_string().contains("test-secret-token"));
    task.join().unwrap();
}

#[test]
fn changed_remote_identity_cannot_reuse_old_partial_data() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("test.txt");
    let (api, _, task) = server(vec![
        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 6\r\n\r\nabc".into(),
    ]);
    assert!(
        download(
            &api,
            &file(),
            &api.endpoints().pan,
            &target,
            &AtomicBool::new(false),
            |_, _, _| {}
        )
        .is_err()
    );
    task.join().unwrap();
    let mut changed = file();
    changed.md5 = "new-version".into();
    assert!(
        download(
            &api,
            &changed,
            &api.endpoints().pan,
            &target,
            &AtomicBool::new(false),
            |_, _, _| {}
        )
        .unwrap_err()
        .to_string()
        .contains("远端文件已改变")
    );
    assert!(!target.exists());
}

#[test]
fn cancellation_after_last_bytes_retains_a_resumable_complete_partial() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("test.txt");
    let (api, _, task) = server(vec![response("200 OK", "", "abcdef")]);
    let cancel = AtomicBool::new(false);
    assert!(
        download(
            &api,
            &file(),
            &api.endpoints().pan,
            &target,
            &cancel,
            |done, _, _| {
                if done == 6 {
                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        )
        .is_err()
    );
    task.join().unwrap();
    assert!(!target.exists());
    download(
        &api,
        &file(),
        &api.endpoints().pan,
        &target,
        &AtomicBool::new(false),
        |_, _, _| {},
    )
    .unwrap();
    assert_eq!(std::fs::read(target).unwrap(), b"abcdef");
}

#[test]
fn destination_created_during_download_is_never_replaced() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("test.txt");
    let (api, _, task) = server(vec![response("200 OK", "", "abcdef")]);
    let mut occupied = false;
    assert!(
        download(
            &api,
            &file(),
            &api.endpoints().pan,
            &target,
            &AtomicBool::new(false),
            |done, _, _| {
                if done == 6 && !occupied {
                    std::fs::write(&target, "user-data").unwrap();
                    occupied = true;
                }
            }
        )
        .is_err()
    );
    task.join().unwrap();
    assert_eq!(std::fs::read(target).unwrap(), b"user-data");
}

#[test]
fn upload_rejects_invalid_server_block_index_before_sending_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("upload.bin");
    std::fs::write(&path, "abc").unwrap();
    let (api, requests, task) = server(vec![json(
        r#"{"errno":0,"uploadid":"u","return_type":1,"block_list":[9]}"#,
    )]);
    assert!(
        upload(
            &api,
            &path,
            "/apps/demo/a.bin",
            &AtomicBool::new(false),
            |_, _, _| {}
        )
        .is_err()
    );
    task.join().unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[test]
fn upload_does_not_commit_after_cancellation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("upload.bin");
    std::fs::write(&path, "abc").unwrap();
    let (api, requests, task) = server(vec![json(
        r#"{"errno":0,"uploadid":"u","return_type":1,"block_list":[]}"#,
    )]);
    let cancel = AtomicBool::new(false);
    assert!(
        upload(&api, &path, "/apps/demo/a.bin", &cancel, |_, _, stage| {
            if stage == "上传中" {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        })
        .is_err()
    );
    task.join().unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[test]
fn credential_snapshot_remains_responsive_during_network_refresh() {
    use std::{sync::mpsc, time::Duration};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (arrived_tx, arrived_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server_task = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0; 4096];
        assert!(stream.read(&mut buffer).unwrap() > 0);
        arrived_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        stream
            .write_all(
                json(r#"{"access_token":"new","refresh_token":"rotated","expires_in":3600}"#)
                    .as_bytes(),
            )
            .unwrap();
    });
    let api = Api::with_endpoints(
        Credentials {
            app_key: "key".into(),
            secret_key: "secret".into(),
            access_token: "old".into(),
            refresh_token: "refresh".into(),
            expires_at: 1,
            ..Default::default()
        },
        Endpoints {
            pan: base.clone(),
            oauth: base.clone(),
            pcs: base,
        },
    )
    .unwrap();
    let worker_api = api.clone();
    let refresh_task = thread::spawn(move || worker_api.token().unwrap());
    arrived_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (snapshot_tx, snapshot_rx) = mpsc::channel();
    let snapshot_task = thread::spawn(move || snapshot_tx.send(api.credentials()).unwrap());
    let responsive = snapshot_rx.recv_timeout(Duration::from_millis(200)).is_ok();
    release_tx.send(()).unwrap();
    refresh_task.join().unwrap();
    server_task.join().unwrap();
    snapshot_task.join().unwrap();
    assert!(
        responsive,
        "UI credential snapshots must not wait for network refresh IO"
    );
}
