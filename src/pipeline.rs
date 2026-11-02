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
    mut progress: impl FnMut(usize, usize, u64),
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

        let result = files::copy_verified(
            &item.source,
            &item.destination,
            Some(item.expected_source),
            item.expected_sha256.as_deref(),
        )
        .map_err(|error| error.to_string());
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
