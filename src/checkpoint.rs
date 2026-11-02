use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Debug)]
pub struct SavedReview {
    pub root: PathBuf,
    pub file_sha256: BTreeMap<PathBuf, String>,
    pub excluded_paths: BTreeSet<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SavedCopyItem {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub expected_size: u64,
    pub modified_before_epoch: bool,
    pub modified_seconds: Option<u64>,
    pub modified_nanos: Option<u32>,
    pub expected_sha256: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct PendingCopyCheckpoint {
    version: u8,
    items: Vec<SavedCopyItem>,
}

pub fn save_pending_copy(items: &[SavedCopyItem]) -> io::Result<()> {
    write_pending_copy_to(&pending_copy_path()?, items)
}

fn write_pending_copy_to(path: &Path, items: &[SavedCopyItem]) -> io::Result<()> {
    validate_pending_copy(items)?;
    write_json_atomically(
        path,
        &PendingCopyCheckpoint {
            version: 1,
            items: items.to_vec(),
        },
    )
}

pub fn load_pending_copy() -> io::Result<Option<Vec<SavedCopyItem>>> {
    read_pending_copy_from(&pending_copy_path()?)
}

fn read_pending_copy_from(path: &Path) -> io::Result<Option<Vec<SavedCopyItem>>> {
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "复制任务检查点必须是普通文件",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    let data = match fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let checkpoint: PendingCopyCheckpoint =
        serde_json::from_slice(&data).map_err(io::Error::other)?;
    if checkpoint.version != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "不支持的复制任务检查点版本",
        ));
    }
    validate_pending_copy(&checkpoint.items)?;
    Ok(Some(checkpoint.items))
}

fn validate_pending_copy(items: &[SavedCopyItem]) -> io::Result<()> {
    if items.is_empty() || items.len() > 1_000_000 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "复制任务文件数无效",
        ));
    }
    let mut destinations = BTreeSet::new();
    for item in items {
        if !item.source.is_absolute()
            || !item.destination.is_absolute()
            || item.source == item.destination
            || !destinations.insert(item.destination.clone())
            || item
                .modified_nanos
                .is_some_and(|nanos| nanos >= 1_000_000_000)
            || item.modified_seconds.is_some() != item.modified_nanos.is_some()
            || item.expected_sha256.as_ref().is_some_and(|sha256| {
                sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "复制任务路径、时间戳或 SHA-256 无效",
            ));
        }
    }
    Ok(())
}

pub fn clear_pending_copy() -> io::Result<()> {
    match fs::remove_file(pending_copy_path()?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn save(review: &SavedReview) -> io::Result<()> {
    write_to(&checkpoint_path()?, review)
}

pub fn load() -> io::Result<Option<SavedReview>> {
    let path = checkpoint_path()?;
    read_from(&path)
}

pub fn clear() -> io::Result<()> {
    let path = checkpoint_path()?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn checkpoint_path() -> io::Result<PathBuf> {
    Ok(checkpoint_dir()?.join("task-checkpoint.json"))
}

fn pending_copy_path() -> io::Result<PathBuf> {
    Ok(checkpoint_dir()?.join("unfinished-copy.json"))
}

fn checkpoint_dir() -> io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    let base = env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support"));

    #[cfg(target_os = "windows")]
    let base = env::var_os("APPDATA").map(PathBuf::from);

    #[cfg(target_os = "linux")]
    let base = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local/state"))
        });

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let base = env::var_os("HOME").map(PathBuf::from);

    base.map(|path| path.join("PhotoArchiveManager"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "应用私有数据目录不可用"))
}

fn write_to(path: &Path, review: &SavedReview) -> io::Result<()> {
    validate(review)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "检查点路径没有父目录"))?;
    create_private_dir(parent)?;

    let files = review
        .file_sha256
        .iter()
        .map(|(path, sha256)| {
            serde_json::json!({
                "path": path.to_string_lossy(),
                "sha256": sha256,
            })
        })
        .collect::<Vec<_>>();
    let excluded_paths = review
        .excluded_paths
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    let data = serde_json::to_vec_pretty(&serde_json::json!({
        "version": 1,
        "root": review.root.to_string_lossy(),
        "files": files,
        "excluded_paths": excluded_paths,
    }))
    .map_err(io::Error::other)?;

    write_data_atomically(parent, path, &data)
}

fn write_json_atomically(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "检查点路径没有父目录"))?;
    create_private_dir(parent)?;
    let data = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_data_atomically(parent, path, &data)
}

