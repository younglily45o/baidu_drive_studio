mod support;
use baidu_drive_studio::{
    api::Api,
    download_plan::{prepare_directories, scan},
    model::{Credentials, RemoteFile},
};
use std::sync::atomic::{AtomicBool, Ordering};
use support::{json, response, server};

fn entry(id: u64, path: &str, directory: bool) -> RemoteFile {
    RemoteFile {
        fs_id: id,
        path: path.into(),
        server_filename: path.rsplit('/').next().unwrap().into(),
        isdir: u8::from(directory),
        size: if directory { 0 } else { 6 },
        ..Default::default()
    }
}
fn cookie() -> Credentials {
    Credentials {
        cookies: "BDUSS=folder-test".into(),
        ..Default::default()
    }
}
fn csrf() -> String {
    json(r#"{"errno":0,"result":{"bdstoken":"folder-csrf"}}"#)
}
fn listing(files: &[RemoteFile]) -> String {
    json(&serde_json::json!({"errno":0,"list":files}).to_string())
}

#[test]
fn recursive_folder_scan_preserves_subfolders_and_empty_directories() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("downloads");
    let root = entry(1, "/资料", true);
    let (api, requests, task) = server(
        vec![
            csrf(),
            listing(&[
                entry(2, "/资料/子目录", true),
                entry(3, "/资料/空目录", true),
                entry(4, "/资料/a.txt", false),
            ]),
            listing(&[entry(5, "/资料/子目录/b.txt", false)]),
            listing(&[]),
        ],
        cookie(),
    );
    let plan = scan(&api, &[root], &base, &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(plan.files.len(), 2);
    assert_eq!(plan.total_bytes, 12);
    assert!(
        plan.files
            .iter()
            .any(|f| f.target == base.join("资料/a.txt"))
    );
    assert!(
        plan.files
            .iter()
            .any(|f| f.target == base.join("资料/子目录/b.txt"))
    );
    assert!(
        !base.exists(),
        "scanning must not create files or directories"
    );
    prepare_directories(&plan, &AtomicBool::new(false)).unwrap();
    assert!(base.join("资料/空目录").is_dir());
    assert!(base.join("资料/子目录").is_dir());
    assert!(!base.join("资料/a.txt").exists());
    task.join().unwrap();
    assert_eq!(requests.lock().unwrap().len(), 4);
}

#[test]
fn recursive_plan_downloads_a_nested_file_through_the_existing_cookie_pipeline() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("downloads");
    let (api, requests, task) = server(
        vec![
            csrf(),
            listing(&[entry(2, "/root/sub", true), entry(3, "/root/empty", true)]),
            listing(&[]),
            listing(&[entry(4, "/root/sub/a.txt", false)]),
            json(
                r#"{"errno":0,"info":[{"fs_id":4,"path":"/root/sub/a.txt","server_filename":"a.txt","size":6,"isdir":0,"dlink":"{{BASE}}/cdn"}]}"#,
            ),
            response("200 OK", "", "abcdef"),
        ],
        cookie(),
    );
    let cancel = AtomicBool::new(false);
    let plan = scan(&api, &[entry(1, "/root", true)], &base, &cancel, |_| {}).unwrap();
    prepare_directories(&plan, &cancel).unwrap();
    for file in plan.files {
        let metadata = api.resolve_download(&file.remote).unwrap();
        baidu_drive_studio::transfer::download(
            &api,
            &metadata,
            &metadata.dlink,
            &file.target,
            &cancel,
            |_, _, _| {},
        )
        .unwrap();
    }
    assert_eq!(
        std::fs::read(base.join("root/sub/a.txt")).unwrap(),
        b"abcdef"
    );
    assert_eq!(std::fs::read_dir(base.join("root/sub")).unwrap().count(), 1);
    assert!(base.join("root/empty").is_dir());
    task.join().unwrap();
    assert_eq!(requests.lock().unwrap().len(), 6);
}

