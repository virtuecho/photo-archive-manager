use std::{
    ffi::OsString,
    fs::{self, File, FileTimes, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

use sha2::{Digest, Sha256};

const BUFFER_SIZE: usize = 1024 * 1024;
static NEXT_PARTIAL: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileStamp {
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyReceipt {
    pub size: u64,
    pub source_sha256: String,
    pub destination_sha256: String,
    pub cleanup_warning: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyResult {
    pub source_size: u64,
    pub destination_size: u64,
    pub source_sha256: String,
    pub destination_sha256: String,
    pub matches: bool,
}

/// Stream a regular source file to a new destination, hash both copies, and commit only after they match.
/// The temporary file is created beside the destination; a hard-link commit prevents replacing an existing file.
pub fn copy_verified(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    expected_source: Option<FileStamp>,
    expected_sha256: Option<&str>,
) -> io::Result<CopyReceipt> {
    let source = source.as_ref();
    let destination = destination.as_ref();
    let source_metadata = fs::symlink_metadata(source)?;
    if !source_metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("source is not a regular file: {}", source.display()),
        ));
    }
    let source_file = File::open(source)?;
    let source_metadata = source_file.metadata()?;
    let source_stamp = stamp(&source_metadata);
    if expected_source.is_some_and(|expected| expected != source_stamp) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("source changed after scan: {}", source.display()),
        ));
    }
    if fs::symlink_metadata(destination).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination already exists: {}", destination.display()),
        ));
    }
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;

    let (partial_path, partial_file) = create_partial(
        parent,
        destination.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name")
        })?,
    )?;
    let mut cleanup = RemoveOnDrop(Some(partial_path.clone()));
    let mut partial_file = partial_file;
    let mut source_file = source_file;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut copied = 0u64;
    loop {
        let count = source_file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        partial_file.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        copied = copied.checked_add(count as u64).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "file size exceeds supported range",
            )
        })?;
    }
    partial_file.set_times(
        FileTimes::new().set_modified(source_stamp.modified.unwrap_or(SystemTime::now())),
    )?;
    partial_file.sync_all()?;

    let after_copy = source_file.metadata()?;
    if stamp(&after_copy) != source_stamp || copied != source_stamp.size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("source changed while copying: {}", source.display()),
        ));
    }
    let source_sha256 = to_hex(&hasher.finalize());
    if expected_sha256.is_some_and(|expected| expected != source_sha256) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "source no longer matches the final manifest: {}",
                source.display()
            ),
        ));
    }
    let destination_sha256 = sha256_file(&partial_path)?;
    if source_sha256 != destination_sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("temporary copy hash mismatch: {}", destination.display()),
        ));
    }
    fs::hard_link(&partial_path, destination).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "cannot atomically commit without overwrite to {}: {error}",
                destination.display()
            ),
        )
    })?;
    let cleanup_warning = cleanup.remove_now().err().map(|error| {
        format!(
            "temporary file remains at {}: {error}",
            partial_path.display()
        )
    });
    Ok(CopyReceipt {
        size: copied,
        source_sha256,
        destination_sha256,
        cleanup_warning,
    })
}

/// Re-read both paths and compare their current size and SHA-256 values.
pub fn verify_pair(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> io::Result<VerifyResult> {
    let source = source.as_ref();
    let destination = destination.as_ref();
    if !fs::symlink_metadata(source)?.file_type().is_file()
        || !fs::symlink_metadata(destination)?.file_type().is_file()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source and destination must both be regular files",
        ));
    }
    let mut source_file = File::open(source)?;
    let mut destination_file = File::open(destination)?;
    let source_before = source_file.metadata()?;
    let destination_before = destination_file.metadata()?;
    if !source_before.is_file() || !destination_before.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source and destination must both be regular files",
        ));
    }
    let (source_size, source_sha256) = hash_reader(&mut source_file)?;
    let (destination_size, destination_sha256) = hash_reader(&mut destination_file)?;
    if stamp(&source_before) != stamp(&source_file.metadata()?)
        || stamp(&destination_before) != stamp(&destination_file.metadata()?)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source or destination changed during verification",
        ));
    }
    Ok(VerifyResult {
        source_size,
        destination_size,
        matches: source_size == destination_size && source_sha256 == destination_sha256,
        source_sha256,
        destination_sha256,
    })
}

