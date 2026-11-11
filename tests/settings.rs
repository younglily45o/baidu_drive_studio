use baidu_drive_studio::{
    config,
    model::{parent, remote_child, remote_directory},
};

#[test]
fn settings_roundtrip_contains_only_non_secret_preferences() {
    let temp = tempfile::tempdir().unwrap();
    let settings = config::Settings {
        app_key: "public-app-key".into(),
        upload_dir: "/apps/照片".into(),
        ..Default::default()
    };
    config::save_settings(temp.path(), &settings).unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("settings.json")).unwrap()).unwrap();
    for key in ["secret_key", "access_token", "refresh_token", "cookies"] {
        assert!(value.get(key).is_none());
    }
    assert_eq!(
        config::load_settings(temp.path()).unwrap().upload_dir,
        "/apps/照片"
    );
}

#[test]
fn old_oauth_credentials_load_without_a_cookie_field() {
    let old = r#"{"access_token":"legacy-access","refresh_token":"legacy-refresh","expires_at":0}"#;
    let credentials: baidu_drive_studio::model::Credentials = serde_json::from_str(old).unwrap();
    assert!(credentials.cookies.is_empty());
    assert_eq!(credentials.access_token, "legacy-access");
}

#[test]
fn remote_paths_are_absolute_and_cannot_traverse() {
    assert_eq!(remote_directory(" /apps//照片/ ").unwrap(), "/apps/照片");
    assert_eq!(remote_child("/", "中文.txt").unwrap(), "/中文.txt");
    assert_eq!(parent("/apps/照片"), "/apps");
    assert_eq!(parent("/"), "/");
    for path in ["relative", "/a/../b", "/a/./b", "/a\\b"] {
        assert!(remote_directory(path).is_err());
    }
}

#[cfg(windows)]
#[test]
fn dpapi_roundtrip_and_logout_removes_encrypted_credential_file() {
    use baidu_drive_studio::model::Credentials;
    let temp = tempfile::tempdir().unwrap();
    let credentials = Credentials {
        access_token: "sensitive-access".into(),
        secret_key: "sensitive-secret".into(),
        cookies: "BDUSS=sensitive-cookie".into(),
        ..Default::default()
    };
    config::save_credentials(temp.path(), &credentials).unwrap();
    let bytes = std::fs::read(temp.path().join("login.credentials")).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("sensitive-access"));
    assert!(!String::from_utf8_lossy(&bytes).contains("sensitive-cookie"));
    assert!(config::load_credentials(temp.path()).unwrap().unwrap() == credentials);
    config::forget_credentials(temp.path()).unwrap();
    assert!(config::load_credentials(temp.path()).unwrap().is_none());
}
