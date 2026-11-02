use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

use image::{DynamicImage, ImageDecoder, ImageReader, imageops::FilterType};

use crate::{
    files,
    metadata::{MetadataRecord, MetadataStatus, capture_epoch_millis},
    task_control::TaskControl,
};

const MAX_TIME_CANDIDATES: usize = 100_000;

#[derive(Clone, Debug)]
pub struct ExactDuplicateGroup {
    pub sha256: String,
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct TimeCandidate {
    pub first: PathBuf,
    pub second: PathBuf,
    pub first_capture_time: String,
    pub second_capture_time: String,
    pub delta_millis: i64,
    pub first_sha256: String,
    pub second_sha256: String,
    pub phash_distance: Option<u32>,
    pub phash_note: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct DuplicateReport {
    pub exact_groups: Vec<ExactDuplicateGroup>,
    pub time_candidates: Vec<TimeCandidate>,
    pub hash_errors: Vec<(PathBuf, String)>,
    pub file_sha256: BTreeMap<PathBuf, String>,
    pub unknown_timezone_count: usize,
    pub different_device_pairs_skipped: usize,
    pub time_candidates_truncated: bool,
}

struct HashedFile<'a> {
    metadata: &'a MetadataRecord,
    sha256: String,
}

pub fn analyze<F>(
    metadata: &[MetadataRecord],
    threshold_seconds: u32,
    control: &TaskControl,
    mut progress: F,
) -> (DuplicateReport, bool)
where
    F: FnMut(usize, usize),
{
    let mut report = DuplicateReport::default();
    let mut hashed = Vec::with_capacity(metadata.len());
    let mut by_hash = BTreeMap::<String, Vec<PathBuf>>::new();

    for (index, record) in metadata.iter().enumerate() {
        if !control.checkpoint() {
            return (report, true);
        }
        match files::sha256_file(&record.path) {
            Ok(sha256) => {
                by_hash
                    .entry(sha256.clone())
                    .or_default()
                    .push(record.path.clone());
                report
                    .file_sha256
                    .insert(record.path.clone(), sha256.clone());
                hashed.push(HashedFile {
                    metadata: record,
                    sha256,
                });
            }
            Err(error) => report
                .hash_errors
                .push((record.path.clone(), error.to_string())),
        }
        progress(index + 1, metadata.len());
    }

    report.exact_groups = by_hash
        .into_iter()
        .filter_map(|(sha256, mut paths)| {
            (paths.len() > 1).then(|| {
                paths.sort();
                ExactDuplicateGroup { sha256, paths }
            })
        })
        .collect();

    let mut timed = Vec::new();
    for file in &hashed {
        if let Some(time) = capture_epoch_millis(file.metadata) {
            if is_candidate_media(file.metadata) {
                timed.push((time, file));
            }
        } else if file.metadata.time_status == MetadataStatus::Valid
            && is_candidate_media(file.metadata)
            && !file.metadata.capture_has_timezone
        {
            report.unknown_timezone_count += 1;
        }
    }
    timed.sort_by_key(|(time, _)| *time);

    let tolerance = i64::from(threshold_seconds).saturating_mul(1000);
    let mut phashes = HashMap::<PathBuf, Result<u64, String>>::new();
    for first_index in 0..timed.len() {
        if !control.checkpoint() {
            return (report, true);
        }
        let (first_time, first) = timed[first_index];
        for (pair_index, (second_time, second)) in timed.iter().skip(first_index + 1).enumerate() {
            if pair_index % 1024 == 0 && !control.checkpoint() {
                return (report, true);
            }
            let delta_millis = second_time.saturating_sub(first_time);
            if delta_millis > tolerance {
                break;
            }
            if is_video(first.metadata) != is_video(second.metadata) {
                continue;
            }
            if devices_differ(first.metadata, second.metadata) {
                report.different_device_pairs_skipped += 1;
                continue;
            }
            if report.time_candidates.len() >= MAX_TIME_CANDIDATES {
                report.time_candidates_truncated = true;
                break;
            }

            let phash_note = if is_video(first.metadata) {
                Some("视频不参与静态照片 pHash。".into())
            } else {
                let first_hash = phashes
                    .entry(first.metadata.path.clone())
                    .or_insert_with(|| perceptual_hash(&first.metadata.path))
                    .clone();
                let second_hash = phashes
                    .entry(second.metadata.path.clone())
                    .or_insert_with(|| perceptual_hash(&second.metadata.path))
                    .clone();
                match (first_hash, second_hash) {
                    (Ok(first_hash), Ok(second_hash)) => {
                        let hamming = (first_hash ^ second_hash).count_ones();
                        report.time_candidates.push(TimeCandidate {
                            first: first.metadata.path.clone(),
                            second: second.metadata.path.clone(),
                            first_capture_time: first
                                .metadata
                                .capture_normalized
                                .clone()
                                .unwrap_or_default(),
                            second_capture_time: second
                                .metadata
                                .capture_normalized
                                .clone()
                                .unwrap_or_default(),
                            delta_millis,
                            first_sha256: first.sha256.clone(),
                            second_sha256: second.sha256.clone(),
                            phash_distance: Some(hamming),
                            phash_note: None,
                        });
                        continue;
                    }
                    (Err(error), _) | (_, Err(error)) => Some(error.clone()),
                }
            };
            report.time_candidates.push(TimeCandidate {
                first: first.metadata.path.clone(),
                second: second.metadata.path.clone(),
                first_capture_time: first
                    .metadata
                    .capture_normalized
                    .clone()
                    .unwrap_or_default(),
                second_capture_time: second
                    .metadata
                    .capture_normalized
                    .clone()
                    .unwrap_or_default(),
                delta_millis,
                first_sha256: first.sha256.clone(),
                second_sha256: second.sha256.clone(),
                phash_distance: None,
                phash_note,
            });
        }
        if report.time_candidates_truncated {
            break;
        }
    }
    report.time_candidates.sort_by(|a, b| {
        a.delta_millis
            .cmp(&b.delta_millis)
            .then_with(|| a.first.cmp(&b.first))
            .then_with(|| a.second.cmp(&b.second))
    });
    (report, false)
}

fn is_video(record: &MetadataRecord) -> bool {
    record.file_type.as_deref().is_some_and(|file_type| {
        matches!(
            file_type.to_ascii_uppercase().as_str(),
            "MOV" | "MP4" | "M4V"
        )
    })
}

fn is_candidate_media(record: &MetadataRecord) -> bool {
    is_video(record)
        || record.file_type.as_deref().is_some_and(|file_type| {
            matches!(
                file_type.to_ascii_uppercase().as_str(),
                "JPEG"
                    | "HEIC"
                    | "HEIF"
                    | "TIFF"
                    | "PNG"
                    | "CR2"
                    | "CR3"
                    | "NEF"
                    | "ARW"
                    | "DNG"
                    | "RAF"
            )
        })
}

fn devices_differ(first: &MetadataRecord, second: &MetadataRecord) -> bool {
    let first_device = first.make.as_deref().zip(first.model.as_deref());
    let second_device = second.make.as_deref().zip(second.model.as_deref());
    matches!((first_device, second_device), (Some((first_make, first_model)), Some((second_make, second_model)))
        if !first_make.eq_ignore_ascii_case(second_make)
            || !first_model.eq_ignore_ascii_case(second_model))
}

pub fn perceptual_hash(path: &Path) -> Result<u64, String> {
    crate::image_formats::register_heif_decoder();
    let reader = ImageReader::open(path).map_err(|error| error.to_string())?;
    let reader = reader
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|error| error.to_string())?;
    let orientation = decoder.orientation().map_err(|error| error.to_string())?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|error| error.to_string())?;
    image.apply_orientation(orientation);
    let pixels = image.resize_exact(32, 32, FilterType::Lanczos3).to_luma8();
    Ok(phash_32(&pixels))
}