#[test]
fn oauth_folder_scan_uses_offsets_instead_of_cookie_page_numbers() {
    let temp = tempfile::tempdir().unwrap();
    let page: Vec<_> = (0..200)
        .map(|i| entry(i + 10, &format!("/root/f{i:03}.bin"), false))
        .collect();
    let (api, requests, task) = server(
        vec![listing(&page), listing(&[])],
        Credentials {
            access_token: "folder-test".into(),
            ..Default::default()
        },
    );
    let plan = scan(
        &api,
        &[entry(1, "/root", true)],
        temp.path(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(plan.files.len(), 200);
    task.join().unwrap();
    let requests = requests.lock().unwrap();
    let request = String::from_utf8_lossy(&requests[1]);
    assert!(request.starts_with("GET /rest/2.0/xpan/file?"));
    assert!(request.contains("start=200"));
    assert!(!request.to_lowercase().contains("cookie:"));
}

#[cfg(windows)]
#[test]
fn windows_folder_download_publishes_beyond_max_path() {
    let temp = tempfile::tempdir().unwrap();
    let mut directory = temp.path().to_path_buf();
    for _ in 0..8 {
        directory.push("long-nested-folder-name-0123456789");
    }
    std::fs::create_dir_all(&directory).unwrap();
    let target = directory.join("a.txt");
    assert!(target.as_os_str().len() > 260);
    let (api, _, task) = server(
        vec![response("200 OK", "", "abcdef")],
        Credentials {
            access_token: "folder-test".into(),
            ..Default::default()
        },
    );
    baidu_drive_studio::transfer::download(
        &api,
        &entry(1, "/root/a.txt", false),
        &api.endpoints().pan,
        &target,
        &AtomicBool::new(false),
        |_, _, _| {},
    )
    .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"abcdef");
    task.join().unwrap();
    assert!(
        baidu_drive_studio::transfer::download(
            &api,
            &entry(1, "/root/a.txt", false),
            &api.endpoints().pan,
            &target,
            &AtomicBool::new(false),
            |_, _, _| {}
        )
        .is_err()
    );
    assert_eq!(std::fs::read(target).unwrap(), b"abcdef");
}

#[test]
fn folder_scan_reads_all_pages_and_uses_the_correct_page_number() {
    let temp = tempfile::tempdir().unwrap();
    let page: Vec<_> = (0..200)
        .map(|i| entry(i + 10, &format!("/root/f{i:03}.bin"), false))
        .collect();
    let (api, requests, task) = server(
        vec![
            csrf(),
            listing(&page),
            listing(&[entry(999, "/root/last.bin", false)]),
        ],
        cookie(),
    );
    let plan = scan(
        &api,
        &[entry(1, "/root", true)],
        temp.path(),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(plan.files.len(), 201);
    task.join().unwrap();
    assert!(String::from_utf8_lossy(&requests.lock().unwrap()[2]).contains("page=2"));
}

#[test]
fn overlapping_selections_are_downloaded_once_under_the_selected_parent() {
    let temp = tempfile::tempdir().unwrap();
    let (api, _, task) = server(
        vec![csrf(), listing(&[entry(2, "/root/a.txt", false)])],
        cookie(),
    );
    let roots = [
        entry(2, "/root/a.txt", false),
        entry(1, "/root", true),
        entry(1, "/root", true),
    ];
    let plan = scan(&api, &roots, temp.path(), &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(plan.files.len(), 1);
    assert_eq!(plan.files[0].target, temp.path().join("root/a.txt"));
    task.join().unwrap();
}

#[test]
fn scan_cancellation_before_start_does_not_access_network_or_disk() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("downloads");
    let api = Api::new(Credentials::default()).unwrap();
    assert!(
        scan(
            &api,
            &[entry(1, "/root", true)],
            &base,
            &AtomicBool::new(true),
            |_| {}
        )
        .is_err()
    );
    assert!(!base.exists());
}

#[test]
fn cancellation_during_scan_does_not_return_a_partial_plan() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("downloads");
    let (api, _, task) = server(
        vec![csrf(), listing(&[entry(2, "/root/a.txt", false)])],
        cookie(),
    );
    let cancel = AtomicBool::new(false);
    assert!(
        scan(&api, &[entry(1, "/root", true)], &base, &cancel, |p| {
            if p.files > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .is_err()
    );
    assert!(!base.exists());
    task.join().unwrap();
}

#[test]
fn scan_rejects_unsafe_names_path_escape_and_duplicate_directory_ids() {
    for child in [
        entry(2, "/outside/a.txt", false),
        entry(2, "/root/NUL.txt", false),
        entry(1, "/root/loop", true),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let (api, _, task) = server(vec![csrf(), listing(&[child])], cookie());
        assert!(
            scan(
                &api,
                &[entry(1, "/root", true)],
                temp.path(),
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
        );
        task.join().unwrap();
    }
}

#[test]
fn windows_case_collisions_fail_before_any_directories_are_created() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("downloads");
    let (api, _, task) = server(
        vec![
            csrf(),
            listing(&[
                entry(2, "/root/a.txt", false),
                entry(3, "/root/A.TXT", false),
            ]),
        ],
        cookie(),
    );
    assert!(
        scan(
            &api,
            &[entry(1, "/root", true)],
            &base,
            &AtomicBool::new(false),
            |_| {}
        )
        .is_err()
    );
    assert!(!base.exists());
    task.join().unwrap();
}

#[test]
fn failed_subdirectory_request_does_not_return_a_partial_plan() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("downloads");
    let (api, _, task) = server(
        vec![
            csrf(),
            listing(&[entry(2, "/root/sub", true), entry(3, "/root/a.txt", false)]),
            json(r#"{"errno":-9}"#),
        ],
        cookie(),
    );
    assert!(
        scan(
            &api,
            &[entry(1, "/root", true)],
            &base,
            &AtomicBool::new(false),
            |_| {}
        )
        .is_err()
    );
    assert!(!base.exists());
    task.join().unwrap();
}

#[test]
fn preparing_a_folder_never_replaces_an_existing_file() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("downloads");
    std::fs::create_dir(&base).unwrap();
    std::fs::write(base.join("root"), b"user-content").unwrap();
    let (api, _, task) = server(vec![csrf(), listing(&[])], cookie());
    let plan = scan(
        &api,
        &[entry(1, "/root", true)],
        &base,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert!(prepare_directories(&plan, &AtomicBool::new(false)).is_err());
    assert_eq!(std::fs::read(base.join("root")).unwrap(), b"user-content");
    task.join().unwrap();
}

#[cfg(unix)]
#[test]
fn preparing_a_folder_rejects_child_symlinks() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().join("downloads");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, base.join("root")).unwrap();
    let (api, _, task) = server(vec![csrf(), listing(&[])], cookie());
    let plan = scan(
        &api,
        &[entry(1, "/root", true)],
        &base,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert!(prepare_directories(&plan, &AtomicBool::new(false)).is_err());
    task.join().unwrap();
}
