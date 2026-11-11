use baidu_drive_studio::{
    api::{Api, PAGE_SIZE},
    config::{self, Settings},
    cookies::{LinkAccess, LinkReport},
    download_plan::{self, DownloadPlan, ScanProgress},
    model::{
        Credentials, DeviceCode, Error, RemoteFile, Result, parent, remote_child, remote_directory,
        size, valid_filename,
    },
    transfer::{self, check_cancel},
};
use eframe::egui::{self, Color32, RichText, Vec2};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

const BLUE: Color32 = Color32::from_rgb(36, 103, 232);
const MUTED: Color32 = Color32::from_rgb(111, 122, 143);
const RED: Color32 = Color32::from_rgb(192, 61, 57);
const TRANSFER_PAGE_SIZE: usize = 100;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Files,
    Transfers,
    Settings,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Queued,
    Running,
    Cancelling,
    Done,
    Failed,
    Cancelled,
}
impl State {
    fn label(self) -> &'static str {
        match self {
            Self::Queued => "等待中",
            Self::Running => "传输中",
            Self::Cancelling => "正在取消",
            Self::Done => "已完成",
            Self::Failed => "失败",
            Self::Cancelled => "已取消",
        }
    }
    fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Cancelling)
    }
}
#[derive(Clone)]
enum Task {
    Download(RemoteFile, PathBuf),
    Upload(PathBuf, String),
}
struct Entry {
    id: u64,
    task: Task,
    name: String,
    state: State,
    done: u64,
    total: u64,
    stage: String,
    detail: String,
    cancel: Arc<AtomicBool>,
    started: Option<Instant>,
    rate_start: Option<(Instant, u64, String)>,
}
enum AuthEvent {
    Code(DeviceCode),
    Complete(Credentials),
    CookieComplete(Api),
    Failed(Error),
}
struct Listing {
    path: String,
    start: usize,
    files: Vec<RemoteFile>,
}
enum TransferEvent {
    Progress(u64, u64, String),
    Finished(Result<String>),
}
enum FolderEvent {
    Progress(ScanProgress),
    Finished(Result<DownloadPlan>),
}
struct FolderJob {
    receiver: Receiver<FolderEvent>,
    cancel: Arc<AtomicBool>,
}

pub struct Studio {
    settings: Settings,
    config_path: PathBuf,
    api: Option<Api>,
    saved_credentials: Option<Credentials>,
    secret_key: String,
    manual_token: String,
    cookie_input: String,
    link_job: Option<Receiver<Result<LinkReport>>>,
    link_report: Option<LinkReport>,
    page: Page,
    path: String,
    path_input: String,
    files: Vec<RemoteFile>,
    selected: HashSet<u64>,
    start: usize,
    filter: String,
    quota: Option<(u64, u64)>,
    list_job: Option<Receiver<Result<Listing>>>,
    quota_job: Option<Receiver<Result<(u64, u64)>>>,
    auth_job: Option<Receiver<AuthEvent>>,
    auth_cancel: Arc<AtomicBool>,
    device: Option<(DeviceCode, Instant)>,
    entries: Vec<Entry>,
    transfer_page: usize,
    next_id: u64,
    transfer_job: Option<(u64, Receiver<TransferEvent>)>,
    folder_job: Option<FolderJob>,
    folder_status: String,
    status: String,
    error: bool,
    closing: bool,
    allow_close: bool,
}

