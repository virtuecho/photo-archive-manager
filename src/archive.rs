use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use sevenz_rust2::{
    ArchiveEntry, ArchiveReader, ArchiveWriter, EncoderMethod, Password,
    encoder_options::AesEncoderOptions,
};
use sha2::{Digest, Sha256};

use crate::{files, scan_sources, task_control::TaskControl};

const BUFFER_SIZE: usize = 1024 * 1024;
static NEXT_PARTIAL: AtomicU64 = AtomicU64::new(0);
static NEXT_RESTORE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct ArchiveResult {
    pub volume_paths: Vec<PathBuf>,
    pub volume_count: usize,
    pub file_count: usize,
    pub total_bytes: u64,
    pub archive_bytes: u64,
    pub strict_restore_verified: bool,
    pub cleanup_warning: Option<String>,
}

#[derive(Clone, Copy)]
struct ManifestFile<'a> {
    path: &'a Path,
    relative: &'a Path,
    size: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone)]
struct ExpectedEntry {
    directory: bool,
    size: u64,
    sha256: Option<String>,
}

struct RemoveOnDrop(Vec<PathBuf>);

struct TemporaryRestore(Option<PathBuf>);

impl TemporaryRestore {
    fn create() -> io::Result<Self> {
        let base = std::env::temp_dir();
        loop {
            let suffix = NEXT_RESTORE.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!(
                "photo-archive-restore-{}-{suffix}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Err(error) =
                            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                        {
                            let _ = fs::remove_dir(&path);
                            return Err(error);
                        }
                    }
                    return Ok(Self(Some(path)));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    fn path(&self) -> &Path {
        self.0.as_deref().expect("restore directory is active")
    }

    fn remove_now(&mut self) -> io::Result<()> {
        let Some(path) = self.0.take() else {
            return Ok(());
        };
        match fs::remove_dir_all(&path) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.0 = Some(path);
                Err(error)
            }
        }
    }
}

impl Drop for TemporaryRestore {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

impl RemoveOnDrop {
    fn track(&mut self, path: PathBuf) {
        self.0.push(path);
    }

    fn remove_path(&mut self, path: &Path) -> std::io::Result<()> {
        fs::remove_file(path)?;
        self.0.retain(|tracked| tracked != path);
        Ok(())
    }

    fn remove_now(&mut self) -> std::io::Result<()> {
        let mut first_error = None;
        for path in self.0.drain(..) {
            if let Err(error) = fs::remove_file(path)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        for path in self.0.drain(..) {
            let _ = fs::remove_file(path);
        }
    }
}

struct VolumeReader {
    files: Vec<File>,
    lengths: Vec<u64>,
    offsets: Vec<u64>,
    total_len: u64,
    position: u64,
}

impl VolumeReader {
    fn open(paths: &[PathBuf]) -> io::Result<Self> {
        if paths.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "archive volume list is empty",
            ));
        }
        let mut files = Vec::with_capacity(paths.len());
        let mut lengths = Vec::with_capacity(paths.len());
        let mut offsets = Vec::with_capacity(paths.len());
        let mut total_len = 0u64;
        for path in paths {
            let file = File::open(path)?;
            offsets.push(total_len);
            let length = file.metadata()?.len();
            total_len = total_len.checked_add(length).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "archive volume sizes overflow")
            })?;
            files.push(file);
            lengths.push(length);
        }
        Ok(Self {
            files,
            lengths,
            offsets,
            total_len,
            position: 0,
        })
    }
}

impl Read for VolumeReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.position >= self.total_len {
            return Ok(0);
        }
        let volume_index = self
            .offsets
            .partition_point(|offset| *offset <= self.position)
            .saturating_sub(1);
        let local_position = self.position - self.offsets[volume_index];
        let available =
            (self.lengths[volume_index] - local_position).min(buffer.len() as u64) as usize;
        self.files[volume_index].seek(SeekFrom::Start(local_position))?;
        let count = self.files[volume_index].read(&mut buffer[..available])?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "archive volume ended before its recorded size",
            ));
        }
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for VolumeReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let target = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.total_len) + i128::from(offset),
        };
        if target < 0 || target > i128::from(u64::MAX) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid archive volume seek",
            ));
        }
        self.position = target as u64;
        Ok(self.position)
    }
}

