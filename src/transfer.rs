use crate::{
    api::Api,
    model::{Error, RemoteFile, Result, remote_directory, valid_filename},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub const BLOCK_SIZE: usize = 4 * 1024 * 1024;
const MAX_BLOCKS: u64 = 1024;

pub fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::cancelled())
    } else {
        Ok(())
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Identity {
    fs_id: u64,
    path: String,
    size: u64,
    md5: String,
}

pub fn partial_paths(target: &Path, fs_id: u64) -> Result<(PathBuf, PathBuf)> {
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::new("本地文件名无效"))?;
    if !valid_filename(name) {
        return Err(Error::new("该网盘文件名不能安全地保存为 Windows 文件"));
    }
    let prefix = format!(".baidu-{:x}-{fs_id}", md5::compute(name.as_bytes()));
    Ok((
        target.with_file_name(format!("{prefix}.part")),
        target.with_file_name(format!("{prefix}.json")),
    ))
}

pub fn download(
    api: &Api,
    remote: &RemoteFile,
    link: &str,
    target: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64, &str),
) -> Result<()> {
    check_cancel(cancel)?;
    if target.exists() {
        return Err(Error::new(
            "本地已有同名文件，已停止下载；请移动文件或更换下载目录",
        ));
    }
    let (partial, meta) = partial_paths(target, remote.fs_id)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut output = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&partial)?;
    // OS lock releases even after a crash; independent processes cannot append to the same partial.
    output
        .try_lock()
        .map_err(|_| Error::new("另一个任务正在写入此下载临时文件"))?;
    let identity = Identity {
        fs_id: remote.fs_id,
        path: remote.path.clone(),
        size: remote.size,
        md5: remote.md5.clone(),
    };
    let mut done = output.metadata()?.len();
    if meta.exists() {
        let previous: Identity = serde_json::from_slice(&fs::read(&meta)?)?;
        if previous != identity {
            return Err(Error::new(
                "远端文件已改变，不能使用旧断点；请先删除对应 .baidu-*.part 和 .json 临时文件",
            ));
        }
    } else {
        if done != 0 {
            return Err(Error::new(
                "断点缺少身份信息，请先删除对应 .baidu-*.part 临时文件",
            ));
        }
        let mut metadata = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&meta)?;
        metadata.write_all(&serde_json::to_vec(&identity)?)?;
        metadata.sync_all()?;
    }
    if done > remote.size {
        return Err(Error::new("下载断点长度异常，请删除对应临时文件后重试"));
    }
    progress(done, remote.size, "下载中");
    if done < remote.size {
        let mut response = api.download_response(link, done)?;
        let status = response.status();
        if let Some(encoding) = response.headers().get("content-encoding")
            && encoding != "identity"
        {
            return Err(Error::new("服务器返回压缩内容，无法安全续传"));
        }
        if status == reqwest::StatusCode::PARTIAL_CONTENT {
            let range = response
                .headers()
                .get("content-range")
                .and_then(|h| h.to_str().ok())
                .and_then(parse_range)
                .ok_or_else(|| Error::new("服务器没有返回有效 Content-Range，已停止续传"))?;
            if range.0 != done || range.2 != remote.size || range.1 < range.0 || range.1 >= range.2
            {
                return Err(Error::new("续传范围与本地断点不一致，已停止下载"));
            }
            if let Some(length) = response.content_length()
                && length != range.1 - range.0 + 1
            {
                return Err(Error::new("续传 Content-Length 与范围不一致"));
            }
        } else if status == reqwest::StatusCode::OK {
            if let Some(length) = response.content_length()
                && length != remote.size
            {
                return Err(Error::new("下载响应长度与网盘文件不一致"));
            }
            output.set_len(0)?;
            done = 0;
        } else {
            return Err(Error::new(format!(
                "下载返回 HTTP {}，请刷新列表并重试",
                status.as_u16()
            )));
        }
        output.seek(SeekFrom::Start(done))?;
        let mut buffer = [0; 64 * 1024];
        loop {
            check_cancel(cancel)?;
            let n = response
                .read(&mut buffer)
                .map_err(|_| Error::new("下载连接中断；断点已保留，可点击重试"))?;
            if n == 0 {
                break;
            }
            if done.saturating_add(n as u64) > remote.size {
                return Err(Error::new("收到的数据超过文件声明长度，已停止下载"));
            }
            output.write_all(&buffer[..n])?;
            done += n as u64;
            progress(done, remote.size, "下载中");
        }
    }
    check_cancel(cancel)?;
    if done != remote.size {
        return Err(Error::new("下载未完成；断点已保留，可点击重试"));
    }
    output.sync_all()?;
    drop(output);
    publish_no_replace(&partial, target).map_err(|e| {
        Error::new(format!(
            "无法发布下载文件（目标被占用或目录不可写）；完整临时文件已保留：{e}"
        ))
    })?;
    // Once published, cleanup failure must not report the completed transfer as failed.
    #[cfg(not(any(windows, target_os = "linux")))]
    let _ = fs::remove_file(&partial);
    let _ = fs::remove_file(&meta);
    progress(done, remote.size, "下载完成");
    Ok(())
}

#[cfg(windows)]
fn publish_no_replace(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    // canonicalize supplies Windows' extended-length prefix, including for UNC paths.
    // The destination does not exist yet; resolve its parent instead.
    let source = fs::canonicalize(source)?;
    let name = target.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "download target has no filename",
        )
    })?;
    let directory = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let target = fs::canonicalize(directory)?.join(name);
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    // Same-directory rename, no REPLACE_EXISTING: supports FAT32/exFAT and refuses an occupied target.
    if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn publish_no_replace(source: &Path, target: &Path) -> std::io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        source,
        rustix::fs::CWD,
        target,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(Into::into)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn publish_no_replace(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::hard_link(source, target)
}