impl Studio {
    pub fn new(ctx: &egui::Context) -> Self {
        Self::new_in(ctx, config::config_dir())
    }
    fn new_in(ctx: &egui::Context, directory: PathBuf) -> Self {
        setup_style(ctx);
        let loaded = config::load_settings(&directory);
        let mut error_message = loaded.as_ref().err().map(ToString::to_string);
        let mut settings = loaded.unwrap_or_default();
        if !cfg!(windows) {
            settings.remember_login = false;
        }
        let credentials = if settings.remember_login {
            match config::load_credentials(&directory) {
                Ok(c) => c,
                Err(e) => {
                    error_message = Some(e.to_string());
                    None
                }
            }
        } else {
            None
        };
        let api = credentials.as_ref().and_then(|c| Api::new(c.clone()).ok());
        let mut app = Self {
            secret_key: credentials
                .as_ref()
                .map(|c| c.secret_key.clone())
                .unwrap_or_default(),
            saved_credentials: credentials,
            settings,
            config_path: directory,
            page: if api.is_some() {
                Page::Files
            } else {
                Page::Settings
            },
            api,
            manual_token: String::new(),
            cookie_input: String::new(),
            link_job: None,
            link_report: None,
            path: "/".into(),
            path_input: "/".into(),
            files: vec![],
            selected: HashSet::new(),
            start: 0,
            filter: String::new(),
            quota: None,
            list_job: None,
            quota_job: None,
            auth_job: None,
            auth_cancel: Arc::new(AtomicBool::new(false)),
            device: None,
            entries: vec![],
            transfer_page: 0,
            next_id: 0,
            transfer_job: None,
            folder_job: None,
            folder_status: String::new(),
            status: error_message
                .clone()
                .unwrap_or_else(|| "粘贴网盘首页 Cookies 登录，或展开 OAuth 使用应用授权。".into()),
            error: error_message.is_some(),
            closing: false,
            allow_close: false,
        };
        if app.api.is_some() {
            app.request_list("/".into(), 0);
        }
        app
    }
    fn status(&mut self, text: impl Into<String>, error: bool) {
        self.status = text.into();
        self.error = error;
    }
    fn busy(&self) -> bool {
        self.folder_job.is_some() || self.entries.iter().any(|e| e.state.active())
    }
    fn persist(&mut self) {
        let result = (|| {
            config::save_settings(&self.config_path, &self.settings)?;
            if self.settings.remember_login {
                if let Some(api) = &self.api {
                    let c = api.credentials();
                    config::save_credentials(&self.config_path, &c)?;
                    self.saved_credentials = Some(c);
                }
            } else {
                config::forget_credentials(&self.config_path)?;
                self.saved_credentials = None;
            }
            Ok::<_, Error>(())
        })();
        if let Err(e) = result {
            self.status(e.to_string(), true);
        }
    }
    fn request_list(&mut self, directory: String, start: usize) {
        if self.list_job.is_some() {
            return;
        }
        let Some(api) = self.api.clone() else {
            self.page = Page::Settings;
            return;
        };
        let directory = match remote_directory(&directory) {
            Ok(p) => p,
            Err(e) => {
                self.status(e.to_string(), true);
                return;
            }
        };
        if self.quota.is_none() && self.quota_job.is_none() {
            let quota_api = api.clone();
            let (tx, rx) = mpsc::channel();
            self.quota_job = Some(rx);
            std::thread::spawn(move || {
                let _ = tx.send(quota_api.quota());
            });
        }
        let (tx, rx) = mpsc::channel();
        self.list_job = Some(rx);
        self.status("正在获取网盘文件…", false);
        std::thread::spawn(move || {
            let result = api.list(&directory, start).map(|files| Listing {
                path: directory,
                start,
                files,
            });
            let _ = tx.send(result);
        });
    }
    fn login(&mut self) {
        if self.settings.app_key.trim().is_empty() || self.secret_key.trim().is_empty() {
            self.status("请填写自己应用的 AppKey 和 SecretKey。", true);
            return;
        }
        self.auth_cancel.store(true, Ordering::Relaxed);
        self.auth_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.auth_cancel.clone();
        let key = self.settings.app_key.trim().to_owned();
        let secret = self.secret_key.trim().to_owned();
        let (tx, rx) = mpsc::channel();
        self.auth_job = Some(rx);
        self.device = None;
        self.status("正在申请设备码…", false);
        std::thread::spawn(move || {
            let result = (|| {
                let api = Api::new(Credentials::default())?;
                let device = api.device_code(&key)?;
                let deadline = Instant::now() + Duration::from_secs(device.expires_in.min(3600));
                let mut interval = device.interval.clamp(2, 60);
                let _ = tx.send(AuthEvent::Code(device.clone()));
                loop {
                    let wait_until = Instant::now() + Duration::from_secs(interval);
                    while Instant::now() < wait_until {
                        check_cancel(&cancel)?;
                        if Instant::now() >= deadline {
                            return Err(Error::new("设备码已过期，请重新登录"));
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    check_cancel(&cancel)?;
                    match api.device_token(&key, &secret, &device.device_code) {
                        Ok(c) => {
                            check_cancel(&cancel)?;
                            let _ = tx.send(AuthEvent::Complete(c));
                            return Ok(());
                        }
                        Err(e) if e.code.as_deref() == Some("authorization_pending") => {}
                        Err(e) if e.code.as_deref() == Some("slow_down") => {
                            interval = (interval + 5).min(120);
                        }
                        Err(e) => return Err(e),
                    }
                }
            })();
            if let Err(e) = result {
                let _ = tx.send(AuthEvent::Failed(e));
            }
        });
    }
    fn use_token(&mut self) {
        if self.manual_token.trim().is_empty() {
            self.status("请填写 Access Token。", true);
            return;
        }
        let c = Credentials {
            app_key: self.settings.app_key.trim().into(),
            secret_key: self.secret_key.trim().into(),
            access_token: self.manual_token.trim().into(),
            ..Default::default()
        };
        self.install_login(c);
    }
    fn login_cookie(&mut self) {
        if self.api.is_some() || self.auth_job.is_some() || self.busy() {
            return;
        }
        let normalized = match baidu_drive_studio::cookies::normalize_cookie(&self.cookie_input) {
            Ok(c) => c,
            Err(e) => {
                self.status(e.to_string(), true);
                return;
            }
        };
        let credentials = Credentials {
            cookies: normalized,
            ..Default::default()
        };
        self.auth_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.auth_cancel.clone();
        let (tx, rx) = mpsc::channel();
        self.auth_job = Some(rx);
        self.status("正在验证 Cookie 会话…", false);
        std::thread::spawn(move || {
            let result = (|| {
                let api = Api::new(credentials)?;
                api.validate_cookie()?;
                check_cancel(&cancel)?;
                Ok(api)
            })();
            let _ = tx.send(match result {
                Ok(api) => AuthEvent::CookieComplete(api),
                Err(e) => AuthEvent::Failed(e),
            });
        });
    }
    fn install_login(&mut self, credentials: Credentials) {
        match Api::new(credentials) {
            Ok(api) => self.install_api(api),
            Err(e) => self.status(e.to_string(), true),
        }
    }
    fn install_api(&mut self, api: Api) {
        self.auth_cancel.store(true, Ordering::Relaxed);
        self.auth_job = None;
        self.device = None;
        self.list_job = None;
        self.quota_job = None;
        self.link_job = None;
        self.link_report = None;
        if api.is_cookie() && self.settings.upload_dir == "/apps/你的应用名称" {
            self.settings.upload_dir = "/".into();
        }
        self.api = Some(api);
        self.manual_token.clear();
        self.cookie_input.clear();
        self.quota = None;
        self.persist();
        self.page = Page::Files;
        self.request_list("/".into(), 0);
    }
    fn begin_link_probe(&mut self) {
        if self.link_job.is_some() {
            return;
        }
        let Some(api) = self.api.clone() else {
            return;
        };
        let selected = self.selected_downloads();
        if selected.len() != 1 || selected[0].isdir != 0 {
            self.status("请选择一个普通文件以获取并验证直链。", true);
            return;
        }
        let file = selected.into_iter().next().unwrap();
        let (tx, rx) = mpsc::channel();
        self.link_job = Some(rx);
        self.link_report = None;
        self.status("正在获取下载直链并验证首字节…", false);
        std::thread::spawn(move || {
            let _ = tx.send(api.inspect_direct_link(&file));
        });
    }
    fn logout(&mut self) {
        if self.busy() {
            self.status("请先取消或完成队列中的传输任务。", true);
            return;
        }
        self.auth_cancel.store(true, Ordering::Relaxed);
        self.auth_job = None;
        self.device = None;
        self.list_job = None;
        self.quota_job = None;
        // Drop receivers as well as the session so late background messages cannot log back in.
        self.api = None;
        self.saved_credentials = None;
        self.manual_token.clear();
        self.cookie_input.clear();
        self.link_job = None;
        self.link_report = None;
        self.secret_key.clear();
        self.files.clear();
        self.selected.clear();
        self.entries.clear();
        self.transfer_page = 0;
        self.folder_status.clear();
        self.quota = None;
        self.page = Page::Settings;
        match config::forget_credentials(&self.config_path) {
            Ok(()) => self.status("已退出登录，并清除保存的凭据。", false),
            Err(e) => self.status(format!("已退出当前会话；清除磁盘凭据失败：{e}"), true),
        }
    }
    fn enqueue(&mut self, task: Task, name: String, total: u64) {
        // Reject duplicate unfinished destinations, including queued tasks.
        let duplicate = self.entries.iter().any(|e| {
            e.state.active()
                && match (&e.task, &task) {
                    (Task::Download(_, a), Task::Download(_, b)) => a == b,
                    (Task::Upload(a, p), Task::Upload(b, q)) => a == b && p == q,
                    _ => false,
                }
        });
        if duplicate {
            self.status(format!("{name} 已在队列中。"), true);
            return;
        }
        self.push_entry(task, name, total);
    }
    fn push_entry(&mut self, task: Task, name: String, total: u64) {
        self.next_id += 1;
        self.entries.push(Entry {
            id: self.next_id,
            task,
            name,
            state: State::Queued,
            done: 0,
            total,
            stage: "等待中".into(),
            detail: String::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            started: None,
            rate_start: None,
        });
    }
    fn enqueue_download_batch(&mut self, files: Vec<download_plan::PlannedFile>) -> (usize, u64) {
        let mut targets: HashSet<_> = self
            .entries
            .iter()
            .filter(|e| e.state.active())
            .filter_map(|e| {
                if let Task::Download(_, target) = &e.task {
                    Some(target.clone())
                } else {
                    None
                }
            })
            .collect();
        let mut added = 0;
        let mut bytes = 0_u64;
        for file in files {
            if targets.insert(file.target.clone()) {
                let name = file.remote.server_filename.clone();
                let total = file.remote.size;
                self.push_entry(Task::Download(file.remote, file.target), name, total);
                added += 1;
                bytes = bytes.saturating_add(total);
            }
        }
        (added, bytes)
    }
    fn selected_downloads(&self) -> Vec<RemoteFile> {
        self.files
            .iter()
            .filter(|f| self.selected.contains(&f.fs_id) && self.matches(f))
            .cloned()
            .collect()
    }
    fn cancel_folder_scan(&mut self) {
        if let Some(job) = self.folder_job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            self.folder_status = "已取消目录扫描，此批次未加入下载队列。".into();
            self.status(self.folder_status.clone(), false);
        }
    }
    fn queue_downloads(&mut self) {
        let Some(api) = self.api.clone() else {
            return;
        };
        let selected = self.selected_downloads();
        if selected.is_empty() {
            return;
        }
        if selected.iter().any(|f| f.isdir != 0) {
            if self.folder_job.is_some() {
                self.status("请等待当前目录扫描完成，或在传输列表取消扫描。", true);
                return;
            }
            let base = self.settings.download_dir.clone();
            let (tx, receiver) = mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            self.folder_job = Some(FolderJob {
                receiver,
                cancel: cancel.clone(),
            });
            self.folder_status = "正在扫描所选文件夹…".into();
            self.status(self.folder_status.clone(), false);
            self.page = Page::Transfers;
            std::thread::spawn(move || {
                let result = (|| {
                    let plan = download_plan::scan(&api, &selected, &base, &cancel, |p| {
                        let _ = tx.send(FolderEvent::Progress(p));
                    })?;
                    download_plan::prepare_directories(&plan, &cancel)?;
                    check_cancel(&cancel)?;
                    Ok(plan)
                })();
                let _ = tx.send(FolderEvent::Finished(result));
            });
            return;
        }
        for file in selected {
            if !valid_filename(&file.server_filename) {
                self.status(
                    format!("文件名无法在 Windows 中保存：{}", file.server_filename),
                    true,
                );
                continue;
            }
            let target = self.settings.download_dir.join(&file.server_filename);
            self.enqueue(
                Task::Download(file.clone(), target),
                file.server_filename.clone(),
                file.size,
            );
        }
        self.page = Page::Transfers;
    }
    fn queue_uploads(&mut self) {
        if self.api.is_none() {
            self.page = Page::Settings;
            return;
        }
        let directory = match remote_directory(&self.settings.upload_dir) {
            Ok(p) => p,
            Err(e) => {
                self.status(e.to_string(), true);
                return;
            }
        };
        if let Some(files) = rfd::FileDialog::new()
            .set_title("选择要上传的文件（单文件最多 4 GiB）")
            .pick_files()
        {
            for path in files {
                let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned)
                else {
                    self.status("无法读取文件名。", true);
                    continue;
                };
                match remote_child(&directory, &name) {
                    Ok(remote) => {
                        let length = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                        self.enqueue(Task::Upload(path, remote), name, length);
                    }
                    Err(e) => self.status(e.to_string(), true),
                }
            }
            self.page = Page::Transfers;
        }
    }
    fn start_next(&mut self) {
        if self.transfer_job.is_some() {
            return;
        }
        let Some(api) = self.api.clone() else {
            return;
        };
        let Some(entry) = self.entries.iter_mut().find(|e| e.state == State::Queued) else {
            return;
        };
        entry.state = State::Running;
        entry.started = Some(Instant::now());
        entry.rate_start = None;
        let id = entry.id;
        let task = entry.task.clone();
        let cancel = entry.cancel.clone();
        let (tx, rx) = mpsc::channel();
        self.transfer_job = Some((id, rx));
        std::thread::spawn(move || {
            let mut last = Instant::now() - Duration::from_secs(1);
            let mut last_stage = String::new();
            let mut progress = |done, total, stage: &str| {
                if last.elapsed() >= Duration::from_millis(120)
                    || stage != last_stage
                    || done == total
                {
                    let _ = tx.send(TransferEvent::Progress(done, total, stage.into()));
                    last = Instant::now();
                    last_stage = stage.into();
                }
            };
            let result = (|| {
                check_cancel(&cancel)?;
                match task {
                    Task::Download(file, target) => {
                        progress(0, file.size, "获取下载信息");
                        let metadata = api.resolve_download(&file)?;
                        transfer::download(
                            &api,
                            &metadata,
                            &metadata.dlink,
                            &target,
                            &cancel,
                            &mut progress,
                        )?;
                        Ok(target.display().to_string())
                    }
                    Task::Upload(path, remote) => {
                        transfer::upload(&api, &path, &remote, &cancel, &mut progress)
                    }
                }
            })();
            let _ = tx.send(TransferEvent::Finished(result));
        });
    }
    fn poll(&mut self) {
        let (folder_events, folder_disconnected) = self
            .folder_job
            .as_ref()
            .map(|job| drain(&job.receiver))
            .unwrap_or_default();
        for event in folder_events {
            match event {
                FolderEvent::Progress(p) => {
                    self.folder_status = format!(
                        "扫描目录：{} · {} 个文件 / {} 个目录",
                        p.current, p.files, p.directories
                    );
                }
                FolderEvent::Finished(result) => {
                    self.folder_job = None;
                    match result {
                        Ok(plan) => {
                            let directories = plan.directories.len();
                            let scanned = plan.files.len();
                            let (added, bytes) = self.enqueue_download_batch(plan.files);
                            self.folder_status = format!(
                                "目录扫描完成 · 已加入 {} 个文件（{}），保留 {} 个目录；跳过 {} 个已排队文件",
                                added,
                                size(bytes),
                                directories,
                                scanned - added,
                            );
                            self.status(self.folder_status.clone(), false);
                        }
                        Err(e) => {
                            self.folder_status = format!("目录扫描失败，此批次未加入下载队列：{e}");
                            self.status(self.folder_status.clone(), true);
                        }
                    }
                }
            }
        }
        if folder_disconnected && self.folder_job.is_some() {
            self.folder_job = None;
            self.folder_status = "目录扫描线程已停止，此批次未加入下载队列，请重试。".into();
            self.status(self.folder_status.clone(), true);
        }
        if let Some(rx) = &self.list_job {
            match rx.try_recv() {
                Ok(result) => {
                    self.list_job = None;
                    match result {
                        Ok(list) => {
                            self.path = list.path;
                            self.path_input = self.path.clone();
                            self.start = list.start;
                            self.files = list.files;
                            self.selected.clear();
                            self.filter.clear();
                            self.status(
                                format!("已连接百度网盘 · 本页 {} 项", self.files.len()),
                                false,
                            );
                        }
                        Err(e) => self.status(e.to_string(), true),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.list_job = None;
                    self.status("文件查询线程已停止，请重试。", true);
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.quota_job {
            match rx.try_recv() {
                Ok(result) => {
                    self.quota = result.ok();
                    self.quota_job = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.quota_job = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let (events, auth_disconnected) = self.auth_job.as_ref().map(drain).unwrap_or_default();
        for event in events {
            match event {
                AuthEvent::Code(c) => {
                    self.device = Some((c, Instant::now()));
                    self.status("设备码已就绪，请在浏览器中完成授权。", false);
                }
                AuthEvent::Complete(c) => self.install_login(c),
                AuthEvent::CookieComplete(api) => self.install_api(api),
                AuthEvent::Failed(e) => {
                    self.auth_job = None;
                    self.device = None;
                    self.status(e.to_string(), true);
                }
            }
        }
        if auth_disconnected && self.auth_job.is_some() {
            self.auth_job = None;
            self.device = None;
            self.status("授权线程已停止，请重新登录。", true);
        }
        if let Some(rx) = &self.link_job {
            match rx.try_recv() {
                Ok(result) => {
                    self.link_job = None;
                    match result {
                        Ok(report) => {
                            self.status(
                                report.detail.clone(),
                                report.access == LinkAccess::Unavailable,
                            );
                            self.link_report = Some(report);
                        }
                        Err(e) => self.status(e.to_string(), true),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.link_job = None;
                    self.status("直链验证线程已停止，请重试。", true);
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let (transfer_events, transfer_disconnected) = self
            .transfer_job
            .as_ref()
            .map(|(_, r)| drain(r))
            .unwrap_or_default();
        let id = self.transfer_job.as_ref().map(|(id, _)| *id);
        for event in transfer_events {
            if let Some(entry) = self.entries.iter_mut().find(|e| Some(e.id) == id) {
                match event {
                    TransferEvent::Progress(done, total, stage) => {
                        if entry
                            .rate_start
                            .as_ref()
                            .is_none_or(|(_, _, s)| s != &stage)
                        {
                            entry.rate_start = Some((Instant::now(), done, stage.clone()));
                        }
                        entry.done = done;
                        entry.total = total;
                        entry.stage = stage;
                    }
                    TransferEvent::Finished(result) => {
                        match result {
                            Ok(detail) => {
                                entry.state = State::Done;
                                entry.done = entry.total;
                                entry.detail = detail;
                                entry.stage = "已完成".into();
                            }
                            Err(e) => {
                                entry.state = if entry.cancel.load(Ordering::Relaxed) {
                                    State::Cancelled
                                } else {
                                    State::Failed
                                };
                                entry.detail = e.to_string();
                            }
                        }
                        self.transfer_job = None;
                    }
                }
            }
        }
        if let Some((id, _)) = &self.transfer_job
            && transfer_disconnected
        {
            if let Some(entry) = self.entries.iter_mut().find(|e| e.id == *id) {
                entry.state = State::Failed;
                entry.detail = "传输线程已停止，请重试。".into();
            }
            self.transfer_job = None;
        }
        if self.settings.remember_login
            && let Some(api) = &self.api
            && self.saved_credentials.as_ref() != Some(&api.credentials())
        {
            self.persist();
        }
        self.start_next();
    }
    fn matches(&self, file: &RemoteFile) -> bool {
        file.server_filename
            .to_lowercase()
            .contains(&self.filter.to_lowercase())
    }
    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("sidebar")
            .exact_width(204.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(242, 246, 253))
                    .inner_margin(20),
            )
            .show(ctx, |ui| {
                ui.add_space(15.0);
                ui.label(RichText::new("Baidu Drive").size(23.0).strong().color(BLUE));
                ui.label(RichText::new("STUDIO  /  RUST").size(10.0).color(MUTED));
                ui.add_space(35.0);
                for (page, name) in [
                    (Page::Files, "全部文件"),
                    (Page::Transfers, "传输列表"),
                    (Page::Settings, "登录与设置"),
                ] {
                    let text = RichText::new(name).size(16.0).color(if self.page == page {
                        BLUE
                    } else {
                        Color32::from_rgb(64, 77, 100)
                    });
                    if ui
                        .add_sized(
                            [164.0, 42.0],
                            egui::Button::new(text)
                                .selected(self.page == page)
                                .frame(self.page == page),
                        )
                        .clicked()
                    {
                        self.page = page;
                    }
                    ui.add_space(7.0);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(
                        RichText::new(format!("第三方客户端 · v{}", env!("CARGO_PKG_VERSION")))
                            .size(11.0)
                            .color(MUTED),
                    );
                    ui.add_space(10.0);
                    if let Some((used, total)) = self.quota {
                        ui.label(
                            RichText::new(format!("{} / {}", size(used), size(total)))
                                .size(11.0)
                                .color(MUTED),
                        );
                        ui.add(
                            egui::ProgressBar::new(if total == 0 {
                                0.0
                            } else {
                                used as f32 / total as f32
                            })
                            .desired_height(5.0),
                        );
                        ui.label("网盘容量");
                    }
                    ui.label(
                        RichText::new(if self.api.is_some() {
                            "● 已加载登录凭据"
                        } else {
                            "○ 尚未登录"
                        })
                        .size(12.0)
                        .color(MUTED),
                    );
                });
            });
    }
    fn files_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("我的网盘");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.api.is_some(),
                        egui::Button::new(RichText::new("上传文件").color(Color32::WHITE))
                            .fill(BLUE),
                    )
                    .clicked()
                {
                    self.queue_uploads();
                }
                let selected = self.selected_downloads();
                let count = selected.len();
                if ui
                    .add_enabled(count > 0, egui::Button::new(format!("下载选中 ({count})")))
                    .clicked()
                {
                    self.queue_downloads();
                }
                if ui
                    .add_enabled(
                        count == 1 && selected[0].isdir == 0 && self.link_job.is_none(),
                        egui::Button::new("获取并验证直链"),
                    )
                    .clicked()
                {
                    self.begin_link_probe();
                }
                if self.link_job.is_some() {
                    ui.spinner();
                }
            });
        });
        ui.add_space(15.0);
        ui.add_enabled_ui(self.list_job.is_none() && self.api.is_some(), |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.path != "/", egui::Button::new("上级"))
                    .clicked()
                {
                    self.request_list(parent(&self.path), 0);
                }
                if ui.button("根目录").clicked() {
                    self.request_list("/".into(), 0);
                }
                let input = ui.add(
                    egui::TextEdit::singleline(&mut self.path_input)
                        .desired_width((ui.available_width() - 110.0).max(150.0)),
                );
                if ui.button("前往").clicked()
                    || (input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    self.request_list(self.path_input.clone(), 0);
                }
                if ui.button("刷新").clicked() {
                    self.request_list(self.path.clone(), self.start);
                }
            });
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("筛选当前页文件名…")
                    .desired_width(270.0),
            );
            if ui.button("全选本页").clicked() {
                self.selected = self
                    .files
                    .iter()
                    .filter(|f| self.matches(f))
                    .map(|f| f.fs_id)
                    .collect();
            }
            if ui.button("清除选择").clicked() {
                self.selected.clear();
            }
            if self.list_job.is_some() {
                ui.spinner();
            }
        });
        ui.add_space(14.0);
        ui.separator();
        if self.api.is_none() {
            ui.add_space(70.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("连接你的百度网盘").size(24.0).strong());
                ui.label("在浏览器中授权后，即可浏览文件和传输数据。");
                ui.add_space(15.0);
                if ui.button("前往登录").clicked() {
                    self.page = Page::Settings;
                }
            });
            return;
        }
        let mut navigate = None;
        let height = (ui.available_height() - 48.0).max(100.0);
        let name_width = (ui.available_width() * 0.45).clamp(220.0, 360.0);
        egui::ScrollArea::vertical()
            .id_salt("files")
            .max_height(height)
            .show(ui, |ui| {
                egui::Grid::new("files_grid")
                    .num_columns(4)
                    .spacing([18.0, 15.0])
                    .striped(true)
                    .min_col_width(40.0)
                    .show(ui, |ui| {
                        ui.label("");
                        filename_cell(ui, name_width, RichText::new("文件名").color(MUTED), false);
                        ui.label(RichText::new("大小").color(MUTED));
                        ui.label(RichText::new("类型").color(MUTED));
                        ui.end_row();
                        let needle = self.filter.to_lowercase();
                        for file in &self.files {
                            if !file.server_filename.to_lowercase().contains(&needle) {
                                continue;
                            }
                            let mut selected = self.selected.contains(&file.fs_id);
                            if ui
                                .add(egui::Checkbox::without_text(&mut selected))
                                .changed()
                            {
                                if selected {
                                    self.selected.insert(file.fs_id);
                                } else {
                                    self.selected.remove(&file.fs_id);
                                }
                            }
                            let name = if file.isdir != 0 {
                                format!("▸  {}", file.server_filename)
                            } else {
                                file.server_filename.clone()
                            };
                            let response = filename_cell(
                                ui,
                                name_width,
                                RichText::new(name).color(if file.isdir != 0 {
                                    BLUE
                                } else {
                                    Color32::from_rgb(43, 54, 73)
                                }),
                                true,
                            );
                            if response.clicked() && file.isdir != 0 {
                                navigate = Some(file.path.clone());
                            }
                            response.on_hover_text(&file.path);
                            ui.label(if file.isdir != 0 {
                                "—".into()
                            } else {
                                size(file.size)
                            });
                            ui.label(if file.isdir != 0 {
                                "文件夹"
                            } else {
                                "文件"
                            });
                            ui.end_row();
                        }
                    });
                if self.files.is_empty() && self.list_job.is_none() {
                    ui.add_space(35.0);
                    ui.label(RichText::new("当前目录没有文件，或本页已到末尾。").color(MUTED));
                }
            });
        if let Some(path) = navigate {
            self.request_list(path, 0);
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "第 {} 页 · 每页最多 {} 项",
                    self.start / PAGE_SIZE + 1,
                    PAGE_SIZE
                ))
                .color(MUTED),
            );
            ui.add_enabled_ui(self.list_job.is_none(), |ui| {
                if ui
                    .add_enabled(self.start > 0, egui::Button::new("上一页"))
                    .clicked()
                {
                    self.request_list(self.path.clone(), self.start.saturating_sub(PAGE_SIZE));
                }
                if ui
                    .add_enabled(self.files.len() == PAGE_SIZE, egui::Button::new("下一页"))
                    .clicked()
                {
                    self.request_list(self.path.clone(), self.start + PAGE_SIZE);
                }
            });
        });
    }
    fn transfers_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("传输列表");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("清除已结束").clicked() {
                    self.entries.retain(|e| e.state.active());
                }
                if ui.button("取消全部").clicked() {
                    self.cancel_folder_scan();
                    for e in &mut self.entries {
                        cancel_entry(e);
                    }
                }
                if ui.button("打开下载目录").clicked()
                    && let Err(e) = open_path(&self.settings.download_dir)
                {
                    self.status(e.to_string(), true);
                }
            });
        });
        ui.add_space(8.0);
        ui.label(
            RichText::new("任务按顺序执行。下载取消后保留断点，重试会重新获取下载链接。")
                .color(MUTED),
        );
        ui.add_space(18.0);
        if !self.folder_status.is_empty() {
            ui.horizontal_wrapped(|ui| {
                if self.folder_job.is_some() {
                    ui.spinner();
                }
                ui.label(&self.folder_status);
                if self.folder_job.is_some() && ui.button("取消目录扫描").clicked() {
                    self.cancel_folder_scan();
                }
            });
            ui.add_space(12.0);
        }
        if self.entries.is_empty() && self.folder_job.is_none() {
            ui.add_space(50.0);
            ui.label("还没有传输任务，在“全部文件”中选择文件或文件夹下载，或上传本地文件。");
        }
        let pages = self.entries.len().div_ceil(TRANSFER_PAGE_SIZE).max(1);
        self.transfer_page = self.transfer_page.min(pages - 1);
        if pages > 1 {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.transfer_page > 0, egui::Button::new("上一页任务"))
                    .clicked()
                {
                    self.transfer_page -= 1;
                }
                ui.label(format!(
                    "第 {} / {} 页 · 共 {} 个任务",
                    self.transfer_page + 1,
                    pages,
                    self.entries.len()
                ));
                if ui
                    .add_enabled(
                        self.transfer_page + 1 < pages,
                        egui::Button::new("下一页任务"),
                    )
                    .clicked()
                {
                    self.transfer_page += 1;
                }
            });
            ui.add_space(8.0);
        }
        egui::ScrollArea::vertical()
            .id_salt(("transfers", self.transfer_page))
            .show(ui, |ui| {
                for entry in self
                    .entries
                    .iter_mut()
                    .skip(self.transfer_page * TRANSFER_PAGE_SIZE)
                    .take(TRANSFER_PAGE_SIZE)
                {
                    ui.push_id(entry.id, |ui| {
                        egui::Frame::group(ui.style())
                            .fill(Color32::WHITE)
                            .inner_margin(16)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(match entry.task {
                                            Task::Download(..) => "↓",
                                            Task::Upload(..) => "↑",
                                        })
                                        .size(23.0)
                                        .color(BLUE),
                                    );
                                    ui.label(RichText::new(&entry.name).size(16.0).strong());
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if matches!(entry.state, State::Queued | State::Running)
                                                && ui.button("取消").clicked()
                                            {
                                                cancel_entry(entry);
                                            }
                                            if matches!(
                                                entry.state,
                                                State::Failed | State::Cancelled
                                            ) && ui.button("重试").clicked()
                                            {
                                                entry.state = State::Queued;
                                                entry.done = 0;
                                                entry.stage = "等待中".into();
                                                entry.detail.clear();
                                                entry.cancel = Arc::new(AtomicBool::new(false));
                                            }
                                            ui.label(RichText::new(entry.state.label()).color(
                                                if entry.state == State::Failed {
                                                    RED
                                                } else {
                                                    MUTED
                                                },
                                            ));
                                        },
                                    );
                                });
                                ui.add_space(6.0);
                                ui.add(
                                    egui::ProgressBar::new(if entry.state == State::Done {
                                        1.0
                                    } else if entry.total == 0 {
                                        0.0
                                    } else {
                                        (entry.done as f32 / entry.total as f32).clamp(0.0, 1.0)
                                    })
                                    .desired_height(6.0),
                                );
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} · {} / {}",
                                            entry.stage,
                                            size(entry.done),
                                            size(entry.total)
                                        ))
                                        .size(12.0)
                                        .color(MUTED),
                                    );
                                    if entry.state == State::Running
                                        && let Some((at, bytes, _)) = &entry.rate_start
                                    {
                                        let speed = entry.done.saturating_sub(*bytes) as f64
                                            / at.elapsed().as_secs_f64().max(0.1);
                                        ui.label(
                                            RichText::new(format!("{} /s", size(speed as u64)))
                                                .size(12.0)
                                                .color(MUTED),
                                        );
                                    }
                                });
                                if !entry.detail.is_empty() {
                                    ui.label(RichText::new(&entry.detail).size(12.0).color(
                                        if entry.state == State::Failed {
                                            RED
                                        } else {
                                            MUTED
                                        },
                                    ));
                                }
                                ui.collapsing("路径详情", |ui| match &entry.task {
                                    Task::Download(remote, target) => {
                                        ui.label(format!("网盘：{}", remote.path));
                                        ui.label(format!("本地：{}", target.display()));
                                    }
                                    Task::Upload(path, remote) => {
                                        ui.label(format!("本地：{}", path.display()));
                                        ui.label(format!("网盘：{remote}"));
                                    }
                                });
                            });
                    });
                    ui.add_space(10.0);
                }
            });
    }
    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("登录与设置");
        ui.add_space(8.0);
        ui.label(RichText::new("导入浏览器 Cookies，或使用 OAuth 授权连接你的网盘。").color(MUTED));
        ui.add_space(18.0);
        egui::ScrollArea::vertical().id_salt("settings").show(ui, |ui| {
            ui.label(RichText::new("Cookies 登录").size(17.0).strong());
            ui.label("在浏览器登录 pan.baidu.com，打开开发者工具 → 网络，复制请求头中完整的 Cookie 值。");
            ui.label(RichText::new("支持 Cookie: 前缀。只在本机粘贴；此方式不需要 AppKey / SecretKey。").size(12.0).color(MUTED));
            ui.add_enabled(self.api.is_none() && self.auth_job.is_none(), egui::TextEdit::singleline(&mut self.cookie_input).password(true).hint_text("BDUSS=…; STOKEN=…; BAIDUID=…").desired_width(590.0));
            ui.horizontal(|ui| {
                if ui.add_enabled(self.api.is_none() && self.auth_job.is_none() && !self.busy(), egui::Button::new(RichText::new("使用 Cookies 登录").color(Color32::WHITE)).fill(BLUE)).clicked() { self.login_cookie(); }
                if let Some(api) = &self.api {
                    ui.label(if api.is_cookie() { "当前：Cookie 会话" } else { "当前：OAuth 授权" });
                    if ui.add_enabled(!self.busy(), egui::Button::new("退出登录")).clicked() { self.logout(); }
                }
                if self.auth_job.is_some() {
                    ui.spinner();
                    if ui.button("取消登录").clicked() { self.auth_cancel.store(true, Ordering::Relaxed); self.auth_job = None; self.device = None; self.status("已取消登录。", false); }
                }
            });
            ui.add_space(16.0);
            ui.collapsing("OAuth 授权 / Access Token（可选）", |ui| {
            ui.label(RichText::new("开放平台应用").size(17.0).strong());
            ui.add_space(8.0);
            ui.label("使用自己在百度网盘开放平台创建的应用。SecretKey 仅用于本机授权，不要分发含密钥的安装包。");
            ui.hyperlink_to("打开百度网盘开放平台", "https://pan.baidu.com/union");
            ui.add_space(10.0);
            egui::Grid::new("auth_settings").num_columns(2).spacing([18.0, 12.0]).show(ui, |ui| {
                ui.label("AppKey / client_id"); ui.add(egui::TextEdit::singleline(&mut self.settings.app_key).desired_width(400.0)); ui.end_row();
                ui.label("SecretKey / client_secret"); ui.add(egui::TextEdit::singleline(&mut self.secret_key).password(true).desired_width(400.0)); ui.end_row();
            });
            ui.add_space(12.0);
            let can_login = self.api.is_none() && !self.busy();
            ui.horizontal(|ui| {
                if ui.add_enabled(can_login && self.auth_job.is_none(), egui::Button::new(RichText::new("浏览器授权登录").color(Color32::WHITE)).fill(BLUE)).clicked() { self.login(); }
                if self.auth_job.is_some() && ui.button("取消授权").clicked() {
                    self.auth_cancel.store(true, Ordering::Relaxed); self.auth_job = None; self.device = None; self.status("已取消授权。", false);
                }
                if self.api.is_some() && ui.add_enabled(!self.busy(), egui::Button::new("退出登录")).clicked() { self.logout(); }
            });
            if let Some((device, started)) = &self.device {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label("用户授权码：");
                    ui.label(RichText::new(&device.user_code).size(25.0).color(BLUE).strong());
                    if ui.button("复制").clicked() { ui.ctx().copy_text(device.user_code.clone()); }
                    // Use a fixed official browser destination rather than trusting a returned URL.
                    if ui.button("打开授权页面").clicked() { let _ = open_url("https://openapi.baidu.com/device"); }
                });
                ui.label(format!("在百度页面输入此授权码。剩余 {} 秒；授权后会自动登录。", device.expires_in.saturating_sub(started.elapsed().as_secs())));
                ui.spinner();
            }
            ui.add_space(14.0);
            ui.collapsing("使用已有 Access Token", |ui| {
                ui.label("适用于已完成 OAuth 的用户；此方式没有 Refresh Token，过期后需重新填写。");
                ui.add(egui::TextEdit::singleline(&mut self.manual_token).password(true).hint_text("Access Token").desired_width(480.0));
                if ui.add_enabled(can_login && self.auth_job.is_none(), egui::Button::new("连接网盘")).clicked() { self.use_token(); }
            });
            });
            ui.add_space(22.0); ui.separator(); ui.add_space(14.0);
            ui.label(RichText::new("传输设置").size(17.0).strong());
            ui.add_space(12.0);
            ui.label("下载到本地目录");
            ui.horizontal(|ui| {
                ui.label(self.settings.download_dir.display().to_string());
                if ui.button("选择目录").clicked() && let Some(p) = rfd::FileDialog::new().pick_folder() { self.settings.download_dir = p; }
            });
            ui.add_space(12.0);
            ui.label("网盘上传目录（需要已存在，并具有写入权限）");
            ui.add(egui::TextEdit::singleline(&mut self.settings.upload_dir).desired_width(480.0));
            ui.label(RichText::new("Cookie 可使用 / 或其它目录；OAuth 受应用授权范围限制。重名自动改名。").size(12.0).color(MUTED));
            if self.api.is_some() && ui.button(format!("使用当前浏览目录：{}", self.path)).clicked() { self.settings.upload_dir = self.path.clone(); }
            ui.add_space(16.0);
            let remember_changed = ui.add_enabled(cfg!(windows), egui::Checkbox::new(&mut self.settings.remember_login, "记住登录（使用 Windows DPAPI 加密，仅当前系统用户可解密）")).changed();
            if remember_changed { self.persist(); }
            if !cfg!(windows) { ui.label(RichText::new("Linux 开发版仅保留当前会话凭据。").size(12.0).color(MUTED)); }
            ui.add_space(12.0);
            if ui.button("保存设置").clicked() { self.status("设置已保存。", false); self.persist(); }
            ui.add_space(20.0);
            ui.label(RichText::new("权限和速度取决于当前账号与接口；Cookie 到期时需重新导入。").size(12.0).color(MUTED));
            ui.label(RichText::new("支持文件和文件夹递归下载、普通文件上传；不含文件夹上传或分享链接转存。").size(12.0).color(MUTED));
        });
    }
    fn link_window(&mut self, ctx: &egui::Context) {
        let mut open = self.link_report.is_some();
        if let Some(report) = &mut self.link_report {
            egui::Window::new("下载直链验证结果").open(&mut open).resizable(true).default_width(620.0).show(ctx, |ui| {
                let (title, color) = match report.access {
                    LinkAccess::Standalone => ("链接与请求头可用，无需 Cookie", BLUE),
                    LinkAccess::SessionRequired => ("需要当前 Cookie 会话", MUTED),
                    LinkAccess::Unavailable => ("未通过下载验证", RED),
                };
                ui.label(RichText::new(title).strong().size(18.0).color(color));
                ui.label(&report.detail);
                ui.add_space(8.0);
                ui.add(egui::TextEdit::multiline(&mut report.url).desired_width(590.0).desired_rows(4).interactive(false));
                ui.label(format!("User-Agent: {}", report.user_agent));
                ui.label(format!("Referer: {}", report.referer));
                ui.horizontal(|ui| {
                    if ui.button("复制 URL").clicked() { ctx.copy_text(report.url.clone()); }
                    if ui.button("复制 UA / Referer").clicked() { ctx.copy_text(format!("User-Agent: {}\nReferer: {}", report.user_agent, report.referer)); }
                });
                ui.label(RichText::new("这是临时链接，本次只验证文件首字节。完整下载、有效期和速度仍取决于服务器响应。").size(12.0).color(MUTED));
                if report.access == LinkAccess::SessionRequired { ui.label("外部工具需自行配置当前 Cookie；程序不会把 Cookie 复制到结果中。"); }
            });
        }
        if !open {
            self.link_report = None;
        }
    }
}

