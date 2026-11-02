use std::path::PathBuf;

use crate::{
    files::{self, CopyReceipt, FileStamp, VerifyResult},
    task_control::TaskControl,
};

#[derive(Clone, Debug)]
pub struct RoutedFile {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub expected_source: FileStamp,
    pub expected_sha256: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CopyItemResult {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub result: Result<CopyReceipt, String>,
}

#[derive(Clone, Debug)]
pub struct VerifyItemResult {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub result: Result<VerifyResult, String>,
}

pub fn copy_batch(
    items: Vec<RoutedFile>,
    control: &TaskControl,
    progress: impl FnMut(usize, usize, u64),
) -> (Vec<CopyItemResult>, bool) {
    copy_batch_inner(items, control, progress, false)
}

pub fn resume_copy_batch(
    items: Vec<RoutedFile>,
    control: &TaskControl,
    progress: impl FnMut(usize, usize, u64),
) -> (Vec<CopyItemResult>, bool) {
    copy_batch_inner(items, control, progress, true)
}

fn copy_batch_inner(
    items: Vec<RoutedFile>,
    control: &TaskControl,
    mut progress: impl FnMut(usize, usize, u64),
    verify_existing: bool,
) -> (Vec<CopyItemResult>, bool) {
    let total = items.len();
    let mut completed = 0;
    let mut copied_bytes = 0u64;
    let mut results = Vec::with_capacity(total);
    let mut cancelled = false;

    for item in items {
        // ponytail: copy pauses between files; mid-file pause needs resumable partial-file state.
        if !control.checkpoint() {
            cancelled = true;
            break;
        }

        let result = copy_one(&item, verify_existing).map_err(|error| error.to_string());
        if let Ok(receipt) = &result {
            copied_bytes = copied_bytes.saturating_add(receipt.size);
        }
        results.push(CopyItemResult {
            source: item.source,
            destination: item.destination,
            result,
        });
        completed += 1;
        progress(completed, total, copied_bytes);
    }
    (results, cancelled)
}

fn copy_one(item: &RoutedFile, verify_existing: bool) -> std::io::Result<CopyReceipt> {
    match files::copy_verified(
        &item.source,
        &item.destination,
        Some(item.expected_source),
        item.expected_sha256.as_deref(),
    ) {
        Ok(receipt) => Ok(receipt),
        Err(error) if verify_existing && error.kind() == std::io::ErrorKind::AlreadyExists => {
            let source_before = files::file_stamp(&item.source)?;
            if source_before != item.expected_source {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("source changed after scan: {}", item.source.display()),
                ));
            }
            let verified = files::verify_pair(&item.source, &item.destination)?;
            if !verified.matches
                || item
                    .expected_sha256
                    .as_deref()
                    .is_some_and(|expected| expected != verified.source_sha256)
                || files::file_stamp(&item.source)? != item.expected_source
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "destination already exists and does not match the scanned source; refusing to overwrite: {}",
                        item.destination.display()
                    ),
                ));
            }
            Ok(CopyReceipt {
                size: verified.source_size,
                source_sha256: verified.source_sha256,
                destination_sha256: verified.destination_sha256,
                cleanup_warning: None,
            })
        }
        Err(error) => Err(error),
    }
}

pub fn verify_batch(
    pairs: Vec<(PathBuf, PathBuf)>,
    control: &TaskControl,
    mut progress: impl FnMut(usize, usize),
) -> (Vec<VerifyItemResult>, bool) {
    let total = pairs.len();
    let mut results = Vec::with_capacity(total);
    let mut cancelled = false;
    for (index, (source, destination)) in pairs.into_iter().enumerate() {
        if !control.checkpoint() {
            cancelled = true;
            break;
        }
        let result = files::verify_pair(&source, &destination).map_err(|error| error.to_string());
        results.push(VerifyItemResult {
            source,
            destination,
            result,
        });
        progress(index + 1, total);
    }
    (results, cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "photo-archive-manager-resume-copy-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn item(source: PathBuf, destination: PathBuf) -> RoutedFile {
        RoutedFile {
            expected_source: files::file_stamp(&source).unwrap(),
            source,
            destination,
            expected_sha256: None,
        }
    }

    #[test]
    fn resume_verifies_an_existing_matching_destination_without_overwriting() {
        let root = temp_dir();
        let source = root.join("source.jpg");
        let destination = root.join("destination.jpg");
        std::fs::write(&source, b"same bytes").unwrap();
        std::fs::write(&destination, b"same bytes").unwrap();
        let before = std::fs::read(&destination).unwrap();

        let (results, cancelled) = resume_copy_batch(
            vec![item(source, destination.clone())],
            &TaskControl::default(),
            |_, _, _| {},
        );

        assert!(!cancelled);
        assert!(results[0].result.is_ok());
        assert_eq!(std::fs::read(&destination).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resume_refuses_to_overwrite_an_existing_mismatching_destination() {
        let root = temp_dir();
        let source = root.join("source.jpg");
        let destination = root.join("destination.jpg");
        std::fs::write(&source, b"source bytes").unwrap();
        std::fs::write(&destination, b"other bytes").unwrap();
        let before = std::fs::read(&destination).unwrap();

        let (results, cancelled) = resume_copy_batch(
            vec![item(source, destination.clone())],
            &TaskControl::default(),
            |_, _, _| {},
        );

        assert!(!cancelled);
        assert!(results[0].result.is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resume_copies_an_item_when_the_destination_was_not_committed() {
        let root = temp_dir();
        let source = root.join("source.jpg");
        let destination = root.join("destination.jpg");
        std::fs::write(&source, b"source bytes").unwrap();

        let (results, cancelled) = resume_copy_batch(
            vec![item(source, destination.clone())],
            &TaskControl::default(),
            |_, _, _| {},
        );

        assert!(!cancelled);
        assert!(results[0].result.is_ok());
        assert_eq!(std::fs::read(&destination).unwrap(), b"source bytes");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resume_rejects_a_source_that_changed_after_the_saved_scan() {
        let root = temp_dir();
        let source = root.join("source.jpg");
        let destination = root.join("destination.jpg");
        std::fs::write(&source, b"original").unwrap();
        std::fs::write(&destination, b"original").unwrap();
        let item = item(source.clone(), destination.clone());
        std::fs::write(&source, b"changed after scan").unwrap();

        let (results, cancelled) =
            resume_copy_batch(vec![item], &TaskControl::default(), |_, _, _| {});

        assert!(!cancelled);
        assert!(results[0].result.is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"original");
        std::fs::remove_dir_all(root).unwrap();
    }
}