fn phash_32(pixels: &image::GrayImage) -> u64 {
    let mut coefficients = [0.0f64; 64];
    let cosines: [[f64; 8]; 32] = std::array::from_fn(|position| {
        std::array::from_fn(|frequency| {
            ((2 * position + 1) as f64 * frequency as f64 * std::f64::consts::PI / 64.0).cos()
        })
    });
    for vertical in 0..8 {
        for horizontal in 0..8 {
            let mut sum = 0.0;
            for y in 0..32 {
                for x in 0..32 {
                    let pixel = f64::from(pixels.get_pixel(x, y).0[0]);
                    sum += pixel * cosines[x as usize][horizontal] * cosines[y as usize][vertical];
                }
            }
            coefficients[vertical * 8 + horizontal] = sum;
        }
    }
    let mut low_frequency: Vec<f64> = coefficients[1..].to_vec();
    low_frequency.sort_by(f64::total_cmp);
    let median = low_frequency[low_frequency.len() / 2];
    coefficients
        .iter()
        .enumerate()
        .fold(0u64, |hash, (bit, value)| {
            hash | (u64::from(*value > median) << bit)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma};
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    fn metadata_record(path: PathBuf, capture: Option<(&str, bool)>) -> MetadataRecord {
        let (capture_normalized, capture_has_timezone, time_status) = match capture {
            Some((value, has_timezone)) => {
                (Some(value.to_string()), has_timezone, MetadataStatus::Valid)
            }
            None => (None, false, MetadataStatus::Unsupported),
        };
        MetadataRecord {
            path,
            file_type: Some("JPEG".into()),
            gps_status: MetadataStatus::Unsupported,
            latitude: None,
            longitude: None,
            gps_source: None,
            gps_zero_zero: false,
            time_status,
            capture_raw: None,
            capture_normalized,
            capture_precision: None,
            capture_has_timezone,
            capture_source: None,
            capture_confidence: None,
            make: None,
            model: None,
            lens: None,
            focal_length: None,
            exposure_time: None,
            iso: None,
            altitude: None,
            error: None,
        }
    }

    fn test_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "photo-archive-manager-duplicates-{}-{}",
            std::process::id(),
            NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn phash_is_stable_for_identical_images_and_distinguishes_patterns() {
        let stripes = GrayImage::from_fn(32, 32, |x, _| {
            Luma([if (x / 4) % 2 == 0 { 0 } else { 255 }])
        });
        let shifted = GrayImage::from_fn(32, 32, |x, _| {
            Luma([if (x / 4) % 2 == 0 { 255 } else { 0 }])
        });
        assert_eq!(phash_32(&stripes), phash_32(&stripes.clone()));
        assert!((phash_32(&stripes) ^ phash_32(&shifted)).count_ones() > 0);
    }

    #[test]
    fn perceptual_hash_uses_the_full_64_bits() {
        let white = GrayImage::from_pixel(32, 32, Luma([255]));
        assert_ne!(phash_32(&white) & (1u64 << 63), 0);
    }

    #[test]
    fn exact_hashes_group_different_paths_without_capture_metadata() {
        let root = test_dir();
        let first = root.join("one.jpg");
        let second = root.join("two.jpg");
        fs::write(&first, b"same bytes").unwrap();
        fs::write(&second, b"same bytes").unwrap();
        let metadata = [
            metadata_record(first.clone(), None),
            metadata_record(second.clone(), None),
        ];

        let (report, cancelled) = analyze(&metadata, 5, &TaskControl::default(), |_, _| {});

        assert!(!cancelled);
        assert_eq!(report.exact_groups.len(), 1);
        assert_eq!(report.exact_groups[0].paths, [first, second]);
        assert_eq!(report.file_sha256.len(), 2);
        assert!(report.time_candidates.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn time_window_compares_each_pair_without_transitive_closure() {
        let root = test_dir();
        let paths = [root.join("a.jpg"), root.join("b.jpg"), root.join("c.jpg")];
        for (index, path) in paths.iter().enumerate() {
            fs::write(path, format!("image {index}")).unwrap();
        }
        let metadata = [
            metadata_record(paths[0].clone(), Some(("2024-01-01T00:00:00Z", true))),
            metadata_record(paths[1].clone(), Some(("2024-01-01T00:00:04Z", true))),
            metadata_record(paths[2].clone(), Some(("2024-01-01T00:00:08Z", true))),
        ];

        let (report, cancelled) = analyze(&metadata, 5, &TaskControl::default(), |_, _| {});

        assert!(!cancelled);
        assert_eq!(report.time_candidates.len(), 2);
        assert!(
            report
                .time_candidates
                .iter()
                .all(|candidate| candidate.delta_millis == 4_000)
        );
        assert!(
            report.time_candidates.iter().all(|candidate| {
                !(candidate.first == paths[0] && candidate.second == paths[2])
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn time_window_includes_the_exact_threshold_but_skips_unknown_zones() {
        let root = test_dir();
        let paths = [
            root.join("a.jpg"),
            root.join("b.jpg"),
            root.join("unknown.jpg"),
        ];
        for (index, path) in paths.iter().enumerate() {
            fs::write(path, format!("image {index}")).unwrap();
        }
        let metadata = [
            metadata_record(paths[0].clone(), Some(("2024-01-01T00:00:00Z", true))),
            metadata_record(paths[1].clone(), Some(("2024-01-01T00:00:05Z", true))),
            metadata_record(paths[2].clone(), Some(("2024-01-01T00:00:02", false))),
        ];

        let (report, _) = analyze(&metadata, 5, &TaskControl::default(), |_, _| {});

        assert_eq!(report.time_candidates.len(), 1);
        assert_eq!(report.time_candidates[0].delta_millis, 5_000);
        assert_eq!(report.unknown_timezone_count, 1);
        fs::remove_dir_all(root).unwrap();
    }
}