#[cfg(test)]
pub fn create_and_verify(
    source: &Path,
    output_directory: &Path,
    archive_name: &str,
    password: &str,
    max_bytes: u64,
    excluded_paths: &BTreeSet<PathBuf>,
    included_sha256: Option<&BTreeMap<PathBuf, String>>,
    control: &TaskControl,
    progress: impl FnMut(usize, usize, u64),
) -> Result<ArchiveResult, String> {
    create_and_verify_with_restore(
        source,
        output_directory,
        archive_name,
        password,
        max_bytes,
        false,
        excluded_paths,
        included_sha256,
        control,
        progress,
    )
}

pub fn create_and_verify_with_restore(
    source: &Path,
    output_directory: &Path,
    archive_name: &str,
    password: &str,
    max_bytes: u64,
    strict_restore: bool,
    excluded_paths: &BTreeSet<PathBuf>,
    included_sha256: Option<&BTreeMap<PathBuf, String>>,
    control: &TaskControl,
    mut progress: impl FnMut(usize, usize, u64),
) -> Result<ArchiveResult, String> {
    let source = fs::canonicalize(source)
        .map_err(|error| format!("无法解析归档来源 {}：{error}", source.display()))?;
    let output_directory = fs::canonicalize(output_directory).map_err(|error| {
        format!(
            "无法解析归档输出目录 {}：{error}",
            output_directory.display()
        )
    })?;
    let excluded_paths = excluded_paths
        .iter()
        .map(|path| fs::canonicalize(path).unwrap_or_else(|_| path.clone()))
        .collect::<BTreeSet<_>>();
    let included_sha256 = included_sha256.map(|entries| {
        entries
            .iter()
            .map(|(path, sha256)| {
                (
                    fs::canonicalize(path).unwrap_or_else(|_| path.clone()),
                    sha256.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>()
    });
    if !source.is_dir() || !output_directory.is_dir() {
        return Err("归档来源和输出位置都必须是现有文件夹。".into());
    }
    if source.starts_with(&output_directory) || output_directory.starts_with(&source) {
        return Err("归档来源与输出目录相同或互相嵌套；请选择不重叠的位置。".into());
    }
    if password.is_empty() {
        return Err("加密密码不能为空。".into());
    }
    if archive_name.is_empty()
        || archive_name == "."
        || archive_name == ".."
        || Path::new(archive_name)
            .file_name()
            .and_then(|name| name.to_str())
            != Some(archive_name)
        || archive_name.contains(['/', '\\', ':'])
    {
        return Err("归档名称只能是单个文件名，不能包含路径或盘符。".into());
    }
    let archive_name = if archive_name.to_ascii_lowercase().ends_with(".7z") {
        archive_name.to_string()
    } else {
        format!("{archive_name}.7z")
    };
    let output = output_directory.join(archive_name);
    if max_bytes == 0 {
        return Err("每卷大小必须大于 0。".into());
    }
    if fs::symlink_metadata(&output).is_ok()
        || fs::symlink_metadata(volume_path(&output, 1, true)).is_ok()
    {
        return Err(format!(
            "目标归档或首卷已存在，不会覆盖：{}",
            output.display()
        ));
    }

    let summary = scan_sources(&[source.clone()], control, |_| {})
        .map_err(|_| "归档扫描已取消，未创建归档。".to_string())?;
    if summary.error_count > 0 {
        return Err(format!(
            "来源扫描有 {} 项读取错误（例如 {}）；不能将不完整清单写入归档。",
            summary.error_count,
            summary
                .error_samples
                .first()
                .map(String::as_str)
                .unwrap_or("未提供")
        ));
    }
    let expected_manifest_paths = included_sha256.as_ref().map(|entries| {
        entries
            .keys()
            .filter(|path| path.starts_with(&source))
            .cloned()
            .collect::<BTreeSet<_>>()
    });
    if expected_manifest_paths
        .as_ref()
        .is_some_and(BTreeSet::is_empty)
    {
        return Err("最终清单中没有当前归档来源下的已确认文件。".into());
    }
    if let Some(expected_paths) = &expected_manifest_paths {
        let current_paths = summary
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<BTreeSet<_>>();
        let missing = expected_paths.difference(&current_paths).count();
        if missing > 0 {
            return Err(format!(
                "最终清单中有 {missing} 个文件已不存在或无法读取；请重新执行重复检查。"
            ));
        }
    }
    let top_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "来源文件夹名不是有效 UTF-8，无法安全写入 7z 清单。".to_string())?;
    let files = summary
        .files
        .iter()
        .filter(|file| {
            !excluded_paths.contains(&file.path)
                && included_sha256
                    .as_ref()
                    .is_none_or(|entries| entries.contains_key(&file.path))
        })
        .map(|file| ManifestFile {
            path: &file.path,
            relative: &file.relative_path,
            size: file.size,
            modified: file.modified,
        })
        .collect::<Vec<_>>();
    let total_bytes = files
        .iter()
        .fold(0u64, |sum, file| sum.saturating_add(file.size));
    let initial_stamps = files
        .iter()
        .map(|file| (file.path.to_path_buf(), (file.size, file.modified)))
        .collect::<BTreeMap<_, _>>();
    let mut expected = BTreeMap::<String, ExpectedEntry>::new();
    expected.insert(
        top_name.to_string(),
        ExpectedEntry {
            directory: true,
            size: 0,
            sha256: None,
        },
    );
    let mut processed_bytes = 0u64;
    for (index, file) in files.iter().enumerate() {
        if !control.checkpoint() {
            return Err("归档已取消，未创建归档。".into());
        }
        let name = archive_entry_name(top_name, file.relative)?;
        let before = fs::metadata(file.path)
            .map_err(|error| format!("无法读取来源 {}：{error}", file.path.display()))?;
        if !before.is_file() || before.len() != file.size || before.modified().ok() != file.modified
        {
            return Err(format!(
                "来源文件在扫描后发生变化：{}；请重新扫描。",
                file.path.display()
            ));
        }
        let sha256 = files::sha256_file(file.path)
            .map_err(|error| format!("无法计算来源 SHA-256 {}：{error}", file.path.display()))?;
        if included_sha256
            .as_ref()
            .and_then(|entries| entries.get(file.path))
            .is_some_and(|expected| expected != &sha256)
        {
            return Err(format!(
                "来源内容与阶段⑥ Final Manifest 的 SHA-256 不符：{}；请重新检查重复项。",
                file.path.display()
            ));
        }
        let after = fs::metadata(file.path)
            .map_err(|error| format!("无法重新读取来源 {}：{error}", file.path.display()))?;
        if after.len() != before.len() || after.modified().ok() != before.modified().ok() {
            return Err(format!(
                "来源文件在建立清单时发生变化：{}；请重新扫描。",
                file.path.display()
            ));
        }
        expected.insert(
            name,
            ExpectedEntry {
                directory: false,
                size: file.size,
                sha256: Some(sha256),
            },
        );
        processed_bytes = processed_bytes.saturating_add(file.size);
        progress(
            index + 1,
            files.len().saturating_mul(4).max(1),
            processed_bytes,
        );
    }

    for directory in archive_directory_names(&source, top_name, control)? {
        expected.entry(directory).or_insert(ExpectedEntry {
            directory: true,
            size: 0,
            sha256: None,
        });
    }

    let partial = loop {
        let suffix = NEXT_PARTIAL.fetch_add(1, Ordering::Relaxed);
        let partial = output_directory.join(format!(
            ".{}.{}.{}.partial",
            output
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("archive"),
            std::process::id(),
            suffix
        ));
        if fs::symlink_metadata(&partial).is_err() {
            break partial;
        }
    };
    let mut cleanup = RemoveOnDrop(vec![partial.clone()]);
    let mut writer =
        ArchiveWriter::create(&partial).map_err(|error| format!("无法创建临时 7z：{error}"))?;
    writer.set_content_methods(vec![
        AesEncoderOptions::new(Password::from(password)).into(),
        EncoderMethod::COPY.into(),
    ]);
    writer.set_encrypt_header(true);
    for name in expected
        .iter()
        .filter_map(|(name, entry)| entry.directory.then_some(name))
    {
        writer
            .push_archive_entry::<File>(ArchiveEntry::new_directory(name), None)
            .map_err(|error| format!("无法写入目录 {name}：{error}"))?;
    }
    processed_bytes = 0;
    for (index, file) in files.iter().enumerate() {
        if !control.checkpoint() {
            return Err("归档已取消，临时输出已清理。".into());
        }
        let name = archive_entry_name(top_name, file.relative)?;
        let mut input = File::open(file.path)
            .map_err(|error| format!("无法打开来源 {}：{error}", file.path.display()))?;
        let before = input
            .metadata()
            .map_err(|error| format!("无法检查来源 {}：{error}", file.path.display()))?;
        if before.len() != file.size || before.modified().ok() != file.modified {
            return Err(format!(
                "来源文件在写入归档前发生变化：{}；请重新扫描。",
                file.path.display()
            ));
        }
        writer
            .push_archive_entry(
                ArchiveEntry::from_path(file.path, name.clone()),
                Some(&mut input),
            )
            .map_err(|error| format!("归档文件 {} 失败：{error}", file.path.display()))?;
        let after = input
            .metadata()
            .map_err(|error| format!("无法重新检查来源 {}：{error}", file.path.display()))?;
        if after.len() != before.len() || after.modified().ok() != before.modified().ok() {
            return Err(format!(
                "来源文件在写入归档时发生变化：{}；临时归档已清理。",
                file.path.display()
            ));
        }
        processed_bytes = processed_bytes.saturating_add(file.size);
        progress(
            files.len().saturating_add(index + 1),
            files.len().saturating_mul(4).max(1),
            processed_bytes,
        );
    }
    writer
        .finish()
        .map_err(|error| format!("7z 编码器无法完成归档：{error}"))?;
    File::open(&partial)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("无法同步临时归档：{error}"))?;

    let final_scan = scan_sources(&[source.clone()], control, |_| {})
        .map_err(|_| "归档过程中来源发生变化或扫描被取消；临时归档已清理。".to_string())?;
    let final_files = final_scan
        .files
        .iter()
        .filter(|file| {
            !excluded_paths.contains(&file.path)
                && included_sha256
                    .as_ref()
                    .is_none_or(|entries| entries.contains_key(&file.path))
        })
        .collect::<Vec<_>>();
    if final_scan.error_count > 0
        || final_files.len() != files.len()
        || final_files.iter().any(|current| {
            initial_stamps.get(&current.path) != Some(&(current.size, current.modified))
        })
    {
        return Err("归档过程中来源清单发生变化；临时归档已清理，请重新扫描后再试。".into());
    }

    let staged_volumes = split_archive_file(
        &partial,
        max_bytes,
        &mut cleanup,
        control,
        |split_bytes, total_split_bytes| {
            let file_units = files.len();
            let split_units = if total_split_bytes == 0 {
                file_units
            } else {
                ((split_bytes.saturating_mul(file_units as u64) + total_split_bytes - 1)
                    / total_split_bytes) as usize
            };
            progress(
                file_units.saturating_mul(2).saturating_add(split_units),
                file_units.saturating_mul(4).max(1),
                split_bytes,
            );
        },
    )?;
    if staged_volumes.len() > 1 {
        cleanup
            .remove_path(&partial)
            .map_err(|error| format!("已拆分归档卷，但无法清理临时单文件：{error}"))?;
    }
    let archive_bytes = staged_volumes.iter().try_fold(0u64, |sum, path| {
        fs::metadata(path)
            .map(|metadata| sum.saturating_add(metadata.len()))
            .map_err(|error| format!("无法读取临时归档卷 {}：{error}", path.display()))
    })?;
    let mut restore = strict_restore
        .then(TemporaryRestore::create)
        .transpose()
        .map_err(|error| format!("无法创建隔离还原目录：{error}"))?;
    verify_archive_volumes(
        &staged_volumes,
        password,
        &expected,
        files.len(),
        restore.as_ref().map(TemporaryRestore::path),
        control,
        &mut progress,
    )?;
    if let Some(restore) = &mut restore {
        if let Err(error) = restore.remove_now() {
            return Err(format!(
                "归档已验证，但无法清理隔离还原目录 {}：{error}",
                restore.path().display()
            ));
        }
    }
    if !control.checkpoint() {
        return Err("归档验证期间已取消；临时归档已清理。".into());
    }
    let final_paths = if staged_volumes.len() == 1 {
        vec![output.clone()]
    } else {
        (1..=staged_volumes.len())
            .map(|index| volume_path(&output, index, true))
            .collect::<Vec<_>>()
    };
    if let Some(existing) = final_paths
        .iter()
        .find(|path| fs::symlink_metadata(path).is_ok())
    {
        return Err(format!(
            "目标归档卷已存在，不会覆盖：{}",
            existing.display()
        ));
    }
    let mut committed = Vec::<PathBuf>::new();
    for (staged, final_path) in staged_volumes.iter().zip(&final_paths) {
        if let Err(error) = fs::hard_link(staged, final_path) {
            let mut rollback_errors = Vec::new();
            for path in &committed {
                if let Err(error) = fs::remove_file(path) {
                    rollback_errors.push(format!("{}：{error}", path.display()));
                }
            }
            return Err(format!(
                "无法安全提交归档卷 {}：{error}。{}",
                final_path.display(),
                if rollback_errors.is_empty() {
                    "已回滚本轮提交的卷。".to_string()
                } else {
                    format!("回滚不完整：{}", rollback_errors.join("；"))
                }
            ));
        }
        committed.push(final_path.clone());
    }
    let cleanup_warning = cleanup
        .remove_now()
        .err()
        .map(|error| format!("归档已提交，但临时卷未能清除：{error}"));
    Ok(ArchiveResult {
        volume_paths: final_paths.clone(),
        volume_count: final_paths.len(),
        file_count: files.len(),
        total_bytes,
        archive_bytes,
        strict_restore_verified: strict_restore,
        cleanup_warning,
    })
}

fn archive_entry_name(top_name: &str, relative: &Path) -> Result<String, String> {
    let mut components = Vec::new();
    for component in relative.components() {
        let value = component
            .as_os_str()
            .to_str()
            .ok_or_else(|| format!("归档路径包含非 UTF-8 文件名：{}", relative.display()))?;
        if value.is_empty() || value == "." || value == ".." || value.contains(['/', '\\']) {
            return Err(format!(
                "归档路径中包含不安全路径段：{}",
                relative.display()
            ));
        }
        components.push(value);
    }
    if components.is_empty() {
        return Err("来源文件的相对路径为空。".into());
    }
    Ok(format!("{top_name}/{}", components.join("/")))
}

fn restore_path(root: &Path, archive_name: &str) -> io::Result<PathBuf> {
    if archive_name.contains(['\\', '\0']) || (cfg!(windows) && archive_name.contains(':')) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "归档还原路径含有不安全字符",
        ));
    }
    let mut path = root.to_path_buf();
    for component in archive_name.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "归档还原路径含有不安全路径段",
            ));
        }
        path.push(component);
    }
    if !path.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "归档还原路径越出隔离目录",
        ));
    }
    Ok(path)
}