fn filename_cell(ui: &mut egui::Ui, width: f32, text: RichText, clickable: bool) -> egui::Response {
    ui.allocate_ui_with_layout(
        Vec2::new(width, 23.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            let label = egui::Label::new(text).halign(egui::Align::Min).truncate();
            ui.add(if clickable {
                label.sense(egui::Sense::click()).selectable(false)
            } else {
                label
            })
        },
    )
    .inner
}

fn drain<T>(receiver: &Receiver<T>) -> (Vec<T>, bool) {
    let mut messages = vec![];
    loop {
        match receiver.try_recv() {
            Ok(message) => messages.push(message),
            Err(mpsc::TryRecvError::Empty) => return (messages, false),
            Err(mpsc::TryRecvError::Disconnected) => return (messages, true),
        }
    }
}

fn cancel_entry(entry: &mut Entry) {
    if entry.state.active() {
        entry.cancel.store(true, Ordering::Relaxed);
        entry.state = if entry.state == State::Queued {
            State::Cancelled
        } else {
            State::Cancelling
        };
    }
}

impl eframe::App for Studio {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.render(ctx);
    }
}
impl Studio {
    fn render(&mut self, ctx: &egui::Context) {
        self.poll();
        if ctx.input(|i| i.viewport().close_requested()) && self.busy() && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = true;
        }
        self.sidebar(ctx);
        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::new().fill(Color32::WHITE).inner_margin(12))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&self.status).size(12.0).color(if self.error {
                        RED
                    } else {
                        MUTED
                    }));
                });
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(251, 252, 254))
                    .inner_margin(26),
            )
            .show(ctx, |ui| match self.page {
                Page::Files => self.files_ui(ui),
                Page::Transfers => self.transfers_ui(ui),
                Page::Settings => self.settings_ui(ui),
            });
        self.link_window(ctx);
        if self.closing {
            egui::Window::new("退出应用")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("还有未完成的传输。退出后队列不保存，下载临时文件会保留。");
                    ui.label("上传合并请求一旦发出，退出不能撤销云端结果。");
                    ui.horizontal(|ui| {
                        if ui.button("继续传输").clicked() {
                            self.closing = false;
                        }
                        if ui.button("退出应用").clicked() {
                            self.allow_close = true;
                            self.cancel_folder_scan();
                            for e in &mut self.entries {
                                e.cancel.store(true, Ordering::Relaxed);
                            }
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
        }
        ctx.request_repaint_after(Duration::from_millis(
            if self.busy()
                || self.auth_job.is_some()
                || self.list_job.is_some()
                || self.link_job.is_some()
            {
                100
            } else {
                1000
            },
        ));
    }
}
impl Drop for Studio {
    fn drop(&mut self) {
        self.auth_cancel.store(true, Ordering::Relaxed);
        if let Some(job) = &self.folder_job {
            job.cancel.store(true, Ordering::Relaxed);
        }
        for e in &self.entries {
            e.cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn setup_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "cjk".into(),
        egui::FontData::from_static(include_bytes!("../assets/NotoSansCJK-Regular.ttc")).into(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "cjk".into());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("cjk".into());
    ctx.set_fonts(fonts);
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.selection.bg_fill = Color32::from_rgb(217, 230, 255);
    style.visuals.selection.stroke.color = BLUE;
    style.spacing.item_spacing = Vec2::new(10.0, 9.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(25.0));
    ctx.set_style(style);
}

fn open_url(url: &str) -> Result<()> {
    spawn_open(std::ffi::OsStr::new(url))
}
fn open_path(path: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    spawn_open(path.as_os_str())
}
fn spawn_open(target: &std::ffi::OsStr) -> Result<()> {
    #[cfg(windows)]
    let program = "explorer.exe";
    #[cfg(not(windows))]
    let program = "xdg-open";
    std::process::Command::new(program)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn folder_plan(base: PathBuf) -> DownloadPlan {
        DownloadPlan {
            files: vec![download_plan::PlannedFile {
                remote: RemoteFile {
                    fs_id: 2,
                    path: "/root/sub/a.txt".into(),
                    server_filename: "a.txt".into(),
                    size: 6,
                    ..Default::default()
                },
                target: base.join("root/sub/a.txt"),
            }],
            directories: vec![
                base.join("root"),
                base.join("root/sub"),
                base.join("root/empty"),
            ],
            total_bytes: 6,
            base,
        }
    }
    #[test]
    fn folder_scan_completion_queues_nested_targets_before_disconnect() {
        let ctx = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&ctx, temp.path().to_owned());
        let (tx, receiver) = mpsc::channel();
        app.folder_job = Some(FolderJob {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        assert!(app.busy());
        tx.send(FolderEvent::Finished(Ok(folder_plan(temp.path().into()))))
            .unwrap();
        drop(tx);
        app.poll();
        assert!(app.folder_job.is_none());
        assert_eq!(app.entries.len(), 1);
        assert!(
            matches!(&app.entries[0].task, Task::Download(f, path) if f.fs_id == 2 && *path == temp.path().join("root/sub/a.txt"))
        );
        assert!(app.folder_status.contains("保留 3 个目录"));
        assert!(!app.error);
    }
    #[test]
    fn folder_batch_skips_active_targets_and_counts_only_added_bytes() {
        let ctx = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&ctx, temp.path().to_owned());
        let (count, bytes) = app.enqueue_download_batch(folder_plan(temp.path().into()).files);
        assert_eq!((count, bytes), (1, 6));
        let (count, bytes) = app.enqueue_download_batch(folder_plan(temp.path().into()).files);
        assert_eq!((count, bytes), (0, 0));
        assert_eq!(app.entries.len(), 1);
        app.entries[0].state = State::Failed;
        let (count, bytes) = app.enqueue_download_batch(folder_plan(temp.path().into()).files);
        assert_eq!((count, bytes), (1, 6));
    }
    #[test]
    fn large_folder_queue_draws_only_the_selected_transfer_page() {
        let ctx = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&ctx, temp.path().to_owned());
        let files = (0..1001)
            .map(|i| {
                let name = format!("bulk-{i:04}.bin");
                download_plan::PlannedFile {
                    remote: RemoteFile {
                        fs_id: i + 1,
                        path: format!("/root/{name}"),
                        server_filename: name.clone(),
                        size: 6,
                        ..Default::default()
                    },
                    target: temp.path().join("root").join(name),
                }
            })
            .collect();
        assert_eq!(app.enqueue_download_batch(files), (1001, 6006));
        app.page = Page::Transfers;
        fn contains(shape: &egui::epaint::Shape, name: &str) -> bool {
            match shape {
                egui::epaint::Shape::Text(t) => t.galley.job.text == name,
                egui::epaint::Shape::Vec(shapes) => shapes.iter().any(|s| contains(s, name)),
                _ => false,
            }
        }
        for (page, visible, hidden) in [
            (0, "bulk-0000.bin", "bulk-0100.bin"),
            (1, "bulk-0100.bin", "bulk-0000.bin"),
        ] {
            app.transfer_page = page;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1180.0, 790.0),
                    )),
                    ..Default::default()
                },
                |ctx| app.render(ctx),
            );
            assert!(output.shapes.iter().any(|s| contains(&s.shape, visible)));
            assert!(!output.shapes.iter().any(|s| contains(&s.shape, hidden)));
        }
        assert_eq!(
            app.entries.len(),
            1001,
            "pagination must preserve the complete queue"
        );
    }
    #[test]
    fn cancelling_folder_scan_discards_late_plan_and_unblocks_logout() {
        let ctx = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&ctx, temp.path().to_owned());
        app.api = Some(
            Api::new(Credentials {
                access_token: "test".into(),
                ..Default::default()
            })
            .unwrap(),
        );
        let (tx, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        app.folder_job = Some(FolderJob {
            receiver,
            cancel: cancel.clone(),
        });
        tx.send(FolderEvent::Finished(Ok(folder_plan(temp.path().into()))))
            .unwrap();
        app.logout();
        assert!(
            app.api.is_some(),
            "scanning must block logout just like a transfer"
        );
        app.cancel_folder_scan();
        assert!(cancel.load(Ordering::Relaxed));
        app.poll();
        assert!(app.entries.is_empty());
        assert!(!app.busy());
        app.logout();
        assert!(app.api.is_none());
    }
    #[test]
    fn folder_scan_failure_never_queues_a_partial_batch() {
        let ctx = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&ctx, temp.path().to_owned());
        let (tx, receiver) = mpsc::channel();
        app.folder_job = Some(FolderJob {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        tx.send(FolderEvent::Progress(ScanProgress {
            files: 25,
            directories: 3,
            current: "/root/sub".into(),
        }))
        .unwrap();
        tx.send(FolderEvent::Finished(Err(Error::new("list failed"))))
            .unwrap();
        drop(tx);
        app.poll();
        assert!(app.error);
        assert!(!app.busy());
        assert!(app.entries.is_empty());
        assert!(app.folder_status.contains("list failed"));
    }
    #[test]
    fn mixed_file_folder_selection_can_download_but_cannot_probe_a_direct_link() {
        let ctx = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&ctx, temp.path().to_owned());
        app.api = Some(
            Api::new(Credentials {
                access_token: "test".into(),
                ..Default::default()
            })
            .unwrap(),
        );
        app.files = vec![
            RemoteFile {
                fs_id: 1,
                path: "/root".into(),
                server_filename: "root".into(),
                isdir: 1,
                ..Default::default()
            },
            RemoteFile {
                fs_id: 2,
                path: "/a.txt".into(),
                server_filename: "a.txt".into(),
                ..Default::default()
            },
        ];
        app.selected.extend([1, 2]);
        assert_eq!(app.selected_downloads().len(), 2);
        app.begin_link_probe();
        assert!(app.link_job.is_none());
        assert!(app.error);
        app.filter = "root".into();
        assert_eq!(app.selected_downloads()[0].isdir, 1);
        app.begin_link_probe();
        assert!(app.link_job.is_none());
    }
    #[test]
    fn folder_checkbox_accepts_mouse_selection_without_navigating() {
        let ctx = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&ctx, temp.path().to_owned());
        app.api = Some(
            Api::new(Credentials {
                access_token: "test".into(),
                ..Default::default()
            })
            .unwrap(),
        );
        app.page = Page::Files;
        app.files = vec![RemoteFile {
            fs_id: 1,
            path: "/folder".into(),
            server_filename: "folder".into(),
            isdir: 1,
            ..Default::default()
        }];
        let input = |events| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1180.0, 790.0),
            )),
            events,
            ..Default::default()
        };
        let mut shapes = vec![];
        for _ in 0..3 {
            shapes = ctx.run(input(vec![]), |ctx| app.render(ctx)).shapes;
        }
        fn collect(
            shape: &egui::epaint::Shape,
            rectangles: &mut Vec<egui::Rect>,
            name: &mut Option<egui::Rect>,
        ) {
            match shape {
                egui::epaint::Shape::Rect(r) => rectangles.push(r.rect),
                egui::epaint::Shape::Text(t) if t.galley.job.text == "▸  folder" => {
                    *name = Some(t.galley.rect.translate(t.pos.to_vec2()))
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, rectangles, name);
                    }
                }
                _ => {}
            }
        }
        let (mut rectangles, mut name) = (vec![], None);
        for s in &shapes {
            collect(&s.shape, &mut rectangles, &mut name);
        }
        let name = name.expect("folder text must render");
        let pos = rectangles
            .into_iter()
            .find(|r| {
                r.width() < 25.0
                    && (r.width() - r.height()).abs() < 1.0
                    && r.right() < name.left()
                    && (r.center().y - name.center().y).abs() < 4.0
            })
            .expect("folder checkbox must render")
            .center();
        let _ = ctx.run(input(vec![egui::Event::PointerMoved(pos)]), |ctx| {
            app.render(ctx)
        });
        for pressed in [true, false] {
            let _ = ctx.run(
                input(vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }]),
                |ctx| app.render(ctx),
            );
        }
        assert!(app.selected.contains(&1));
        assert_eq!(app.selected_downloads().len(), 1);
        assert_eq!(app.path, "/");
        assert!(app.list_job.is_none());
    }
    #[test]
    fn file_and_folder_names_are_left_aligned_under_the_filename_header() {
        fn text_left(shapes: &[egui::epaint::ClippedShape], wanted: &str) -> f32 {
            fn find(shape: &egui::epaint::Shape, wanted: &str) -> Option<f32> {
                match shape {
                    egui::epaint::Shape::Text(t) if t.galley.job.text == wanted => {
                        Some(t.pos.x + t.galley.rect.min.x)
                    }
                    egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|s| find(s, wanted)),
                    _ => None,
                }
            }
            shapes
                .iter()
                .find_map(|s| find(&s.shape, wanted))
                .unwrap_or_else(|| panic!("missing rendered text: {wanted}"))
        }
        let context = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&context, temp.path().to_owned());
        app.api = Some(
            Api::new(Credentials {
                access_token: "test".into(),
                ..Default::default()
            })
            .unwrap(),
        );
        app.page = Page::Files;
        app.files = vec![
            RemoteFile {
                fs_id: 1,
                server_filename: "示例目录".into(),
                path: "/示例目录".into(),
                isdir: 1,
                ..Default::default()
            },
            RemoteFile {
                fs_id: 2,
                server_filename: "demo.bin".into(),
                path: "/demo.bin".into(),
                size: 7_000_000_000,
                ..Default::default()
            },
        ];
        for width in [960.0, 1180.0, 1600.0] {
            let mut shapes = vec![];
            for _ in 0..3 {
                shapes = context
                    .run(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                Vec2::new(width, 790.0),
                            )),
                            ..Default::default()
                        },
                        |ctx| app.render(ctx),
                    )
                    .shapes;
            }
            let header = text_left(&shapes, "文件名");
            for name in ["▸  示例目录", "demo.bin"] {
                let left = text_left(&shapes, name);
                assert!(
                    (left - header).abs() < 1.0,
                    "{width}: {name} left {left}, header {header}"
                );
                assert!(
                    left < width * 0.5 - 20.0,
                    "filename column should stay on the left"
                );
            }
        }
    }
    #[test]
    fn malformed_cookie_never_starts_login_or_saves_credentials() {
        let context = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&context, temp.path().to_owned());
        app.cookie_input = "STOKEN=private-input".into();
        app.login_cookie();
        assert!(app.api.is_none());
        assert!(app.auth_job.is_none());
        assert!(app.error);
        assert!(!app.status.contains("private-input"));
        assert!(!temp.path().join("login.credentials").exists());
    }
    #[test]
    fn logout_discards_late_cookie_login_and_direct_link_results() {
        let context = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&context, temp.path().to_owned());
        let (tx, rx) = mpsc::channel();
        app.auth_job = Some(rx);
        let api = Api::new(Credentials {
            cookies: "BDUSS=test-cookie".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(tx.send(AuthEvent::CookieComplete(api)).is_ok());
        let (tx, rx) = mpsc::channel();
        app.link_job = Some(rx);
        assert!(
            tx.send(Ok(LinkReport {
                url: "https://d.pcs.baidu.com/a?sign=private-link".into(),
                user_agent: "pan.baidu.com".into(),
                referer: "https://pan.baidu.com/disk/home".into(),
                access: LinkAccess::Standalone,
                detail: "首字节已验证".into()
            }))
            .is_ok()
        );
        app.cookie_input = "BDUSS=private-input".into();
        app.logout();
        app.poll();
        assert!(app.api.is_none());
        assert!(app.link_job.is_none());
        assert!(app.link_report.is_none());
        assert!(app.cookie_input.is_empty());
    }
    #[test]
    fn direct_link_results_render_at_minimum_window_size_for_each_outcome() {
        let context = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&context, temp.path().to_owned());
        for access in [
            LinkAccess::Standalone,
            LinkAccess::SessionRequired,
            LinkAccess::Unavailable,
        ] {
            app.link_report = Some(LinkReport {
                url: "https://d.pcs.baidu.com/a?sign=test".into(),
                user_agent: "pan.baidu.com".into(),
                referer: "https://pan.baidu.com/disk/home".into(),
                access,
                detail: "测试结果".into(),
            });
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(960.0, 650.0),
                    )),
                    ..Default::default()
                },
                |ctx| app.render(ctx),
            );
            assert!(!output.shapes.is_empty());
        }
    }
    #[test]
    fn all_pages_render_without_a_window_or_credentials() {
        let context = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&context, temp.path().to_owned());
        for page in [Page::Settings, Page::Files, Page::Transfers] {
            app.page = page;
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1180.0, 790.0),
                    )),
                    ..Default::default()
                },
                |ctx| app.render(ctx),
            );
            assert!(!output.shapes.is_empty());
        }
        assert!(!temp.path().join("login.credentials").exists());
    }
    #[test]
    fn logout_discards_pending_auth_and_file_messages() {
        let context = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&context, temp.path().to_owned());
        let (tx, rx) = mpsc::channel();
        app.auth_job = Some(rx);
        tx.send(AuthEvent::Complete(Credentials {
            access_token: "old-session".into(),
            ..Default::default()
        }))
        .unwrap();
        app.logout();
        app.poll();
        assert!(app.api.is_none());
        assert!(app.auth_job.is_none());
    }
    #[test]
    fn cancelling_a_queued_job_prevents_it_starting() {
        let context = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&context, temp.path().to_owned());
        app.enqueue(
            Task::Upload("a.txt".into(), "/apps/demo/a.txt".into()),
            "a.txt".into(),
            5,
        );
        cancel_entry(&mut app.entries[0]);
        assert_eq!(app.entries[0].state.label(), "已取消");
        assert!(!app.busy());
        app.start_next();
        assert!(app.transfer_job.is_none());
    }
    #[test]
    fn finished_transfer_is_consumed_before_channel_disconnect() {
        let context = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&context, temp.path().to_owned());
        app.enqueue(
            Task::Upload("a.txt".into(), "/apps/demo/a.txt".into()),
            "a.txt".into(),
            5,
        );
        app.entries[0].state = State::Running;
        let (tx, rx) = mpsc::channel();
        app.transfer_job = Some((app.entries[0].id, rx));
        tx.send(TransferEvent::Progress(5, 5, "上传完成".into()))
            .unwrap();
        tx.send(TransferEvent::Finished(Ok("/apps/demo/a.txt".into())))
            .unwrap();
        drop(tx);
        app.poll();
        assert_eq!(app.entries[0].state.label(), "已完成");
        assert_eq!(app.entries[0].done, 5);
        assert!(app.transfer_job.is_none());
    }

    #[test]
    fn pending_quota_does_not_delay_received_file_list() {
        let context = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&context, temp.path().to_owned());
        let (list_tx, list_rx) = mpsc::channel();
        let (_quota_tx, quota_rx) = mpsc::channel();
        app.list_job = Some(list_rx);
        app.quota_job = Some(quota_rx);
        list_tx
            .send(Ok(Listing {
                path: "/资料".into(),
                start: 0,
                files: vec![RemoteFile {
                    fs_id: 1,
                    server_filename: "中文.txt".into(),
                    ..Default::default()
                }],
            }))
            .unwrap();
        app.poll();
        assert_eq!(app.path, "/资料");
        assert_eq!(app.files[0].server_filename, "中文.txt");
        assert!(app.list_job.is_none());
        assert!(app.quota_job.is_some());
    }

    #[test]
    fn authenticated_file_and_transfer_pages_render_without_network() {
        let context = egui::Context::default();
        let temp = tempfile::tempdir().unwrap();
        let mut app = Studio::new_in(&context, temp.path().to_owned());
        app.api = Some(
            Api::new(Credentials {
                access_token: "test-token".into(),
                ..Default::default()
            })
            .unwrap(),
        );
        app.files = vec![
            RemoteFile {
                fs_id: 1,
                server_filename: "资料".into(),
                path: "/资料".into(),
                isdir: 1,
                ..Default::default()
            },
            RemoteFile {
                fs_id: 2,
                server_filename: "固件.bin".into(),
                path: "/固件.bin".into(),
                size: 5_000_000,
                ..Default::default()
            },
        ];
        app.enqueue(
            Task::Download(app.files[1].clone(), temp.path().join("固件.bin")),
            "固件.bin".into(),
            5_000_000,
        );
        app.entries[0].state = State::Failed;
        app.entries[0].detail = "测试失败提示".into();
        for page in [Page::Files, Page::Transfers] {
            app.page = page;
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1180.0, 790.0),
                    )),
                    ..Default::default()
                },
                |ctx| app.render(ctx),
            );
            assert!(!output.shapes.is_empty());
        }
        assert!(app.transfer_job.is_none());
        assert!(app.list_job.is_none());
    }
}