fn parse_range(value: &str) -> Option<(u64, u64, u64)> {
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

fn read_block(file: &mut File, buffer: &mut Vec<u8>) -> Result<()> {
    buffer.clear();
    file.take(BLOCK_SIZE as u64).read_to_end(buffer)?;
    Ok(())
}

pub fn upload(
    api: &Api,
    local: &Path,
    remote: &str,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64, &str),
) -> Result<String> {
    check_cancel(cancel)?;
    let remote = remote_directory(remote)?;
    let mut input = File::open(local)?;
    let initial = input.metadata()?;
    if !initial.is_file() {
        return Err(Error::new("首版仅支持上传普通文件，请逐个选择文件"));
    }
    let total = initial.len();
    if total > MAX_BLOCKS * BLOCK_SIZE as u64 {
        return Err(Error::new("首版 4 MiB 分片最多支持 4 GiB 单文件"));
    }
    let mut hashes = vec![];
    let mut buffer = Vec::with_capacity(BLOCK_SIZE);
    let mut hashed = 0;
    let mut content_md5 = md5::Context::new();
    let mut crc = crc32fast::Hasher::new();
    let mut slice_md5 = String::new();
    progress(0, total, "计算分片 MD5");
    loop {
        check_cancel(cancel)?;
        read_block(&mut input, &mut buffer)?;
        if buffer.is_empty() && !hashes.is_empty() {
            break;
        }
        hashes.push(format!("{:x}", md5::compute(&buffer)));
        if api.is_cookie() {
            content_md5.consume(&buffer);
            crc.update(&buffer);
            if slice_md5.is_empty() {
                slice_md5 = format!(
                    "{:x}",
                    md5::compute(&buffer[..buffer.len().min(256 * 1024)])
                );
            }
        }
        hashed += buffer.len() as u64;
        progress(hashed, total, "计算分片 MD5");
        if buffer.is_empty() {
            break;
        }
    }
    if hashed != total || input.metadata()?.modified()? != initial.modified()? {
        return Err(Error::new(
            "上传源文件在计算哈希时改变，请关闭编辑程序后重试",
        ));
    }
    check_cancel(cancel)?;
    let block_list = serde_json::to_string(&hashes)?;
    let mut pre_fields = vec![
        ("path", remote.clone()),
        ("size", total.to_string()),
        ("isdir", "0".into()),
        ("autoinit", "1".into()),
        ("rtype", "1".into()),
        ("block_list", block_list.clone()),
    ];
    if api.is_cookie() {
        pre_fields.extend([
            ("content-md5", format!("{:x}", content_md5.compute())),
            ("slice-md5", slice_md5),
            ("contentCrc32", crc.finalize().to_string()),
        ]);
    }
    let pre = api.post("precreate", &pre_fields)?;
    if pre["return_type"].as_u64() == Some(2) {
        progress(total, total, "秒传完成");
        return Ok(pre["path"].as_str().unwrap_or(&remote).to_owned());
    }
    let upload_id = pre["uploadid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::new("预上传响应缺少 uploadid"))?;
    let missing: Vec<usize> = serde_json::from_value(
        pre.get("block_list")
            .cloned()
            .ok_or_else(|| Error::new("预上传响应缺少分片索引"))?,
    )?;
    let mut seen = std::collections::HashSet::new();
    if missing
        .iter()
        .any(|&i| i >= hashes.len() || !seen.insert(i))
    {
        return Err(Error::new("服务端返回了无效或重复的分片索引"));
    }
    let missing_bytes: u64 = missing
        .iter()
        .map(|&i| {
            total
                .saturating_sub(i as u64 * BLOCK_SIZE as u64)
                .min(BLOCK_SIZE as u64)
        })
        .sum();
    let mut done = total - missing_bytes;
    progress(done, total, "上传中");
    for i in missing {
        check_cancel(cancel)?;
        input.seek(SeekFrom::Start(i as u64 * BLOCK_SIZE as u64))?;
        read_block(&mut input, &mut buffer)?;
        if format!("{:x}", md5::compute(&buffer)) != hashes[i] {
            return Err(Error::new("源文件内容在上传过程中改变，已停止合并"));
        }
        let n = buffer.len() as u64;
        let received = api.upload_block(&remote, upload_id, i, std::mem::take(&mut buffer))?;
        if !received.eq_ignore_ascii_case(&hashes[i]) {
            return Err(Error::new("服务端返回的分片 MD5 不匹配，已停止合并"));
        }
        done += n;
        progress(done, total, "上传中");
    }
    check_cancel(cancel)?;
    if input.metadata()?.len() != total || input.metadata()?.modified()? != initial.modified()? {
        return Err(Error::new("源文件在上传过程中改变，已停止合并"));
    }
    progress(total, total, "合并云端文件");
    // Even if only some blocks were missing, create needs every block's hash in original order.
    let result = api.post(
        "create",
        &[
            ("path", remote.clone()),
            ("size", total.to_string()),
            ("isdir", "0".into()),
            ("rtype", "1".into()),
            ("uploadid", upload_id.into()),
            ("block_list", block_list),
        ],
    )?;
    let path = result["path"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::new("合并响应缺少最终路径，请刷新网盘列表确认结果"))?;
    progress(total, total, "上传完成");
    Ok(path.to_owned())
}