fn archive_directory_names(
    root: &Path,
    top_name: &str,
    control: &TaskControl,
) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        if !control.checkpoint() {
            return Err("归档目录扫描已取消。".into());
        }
        let entries = fs::read_dir(&directory)
            .map_err(|error| format!("无法扫描目录 {}：{error}", directory.display()))?;
        for entry in entries {
            if !control.checkpoint() {
                return Err("归档目录扫描已取消。".into());
            }
            let entry = entry.map_err(|error| {
                format!("无法读取目录 {} 中的项目：{error}", directory.display())
            })?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("无法检查 {}：{error}", path.display()))?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|error| format!("无法生成内部目录路径：{error}"))?;
                names.insert(archive_entry_name(top_name, relative)?);
                pending.push(path);
            }
        }
    }
    Ok(names)
}

fn volume_path(base: &Path, index: usize, numbered: bool) -> PathBuf {
    if !numbered {
        return base.to_path_buf();
    }
    let name = base
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "archive.7z".into());
    base.with_file_name(format!("{name}.{index:03}"))
}

fn split_archive_file(
    archive: &Path,
    max_bytes: u64,
    cleanup: &mut RemoveOnDrop,
    control: &TaskControl,
    mut progress: impl FnMut(u64, u64),
) -> Result<Vec<PathBuf>, String> {
    let total = fs::metadata(archive)
        .map_err(|error| format!("无法读取临时归档 {}：{error}", archive.display()))?
        .len();
    if total <= max_bytes {
        progress(total, total);
        return Ok(vec![archive.to_path_buf()]);
    }
    let volume_count = usize::try_from(total.div_ceil(max_bytes))
        .map_err(|_| "归档分卷数超过当前平台支持范围。".to_string())?;
    let mut input = File::open(archive)
        .map_err(|error| format!("无法打开待分卷归档 {}：{error}", archive.display()))?;
    let mut paths = Vec::with_capacity(volume_count);
    let mut copied = 0u64;
    let mut next_progress = (max_bytes / 100).max(BUFFER_SIZE as u64);
    let mut buffer = vec![0u8; BUFFER_SIZE.min(max_bytes as usize).max(1)];
    for index in 1..=volume_count {
        if !control.checkpoint() {
            return Err("归档分卷已取消；临时卷已清理。".into());
        }
        let path = volume_path(archive, index, true);
        let output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("无法创建临时归档卷 {}：{error}", path.display()))?;
        cleanup.track(path.clone());
        paths.push(path.clone());
        let mut output = output;
        let mut in_volume = 0u64;
        let volume_limit = (total - copied).min(max_bytes);
        while in_volume < volume_limit {
            if !control.checkpoint() {
                return Err("归档分卷已取消；临时卷已清理。".into());
            }
            let wanted = (volume_limit - in_volume).min(buffer.len() as u64) as usize;
            let count = input
                .read(&mut buffer[..wanted])
                .map_err(|error| format!("读取归档分卷输入失败：{error}"))?;
            if count == 0 {
                return Err("临时归档在分卷过程中意外结束。".into());
            }
            output
                .write_all(&buffer[..count])
                .map_err(|error| format!("写入临时归档卷 {} 失败：{error}", path.display()))?;
            copied = copied.saturating_add(count as u64);
            in_volume = in_volume.saturating_add(count as u64);
            if copied >= next_progress || copied == total {
                progress(copied, total);
                next_progress = copied.saturating_add((max_bytes / 100).max(BUFFER_SIZE as u64));
            }
        }
        output
            .sync_all()
            .map_err(|error| format!("无法同步临时归档卷 {}：{error}", path.display()))?;
    }
    if copied != total {
        return Err(format!(
            "归档分卷总字节数不符：预期 {total}，实际 {copied}。"
        ));
    }
    Ok(paths)
}