fn write_data_atomically(parent: &Path, path: &Path, data: &[u8]) -> io::Result<()> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let temp = parent.join(format!(
        ".task-checkpoint-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let result = write_and_replace(&temp, path, &data);
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn write_and_replace(temp: &Path, path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(temp)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);

    if let Err(error) = fs::rename(temp, path) {
        // Windows may refuse to replace an existing file; retain the complete temp
        // until it is ready, then replace the old checkpoint.
        if path.exists() {
            fs::remove_file(path)?;
            fs::rename(temp, path)?;
        } else {
            return Err(error);
        }
    }
    Ok(())
}

fn read_from(path: &Path) -> io::Result<Option<SavedReview>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "检查点必须是普通文件",
        ));
    }
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let value: serde_json::Value = serde_json::from_slice(&data).map_err(io::Error::other)?;
    if value.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "不支持的检查点版本",
        ));
    }
    let root = value
        .get("root")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "检查点缺少来源目录"))?;
    let mut file_sha256 = BTreeMap::new();
    for entry in value
        .get("files")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "检查点文件清单格式错误"))?
    {
        let path = entry
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "检查点文件路径缺失"))?;
        let sha256 = entry
            .get("sha256")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "检查点哈希缺失"))?
            .to_ascii_lowercase();
        if file_sha256.insert(path, sha256).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "检查点含重复路径",
            ));
        }
    }
    let excluded_paths = value
        .get("excluded_paths")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "排除记录格式错误"))?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(PathBuf::from)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "排除路径格式错误"))
        })
        .collect::<io::Result<BTreeSet<_>>>()?;
    let review = SavedReview {
        root,
        file_sha256,
        excluded_paths,
    };
    validate(&review)?;
    Ok(Some(review))
}

fn validate(review: &SavedReview) -> io::Result<()> {
    if !review.root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "检查点来源目录不是绝对路径",
        ));
    }
    for (path, sha256) in &review.file_sha256 {
        if !path.is_absolute()
            || !path.starts_with(&review.root)
            || sha256.len() != 64
            || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "检查点文件或 SHA-256 无效",
            ));
        }
    }
    if review
        .excluded_paths
        .iter()
        .any(|path| !path.is_absolute() || !path.starts_with(&review.root))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "检查点排除路径越出来源目录",
        ));
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        env::temp_dir().join(format!(
            "photo-archive-checkpoint-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn sample(root: &Path) -> SavedReview {
        let file = root.join("photo.jpg");
        SavedReview {
            root: root.to_path_buf(),
            file_sha256: BTreeMap::from([(file.clone(), "a".repeat(64))]),
            excluded_paths: BTreeSet::from([root.join("skip.jpg")]),
        }
    }

    fn sample_copy_item(root: &Path) -> SavedCopyItem {
        SavedCopyItem {
            source: root.join("photo.jpg"),
            destination: root.join("backup/photo.jpg"),
            expected_size: 123,
            modified_before_epoch: false,
            modified_seconds: Some(1_700_000_000),
            modified_nanos: Some(42),
            expected_sha256: Some("a".repeat(64)),
        }
    }

    #[test]
    fn checkpoint_round_trips_review_and_exclusions() {
        let dir = temp_path();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("checkpoint.json");
        let review = sample(&dir);
        write_to(&path, &review).unwrap();
        assert_eq!(
            read_from(&path).unwrap().unwrap().file_sha256,
            review.file_sha256
        );
        assert_eq!(
            read_from(&path).unwrap().unwrap().excluded_paths,
            review.excluded_paths
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn checkpoint_rejects_paths_outside_root() {
        let dir = temp_path();
        let path = dir.join("checkpoint.json");
        fs::create_dir_all(&dir).unwrap();
        let mut review = sample(&dir);
        review.file_sha256 = BTreeMap::from([(PathBuf::from("/tmp/escape.jpg"), "b".repeat(64))]);
        assert_eq!(
            write_to(&path, &review).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pending_copy_checkpoint_round_trips_items() {
        let dir = temp_path();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unfinished-copy.json");
        let items = vec![sample_copy_item(&dir)];
        write_pending_copy_to(&path, &items).unwrap();
        assert_eq!(read_pending_copy_from(&path).unwrap(), Some(items));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pending_copy_checkpoint_rejects_duplicate_destinations() {
        let dir = temp_path();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unfinished-copy.json");
        let first = sample_copy_item(&dir);
        let mut second = first.clone();
        second.source = dir.join("other.jpg");
        assert_eq!(
            write_pending_copy_to(&path, &[first, second])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