pub fn sha256_file(path: impl AsRef<Path>) -> io::Result<String> {
    let mut file = File::open(path)?;
    Ok(hash_reader(&mut file)?.1)
}

fn hash_reader(reader: &mut impl Read) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut size = 0u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        size = size.checked_add(count as u64).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "file size exceeds supported range",
            )
        })?;
    }
    Ok((size, to_hex(&hasher.finalize())))
}

fn stamp(metadata: &fs::Metadata) -> FileStamp {
    FileStamp {
        size: metadata.len(),
        modified: metadata.modified().ok(),
    }
}

fn create_partial(parent: &Path, file_name: &std::ffi::OsStr) -> io::Result<(PathBuf, File)> {
    for _ in 0..16 {
        let sequence = NEXT_PARTIAL.fetch_add(1, Ordering::Relaxed);
        let mut name = OsString::from(".");
        name.push(file_name);
        name.push(format!(".{}.{}.partial", std::process::id(), sequence));
        let path = parent.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique temporary file",
    ))
}

struct RemoveOnDrop(Option<PathBuf>);

impl RemoveOnDrop {
    fn remove_now(&mut self) -> io::Result<()> {
        let Some(path) = self.0.take() else {
            return Ok(());
        };
        fs::remove_file(path)
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
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
    use std::io::{Seek, SeekFrom};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let suffix = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "photo-archive-manager-copy-test-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn copy_commits_verified_bytes_preserves_mtime_and_refuses_overwrite() {
        let root = temp_dir();
        let source = root.join("来源/相机.CR3");
        let destination = root.join("分类/相机.CR3");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, vec![0x5a; BUFFER_SIZE + 19]).unwrap();
        let source_metadata = fs::metadata(&source).unwrap();
        let expected = stamp(&source_metadata);

        let receipt = copy_verified(&source, &destination, Some(expected), None).unwrap();
        assert_eq!(receipt.size, expected.size);
        assert_eq!(receipt.source_sha256, receipt.destination_sha256);
        assert_eq!(
            stamp(&fs::metadata(&destination).unwrap()).modified,
            expected.modified
        );
        assert!(verify_pair(&source, &destination).unwrap().matches);
        assert_eq!(
            copy_verified(&source, &destination, Some(expected), None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verification_detects_same_size_target_mutation() {
        let root = temp_dir();
        let source = root.join("source.jpg");
        let destination = root.join("destination.jpg");
        fs::write(&source, b"photo bytes").unwrap();
        copy_verified(&source, &destination, None, None).unwrap();

        let mut changed = OpenOptions::new().write(true).open(&destination).unwrap();
        changed.seek(SeekFrom::Start(0)).unwrap();
        changed.write_all(b"X").unwrap();
        changed.sync_all().unwrap();
        let result = verify_pair(&source, &destination).unwrap();

        assert_eq!(result.source_size, result.destination_size);
        assert!(!result.matches);
        assert_ne!(result.source_sha256, result.destination_sha256);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copy_rejects_a_source_that_changed_since_scan() {
        let root = temp_dir();
        let source = root.join("source.jpg");
        fs::write(&source, b"before").unwrap();
        let expected = stamp(&fs::metadata(&source).unwrap());
        fs::write(&source, b"different size").unwrap();

        assert_eq!(
            copy_verified(&source, root.join("copy.jpg"), Some(expected), None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copy_rejects_content_that_no_longer_matches_the_final_manifest() {
        let root = temp_dir();
        let source = root.join("source.jpg");
        let destination = root.join("copy.jpg");
        fs::write(&source, b"approved bytes").unwrap();
        let approved_sha256 = to_hex(&Sha256::digest(b"approved bytes"));
        fs::write(&source, b"changed bytes").unwrap();

        let error = copy_verified(&source, &destination, None, Some(&approved_sha256)).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("final manifest"));
        assert!(!destination.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