fn verify_archive_volumes(
    paths: &[PathBuf],
    password: &str,
    expected: &BTreeMap<String, ExpectedEntry>,
    file_count: usize,
    restore_root: Option<&Path>,
    control: &TaskControl,
    progress: &mut impl FnMut(usize, usize, u64),
) -> Result<(), String> {
    let source =
        VolumeReader::open(paths).map_err(|error| format!("无法打开完整归档卷序列：{error}"))?;
    verify_archive_reader(
        source,
        password,
        expected,
        file_count,
        restore_root,
        control,
        progress,
    )
}

fn verify_archive_reader<R: Read + Seek>(
    source: R,
    password: &str,
    expected: &BTreeMap<String, ExpectedEntry>,
    file_count: usize,
    restore_root: Option<&Path>,
    control: &TaskControl,
    progress: &mut impl FnMut(usize, usize, u64),
) -> Result<(), String> {
    let password = Password::from(password);
    let mut reader = ArchiveReader::new(source, password)
        .map_err(|error| format!("无法打开加密归档或密码不正确：{error}"))?;
    let mut seen = BTreeSet::new();
    let mut verification_errors = Vec::new();
    let mut verified_files = 0usize;
    let mut verified_bytes = 0u64;
    reader
        .for_each_entries(|entry, input| {
            if !control.checkpoint() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "archive verification cancelled",
                )
                .into());
            }
            let name = entry.name().to_string();
            if !seen.insert(name.clone()) {
                verification_errors.push(format!("归档内存在重复路径：{name}"));
                std::io::copy(input, &mut std::io::sink())?;
                return Ok(true);
            }
            let Some(expected_entry) = expected.get(&name) else {
                verification_errors.push(format!("归档中出现清单之外的路径：{name}"));
                std::io::copy(input, &mut std::io::sink())?;
                return Ok(true);
            };
            if entry.is_directory() != expected_entry.directory {
                verification_errors.push(format!("归档路径类型不符：{name}"));
                std::io::copy(input, &mut std::io::sink())?;
                return Ok(true);
            }
            if expected_entry.directory {
                if let Some(root) = restore_root {
                    let path = restore_path(root, &name)?;
                    fs::create_dir_all(&path)?;
                }
                return Ok(true);
            }
            let mut restored_file = if let Some(root) = restore_root {
                let path = restore_path(root, &name)?;
                let parent = path.parent().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "还原文件缺少父目录")
                })?;
                fs::create_dir_all(parent)?;
                Some((
                    path.clone(),
                    OpenOptions::new().write(true).create_new(true).open(path)?,
                ))
            } else {
                None
            };
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; BUFFER_SIZE];
            let mut bytes = 0u64;
            loop {
                if !control.checkpoint() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "archive verification cancelled",
                    )
                    .into());
                }
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
                if let Some((_, file)) = &mut restored_file {
                    file.write_all(&buffer[..count])?;
                }
                bytes = bytes.saturating_add(count as u64);
            }
            let stream_digest = to_hex(&hasher.finalize());
            if bytes != expected_entry.size || Some(stream_digest) != expected_entry.sha256 {
                verification_errors.push(format!(
                    "归档内容与来源清单不符：{name}（预期 {} 字节）",
                    expected_entry.size
                ));
            }
            if let Some((path, file)) = restored_file.take() {
                file.sync_all()?;
                drop(file);
                let metadata = fs::metadata(&path)?;
                let disk_digest = files::sha256_file(&path)?;
                if metadata.len() != expected_entry.size
                    || Some(disk_digest) != expected_entry.sha256
                {
                    verification_errors.push(format!(
                        "隔离还原后的文件与来源清单不符：{}",
                        path.display()
                    ));
                }
            }
            verified_files += 1;
            verified_bytes = verified_bytes.saturating_add(bytes);
            progress(
                file_count.saturating_mul(3).saturating_add(verified_files),
                file_count.saturating_mul(4).max(1),
                verified_bytes,
            );
            Ok(true)
        })
        .map_err(|error| format!("7z 完整性读取测试失败：{error}"))?;

    for name in expected.keys() {
        if !seen.contains(name) {
            verification_errors.push(format!("归档缺少清单项：{name}"));
        }
    }
    if !verification_errors.is_empty() {
        return Err(format!(
            "7z 清单/逐文件 SHA-256 校验失败：{}",
            verification_errors
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join("；")
        ));
    }
    if verified_files != file_count {
        return Err(format!(
            "归档中验证到 {verified_files} 个文件，预期 {file_count} 个。"
        ));
    }
    Ok(())
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::atomic::AtomicU64};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    fn test_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "photo-archive-manager-archive-{}-{}",
            std::process::id(),
            NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn preserve_test_archive(result: &ArchiveResult) {
        let Some(destination) = std::env::var_os("PAM_TEST_ARCHIVE_OUTPUT") else {
            return;
        };
        let destination = PathBuf::from(destination);
        if result.volume_count == 1 {
            fs::copy(&result.volume_paths[0], destination).unwrap();
        } else {
            for (index, volume) in result.volume_paths.iter().enumerate() {
                fs::copy(volume, volume_path(&destination, index + 1, true)).unwrap();
            }
        }
    }

    #[test]
    fn strict_restore_checks_written_files_and_cleans_its_temporary_directory() {
        let root = test_dir();
        let source = root.join("Photos");
        let output = root.join("output");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(source.join("nested/photo.jpg"), b"restored image bytes").unwrap();

        let result = create_and_verify_with_restore(
            &source,
            &output,
            "Photos.7z",
            "pw",
            2_000_000_000,
            true,
            &BTreeSet::new(),
            None,
            &TaskControl::default(),
            |_, _, _| {},
        )
        .unwrap();

        assert!(result.strict_restore_verified);
        assert_eq!(result.file_count, 1);
        let prefix = format!("photo-archive-restore-{}-", std::process::id());
        assert!(
            std::fs::read_dir(std::env::temp_dir())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&prefix))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn strict_restore_rejects_unsafe_archive_paths() {
        let root = test_dir();
        for name in [
            "../escape",
            "Photos/../../escape",
            "/absolute",
            "Photos\\escape",
        ] {
            assert!(restore_path(&root, name).is_err(), "{name}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn volume_reader_rejects_a_missing_required_volume() {
        let root = test_dir();
        let first = root.join("archive.7z.001");
        let missing = root.join("archive.7z.002");
        fs::write(&first, b"first volume").unwrap();

        let error = VolumeReader::open(&[first, missing]).err().unwrap();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn creates_encrypted_store_with_root_and_stream_verifies_manifest() {
        let root = test_dir();
        let source = root.join("Photos");
        let output = root.join("output");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(source.join("相机.JPG"), b"picture").unwrap();
        fs::write(source.join("nested/MOV.MOV"), b"video bytes").unwrap();
        let result = create_and_verify(
            &source,
            &output,
            "Photos 2026_11_02.7z",
            "中文-password",
            2_000_000_000,
            &BTreeSet::new(),
            None,
            &TaskControl::default(),
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(result.file_count, 2);
        assert!(result.volume_paths[0].exists());
        preserve_test_archive(&result);
        let mut reader =
            ArchiveReader::open(&result.volume_paths[0], Password::from("中文-password")).unwrap();
        let mut names = Vec::new();
        reader
            .for_each_entries(|entry, input| {
                names.push(entry.name().to_string());
                std::io::copy(input, &mut std::io::sink())?;
                Ok(true)
            })
            .unwrap();
        assert!(names.contains(&"Photos".to_string()));
        assert!(names.contains(&"Photos/相机.JPG".to_string()));
        assert!(names.contains(&"Photos/nested/MOV.MOV".to_string()));
        assert!(!result.volume_paths[0].with_extension("7z.partial").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_existing_archive_and_output_inside_source() {
        let root = test_dir();
        let source = root.join("Photos");
        let output = root.join("output");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&output).unwrap();
        let error = create_and_verify(
            &source,
            &source,
            "archive.7z",
            "pw",
            2_000_000_000,
            &BTreeSet::new(),
            None,
            &TaskControl::default(),
            |_, _, _| {},
        )
        .unwrap_err();
        assert!(error.contains("互相嵌套"));
        fs::write(output.join("archive.7z"), b"existing").unwrap();
        let error = create_and_verify(
            &source,
            &output,
            "archive.7z",
            "pw",
            2_000_000_000,
            &BTreeSet::new(),
            None,
            &TaskControl::default(),
            |_, _, _| {},
        )
        .unwrap_err();
        assert!(error.contains("已存在"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn carries_only_explicit_exclusions_into_the_archive_manifest() {
        let root = test_dir();
        let source = root.join("Photos");
        let output = root.join("output");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&output).unwrap();
        let keep = source.join("keep.jpg");
        let exclude = source.join("exclude.jpg");
        fs::write(&keep, b"keep").unwrap();
        fs::write(&exclude, b"exclude").unwrap();
        let excluded = BTreeSet::from([exclude]);
        let result = create_and_verify(
            &source,
            &output,
            "Photos.7z",
            "pw",
            2_000_000_000,
            &excluded,
            None,
            &TaskControl::default(),
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(result.file_count, 1);
        assert_eq!(result.total_bytes, 4);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archive_uses_only_final_manifest_files_and_rejects_changed_or_missing_content() {
        let root = test_dir();
        let source = root.join("Photos");
        let output = root.join("output");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&output).unwrap();
        let approved = source.join("approved.jpg");
        let unreviewed = source.join("unreviewed.jpg");
        fs::write(&approved, b"approved bytes").unwrap();
        fs::write(&unreviewed, b"new file not in the reviewed set").unwrap();
        let final_manifest = BTreeMap::from([(
            fs::canonicalize(&approved).unwrap(),
            files::sha256_file(&approved).unwrap(),
        )]);

        let result = create_and_verify(
            &source,
            &output,
            "reviewed.7z",
            "pw",
            2_000_000_000,
            &BTreeSet::new(),
            Some(&final_manifest),
            &TaskControl::default(),
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(result.file_count, 1);

        fs::write(&approved, b"changed after review").unwrap();
        let changed_error = create_and_verify(
            &source,
            &output,
            "changed.7z",
            "pw",
            2_000_000_000,
            &BTreeSet::new(),
            Some(&final_manifest),
            &TaskControl::default(),
            |_, _, _| {},
        )
        .unwrap_err();
        assert!(changed_error.contains("Final Manifest"));

        fs::remove_file(&approved).unwrap();
        let missing_error = create_and_verify(
            &source,
            &output,
            "missing.7z",
            "pw",
            2_000_000_000,
            &BTreeSet::new(),
            Some(&final_manifest),
            &TaskControl::default(),
            |_, _, _| {},
        )
        .unwrap_err();
        assert!(missing_error.contains("已不存在"), "{missing_error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn emits_numbered_split_volumes_below_the_selected_size_limit() {
        let root = test_dir();
        let source = root.join("Photos");
        let output = root.join("output");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(source.join("large.mov"), b"more than four bytes").unwrap();

        let result = create_and_verify(
            &source,
            &output,
            "Photos.7z",
            "中文-password",
            100,
            &BTreeSet::new(),
            None,
            &TaskControl::default(),
            |_, _, _| {},
        )
        .unwrap();
        preserve_test_archive(&result);

        assert!(result.volume_count > 1);
        assert!(
            result.volume_paths[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".001")
        );
        assert!(
            result
                .volume_paths
                .iter()
                .all(|path| fs::metadata(path).unwrap().len() <= 100)
        );
        assert!(
            result
                .volume_paths
                .iter()
                .take(result.volume_count - 1)
                .all(|path| fs::metadata(path).unwrap().len() == 100)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "streams a sparse file larger than the default 2,000,000,000-byte volume limit"]
    fn archives_a_single_file_larger_than_the_default_volume_limit() {
        const VOLUME_BYTES: u64 = 2_000_000_000;
        let root = test_dir();
        let source = root.join("Photos");
        let output = root.join("output");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&output).unwrap();
        File::create(source.join("large.MOV"))
            .unwrap()
            .set_len(VOLUME_BYTES + 1)
            .unwrap();

        let result = create_and_verify(
            &source,
            &output,
            "Photos.7z",
            "中文-password",
            VOLUME_BYTES,
            &BTreeSet::new(),
            None,
            &TaskControl::default(),
            |_, _, _| {},
        )
        .unwrap();

        assert!(result.volume_count > 1);
        assert_eq!(
            fs::metadata(&result.volume_paths[0]).unwrap().len(),
            VOLUME_BYTES
        );
        assert!(
            result
                .volume_paths
                .iter()
                .all(|path| fs::metadata(path).unwrap().len() <= VOLUME_BYTES)
        );
        preserve_test_archive(&result);
        fs::remove_dir_all(root).unwrap();
    }
}
