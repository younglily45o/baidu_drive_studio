//! Recursively enumerate downloads before adding any of the batch to the transfer queue.
use crate::{
    api::{Api, PAGE_SIZE},
    model::{Error, RemoteFile, Result, parent, remote_child, remote_directory, valid_filename},
    transfer::check_cancel,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub const MAX_ENTRIES: usize = 50_000;
pub const MAX_DEPTH: usize = 128;

#[derive(Debug)]
pub struct PlannedFile {
    pub remote: RemoteFile,
    pub target: PathBuf,
}
#[derive(Debug)]
pub struct DownloadPlan {
    pub base: PathBuf,
    pub files: Vec<PlannedFile>,
    pub directories: Vec<PathBuf>,
    pub total_bytes: u64,
}
pub struct ScanProgress {
    pub files: usize,
    pub directories: usize,
    pub current: String,
}

fn validate_remote(file: &RemoteFile, expected_parent: Option<&str>) -> Result<()> {
    if file.fs_id == 0 || file.isdir > 1 || !valid_filename(&file.server_filename) {
        return Err(Error::new(format!(
            "文件或目录名称不能安全保存到 Windows：{}",
            file.server_filename
        )));
    }
    if remote_directory(&file.path)? != file.path
        || remote_child(
            expected_parent.unwrap_or(&parent(&file.path)),
            &file.server_filename,
        )? != file.path
    {
        return Err(Error::new("网盘返回了不匹配的父子路径，已停止目录扫描"));
    }
    Ok(())
}

fn target_key(base: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(base)
        .map_err(|_| Error::new("下载路径超出目标目录"))?;
    let mut parts = vec![];
    for part in relative.components() {
        let Component::Normal(name) = part else {
            return Err(Error::new("下载目标包含不安全路径段"));
        };
        let name = name
            .to_str()
            .filter(|s| valid_filename(s))
            .ok_or_else(|| Error::new("下载目标名称无效"))?;
        parts.push(name.to_uppercase());
    }
    Ok(parts.join("/"))
}

struct Builder {
    plan: DownloadPlan,
    targets: HashSet<String>,
    remote_ids: HashSet<u64>,
}
impl Builder {
    fn add(&mut self, file: RemoteFile, target: PathBuf) -> Result<()> {
        if self.targets.len() >= MAX_ENTRIES {
            return Err(Error::new(
                "单批目录下载最多支持 50,000 个文件和目录，请分批选择",
            ));
        }
        if !self.targets.insert(target_key(&self.plan.base, &target)?) {
            return Err(Error::new(
                "目录中有在 Windows 下重名的目标，请调整名称后重试",
            ));
        }
        if !self.remote_ids.insert(file.fs_id) {
            return Err(Error::new(
                "网盘返回了重复文件标识或目录循环，扫描期间目录可能已改变，请刷新后重试",
            ));
        }
        if file.isdir != 0 {
            self.plan.directories.push(target);
        } else {
            self.plan.total_bytes = self
                .plan
                .total_bytes
                .checked_add(file.size)
                .ok_or_else(|| Error::new("下载批次总大小超出支持范围"))?;
            self.plan.files.push(PlannedFile {
                remote: file,
                target,
            });
        }
        Ok(())
    }
    fn progress(&self, current: &str) -> ScanProgress {
        ScanProgress {
            files: self.plan.files.len(),
            directories: self.plan.directories.len(),
            current: current.into(),
        }
    }
}

pub fn scan(
    api: &Api,
    selected: &[RemoteFile],
    base: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(ScanProgress),
) -> Result<DownloadPlan> {
    check_cancel(cancel)?;
    let mut roots = vec![];
    let mut selected_ids = HashMap::new();
    for file in selected {
        validate_remote(file, None)?;
        if let Some(previous) = selected_ids.insert(file.path.clone(), file.fs_id) {
            if previous != file.fs_id {
                return Err(Error::new("选择中的网盘文件身份冲突，请刷新目录后重试"));
            }
            continue;
        }
        roots.push(file.clone());
    }
    roots.sort_by_key(|f| f.path.len());
    let mut outer_directories: Vec<String> = vec![];
    let mut queue = VecDeque::new();
    let mut builder = Builder {
        plan: DownloadPlan {
            base: base.into(),
            files: vec![],
            directories: vec![],
            total_bytes: 0,
        },
        targets: HashSet::new(),
        remote_ids: HashSet::new(),
    };
    for root in roots {
        check_cancel(cancel)?;
        if outer_directories
            .iter()
            .any(|d| root.path.starts_with(&format!("{d}/")))
        {
            continue;
        }
        let target = base.join(&root.server_filename);
        if root.isdir != 0 {
            outer_directories.push(root.path.clone());
            queue.push_back((root.clone(), target.clone(), 1));
        }
        builder.add(root, target)?;
    }
    while let Some((directory, target, depth)) = queue.pop_front() {
        let mut start = 0;
        loop {
            check_cancel(cancel)?;
            progress(builder.progress(&directory.path));
            check_cancel(cancel)?;
            let children = api
                .list(&directory.path, start)
                .map_err(|e| e.context(&format!("扫描目录 {}", directory.path)))?;
            check_cancel(cancel)?;
            let count = children.len();
            for child in children {
                check_cancel(cancel)?;
                validate_remote(&child, Some(&directory.path))?;
                let child_target = target.join(&child.server_filename);
                if child.isdir != 0 {
                    if depth >= MAX_DEPTH {
                        return Err(Error::new("目录层级超过 128 层，请分批选择子目录"));
                    }
                    queue.push_back((child.clone(), child_target.clone(), depth + 1));
                }
                builder.add(child, child_target)?;
            }
            progress(builder.progress(&directory.path));
            check_cancel(cancel)?;
            if count < PAGE_SIZE {
                break;
            }
            start += PAGE_SIZE;
        }
    }
    check_cancel(cancel)?;
    Ok(builder.plan)
}

/// Create parent-first directories only after a complete successful scan.
pub fn prepare_directories(plan: &DownloadPlan, cancel: &AtomicBool) -> Result<()> {
    check_cancel(cancel)?;
    fs::create_dir_all(&plan.base)?;
    let base = fs::canonicalize(&plan.base)?;
    for directory in &plan.directories {
        target_key(&plan.base, directory)?;
        let relative = directory
            .strip_prefix(&plan.base)
            .map_err(|_| Error::new("目录超出下载根目录"))?;
        let mut current = plan.base.clone();
        for component in relative.components() {
            check_cancel(cancel)?;
            current.push(component);
            let metadata = match fs::symlink_metadata(&current) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    match fs::create_dir(&current) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(e) => return Err(e.into()),
                    }
                    fs::symlink_metadata(&current)?
                }
                Err(e) => return Err(e.into()),
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(Error::new(format!(
                    "下载目录被文件或链接占用：{}",
                    current.display()
                )));
            }
            if !fs::canonicalize(&current)?.starts_with(&base) {
                return Err(Error::new("下载子目录指向了根目录以外的位置"));
            }
        }
    }
    check_cancel(cancel)
}
