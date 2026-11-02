use std::{path::PathBuf, process::Command};

#[cfg(target_os = "macos")]
use std::io::Write;
#[cfg(any(
    target_os = "macos",
    all(target_os = "windows", target_arch = "x86_64"),
    test
))]
use std::{
    fs::{self, OpenOptions},
    path::Path,
};

use serde_json::Value;
#[cfg(any(
    target_os = "macos",
    all(target_os = "windows", target_arch = "x86_64"),
    test
))]
use sha2::{Digest, Sha256};

use crate::task_control::TaskControl;

const FILES_PER_BATCH: usize = 64;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXIFTOOL_SHA256: &str = "224c4384d1df807c14f6ebcb04972ab5b918cac076b9dc4cebc99da981ed0b95";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const EXIFTOOL_SHA256: &str = "cbd028efc28cee50fe5418dd63b14797c76039d636746425cd79f53917954ac6";
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const EXIFTOOL_WINDOWS_ZIP_SHA256: &str =
    "548b1f4e0b713cd2a26824b34080c35db4e2d2edd166468eb38b1d55ada90c2e";
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const EXIFTOOL_WINDOWS_EXE_SHA256: &str =
    "44b336d7596c3ec86577a5ff6d46e8388a56eb69a15f09987d4ac6cc3abbf75e";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXIFTOOL_PAYLOAD: &[u8] = include_bytes!("../resources/exiftool/macos-aarch64/exiftool");
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const EXIFTOOL_PAYLOAD: &[u8] = include_bytes!("../resources/exiftool/macos-x86_64/exiftool");
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const EXIFTOOL_WINDOWS_PAYLOAD: &[u8] =
    include_bytes!("../resources/exiftool/windows-x86_64/exiftool-13.60_64.zip");
#[cfg(all(test, not(all(target_os = "windows", target_arch = "x86_64"))))]
const EXIFTOOL_WINDOWS_TEST_PAYLOAD: &[u8] =
    include_bytes!("../resources/exiftool/windows-x86_64/exiftool-13.60_64.zip");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataStatus {
    Valid,
    Missing,
    Partial,
    Invalid,
    Unsupported,
    ReadError,
}

impl MetadataStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Valid => "VALID",
            Self::Missing => "MISSING",
            Self::Partial => "PARTIAL",
            Self::Invalid => "INVALID",
            Self::Unsupported => "UNSUPPORTED",
            Self::ReadError => "READ_ERROR",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MetadataRecord {
    pub path: PathBuf,
    pub file_type: Option<String>,
    pub gps_status: MetadataStatus,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub gps_source: Option<String>,
    pub gps_zero_zero: bool,
    pub time_status: MetadataStatus,
    pub capture_raw: Option<String>,
    pub capture_normalized: Option<String>,
    pub capture_precision: Option<String>,
    pub capture_has_timezone: bool,
    pub capture_source: Option<String>,
    pub capture_confidence: Option<&'static str>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub focal_length: Option<String>,
    pub exposure_time: Option<String>,
    pub iso: Option<String>,
    pub altitude: Option<String>,
    pub error: Option<String>,
}

impl MetadataRecord {
    pub fn unsupported(path: PathBuf, reason: impl Into<String>) -> Self {
        Self {
            path,
            file_type: None,
            gps_status: MetadataStatus::Unsupported,
            latitude: None,
            longitude: None,
            gps_source: None,
            gps_zero_zero: false,
            time_status: MetadataStatus::Unsupported,
            capture_raw: None,
            capture_normalized: None,
            capture_precision: None,
            capture_has_timezone: false,
            capture_source: None,
            capture_confidence: None,
            make: None,
            model: None,
            lens: None,
            focal_length: None,
            exposure_time: None,
            iso: None,
            altitude: None,
            error: Some(reason.into()),
        }
    }

    fn read_error(path: PathBuf, error: impl Into<String>) -> Self {
        Self {
            path,
            file_type: None,
            gps_status: MetadataStatus::ReadError,
            latitude: None,
            longitude: None,
            gps_source: None,
            gps_zero_zero: false,
            time_status: MetadataStatus::ReadError,
            capture_raw: None,
            capture_normalized: None,
            capture_precision: None,
            capture_has_timezone: false,
            capture_source: None,
            capture_confidence: None,
            make: None,
            model: None,
            lens: None,
            focal_length: None,
            exposure_time: None,
            iso: None,
            altitude: None,
            error: Some(error.into()),
        }
    }
}

pub fn embedded_tool_available() -> bool {
    #[cfg(any(
        target_os = "macos",
        all(target_os = "windows", target_arch = "x86_64")
    ))]
    {
        true
    }
    #[cfg(not(any(
        target_os = "macos",
        all(target_os = "windows", target_arch = "x86_64")
    )))]
    {
        false
    }
}

pub fn embedded_tool_version() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "ExifTool 13.60 · macOS arm64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "ExifTool 13.60 · macOS x86_64"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "ExifTool 13.60 · Windows x86_64"
    }
    #[cfg(not(any(
        target_os = "macos",
        all(target_os = "windows", target_arch = "x86_64")
    )))]
    {
        "当前平台暂无已验证的内置 ExifTool"
    }
}

pub fn inspect_files<F>(
    paths: &[PathBuf],
    control: &TaskControl,
    mut progress: F,
) -> Result<(Vec<MetadataRecord>, bool), String>
where
    F: FnMut(usize, usize),
{
    let tool = EmbeddedExifTool::extract()?;
    let mut results = Vec::with_capacity(paths.len());
    let total = paths.len();

    for batch in paths.chunks(FILES_PER_BATCH) {
        if !control.checkpoint() {
            return Ok((results, true));
        }

        let output = run_exiftool(&tool, batch);

        match output {
            Ok(output) => {
                let parsed = serde_json::from_slice::<Vec<Value>>(&output.stdout);
                match parsed {
                    Ok(entries) => {
                        let no_entries = entries.is_empty();
                        let mut by_path = entries
                            .into_iter()
                            .filter_map(|entry| {
                                let source = get_string(&entry, "SourceFile")?;
                                Some((source, entry))
                            })
                            .collect::<std::collections::BTreeMap<_, _>>();
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        for path in batch {
                            let key = path.to_string_lossy().into_owned();
                            let result = by_path
                                .remove(&key)
                                .map(|entry| parse_record(path.clone(), &entry))
                                .unwrap_or_else(|| {
                                    MetadataRecord::read_error(
                                        path.clone(),
                                        if stderr.trim().is_empty() {
                                            "ExifTool 未为此路径返回结果。".to_string()
                                        } else {
                                            stderr.trim().to_string()
                                        },
                                    )
                                });
                            results.push(result);
                            progress(results.len(), total);
                        }
                        if !output.status.success() && no_entries {
                            return Err(format!(
                                "ExifTool 退出码 {}：{}",
                                output.status,
                                stderr.trim()
                            ));
                        }
                    }
                    Err(error) => {
                        let details = String::from_utf8_lossy(&output.stderr);
                        for path in batch {
                            results.push(MetadataRecord::read_error(
                                path.clone(),
                                format!("无法解析 ExifTool JSON 输出：{error} {}", details.trim()),
                            ));
                            progress(results.len(), total);
                        }
                    }
                }
            }
            Err(error) => {
                for path in batch {
                    results.push(MetadataRecord::read_error(
                        path.clone(),
                        format!("无法启动内置 ExifTool：{error}"),
                    ));
                    progress(results.len(), total);
                }
            }
        }
    }
    Ok((results, false))
}

fn run_exiftool(
    tool: &EmbeddedExifTool,
    batch: &[PathBuf],
) -> std::io::Result<std::process::Output> {
    let mut command = Command::new(&tool.executable);
    command.env_clear();

    #[cfg(target_os = "macos")]
    {
        command
            .env("LC_ALL", "C.UTF-8")
            .env("LANG", "C.UTF-8")
            .args([
                "-json",
                "-n",
                "-G1",
                "-s",
                "-a",
                "-FileType",
                "-GPSLatitude",
                "-GPSLatitudeRef",
                "-GPSLongitude",
                "-GPSLongitudeRef",
                "-GPSCoordinates",
                "-SubSecDateTimeOriginal",
                "-DateTimeOriginal",
                "-OffsetTimeOriginal",
                "-QuickTime:CreateDate",
                "-QuickTime:CreationDate",
                "-QuickTime:GPSCoordinates",
                "-Make",
                "-Model",
                "-LensModel",
                "-FocalLength",
                "-ExposureTime",
                "-ISO",
                "-GPSAltitude",
                "--",
            ])
            .args(batch);
    }

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        let arguments = windows_argfile(batch)?;
        let argfile = tool.directory.join("arguments.txt");
        fs::write(&argfile, arguments)?;
        let system_root = std::env::var_os("SystemRoot").ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Windows SystemRoot 环境变量不可用",
            )
        })?;
        command
            .current_dir(&tool.directory)
            .env("SystemRoot", &system_root)
            .env("WINDIR", &system_root)
            .env("TEMP", &tool.directory)
            .env("TMP", &tool.directory)
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .args(["-charset", "filename=utf8", "-@", "arguments.txt"]);
    }

    #[cfg(not(any(
        target_os = "macos",
        all(target_os = "windows", target_arch = "x86_64")
    )))]
    {
        let _ = (tool, batch);
        return Err(std::io::Error::other(
            "此构建没有对应平台的自包含 ExifTool 有效载荷",
        ));
    }

    command.output()
}

#[cfg(any(all(target_os = "windows", target_arch = "x86_64"), test))]
fn windows_argfile(batch: &[PathBuf]) -> std::io::Result<String> {
    const OPTIONS: &[&str] = &[
        "-json",
        "-n",
        "-G1",
        "-s",
        "-a",
        "-FileType",
        "-GPSLatitude",
        "-GPSLatitudeRef",
        "-GPSLongitude",
        "-GPSLongitudeRef",
        "-GPSCoordinates",
        "-SubSecDateTimeOriginal",
        "-DateTimeOriginal",
        "-OffsetTimeOriginal",
        "-QuickTime:CreateDate",
        "-QuickTime:CreationDate",
        "-QuickTime:GPSCoordinates",
        "-Make",
        "-Model",
        "-LensModel",
        "-FocalLength",
        "-ExposureTime",
        "-ISO",
        "-GPSAltitude",
        "--",
    ];
    let mut lines = OPTIONS
        .iter()
        .map(|line| (*line).to_string())
        .collect::<Vec<_>>();
    for path in batch {
        let path = path.to_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "文件路径不是可表示的 Unicode；请改名后重试",
            )
        })?;
        if path.contains(['\r', '\n']) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Windows ExifTool 参数文件不支持文件名中的换行符；请改名后重试",
            ));
        }
        lines.push(path.to_string());
    }
    Ok(lines.join("\n") + "\n")
}

#[cfg(target_os = "macos")]
struct EmbeddedExifTool {
    directory: PathBuf,
    executable: PathBuf,
}

#[cfg(target_os = "macos")]
impl EmbeddedExifTool {
    fn extract() -> Result<Self, String> {
        let digest = Sha256::digest(EXIFTOOL_PAYLOAD);
        let actual = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if actual != EXIFTOOL_SHA256 {
            return Err(format!("内置 ExifTool 校验失败：SHA-256 {actual}"));
        }

        let directory = private_temp_directory()?;
        let executable = directory.join("exiftool");
        let write_result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&executable)
                .map_err(|error| format!("无法创建 ExifTool 临时文件：{error}"))?;
            file.write_all(EXIFTOOL_PAYLOAD)
                .map_err(|error| format!("无法释放 ExifTool：{error}"))?;
            file.sync_all()
                .map_err(|error| format!("无法同步 ExifTool：{error}"))?;
            set_private_executable_permissions(&executable)
                .map_err(|error| format!("无法设置 ExifTool 权限：{error}"))?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_dir_all(&directory);
            return Err(error);
        }

        Ok(Self {
            directory,
            executable,
        })
    }
}

#[cfg(target_os = "macos")]
impl Drop for EmbeddedExifTool {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
struct EmbeddedExifTool {
    directory: PathBuf,
    executable: PathBuf,
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
impl EmbeddedExifTool {
    fn extract() -> Result<Self, String> {
        let archive_hash = sha256_hex(EXIFTOOL_WINDOWS_PAYLOAD);
        if archive_hash != EXIFTOOL_WINDOWS_ZIP_SHA256 {
            return Err(format!(
                "内置 Windows ExifTool ZIP 校验失败：SHA-256 {archive_hash}"
            ));
        }

        let directory = private_windows_directory()?;
        let executable = directory.join("exiftool.exe");
        let result = (|| {
            extract_windows_payload(EXIFTOOL_WINDOWS_PAYLOAD, &directory)?;
            let executable_bytes = fs::read(&executable)
                .map_err(|error| format!("无法读取释放后的 ExifTool：{error}"))?;
            let executable_hash = sha256_hex(&executable_bytes);
            if executable_hash != EXIFTOOL_WINDOWS_EXE_SHA256 {
                return Err(format!(
                    "释放后的 Windows ExifTool SHA-256 不符：{executable_hash}"
                ));
            }

            let system_root = std::env::var_os("SystemRoot")
                .ok_or_else(|| "Windows SystemRoot 环境变量不可用。".to_string())?;
            let output = Command::new(&executable)
                .current_dir(&directory)
                .env_clear()
                .env("SystemRoot", &system_root)
                .env("WINDIR", &system_root)
                .env("TEMP", &directory)
                .env("TMP", &directory)
                .arg("-ver")
                .output()
                .map_err(|error| format!("无法启动内置 Windows ExifTool：{error}"))?;
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !output.status.success() || version != "13.60" {
                return Err(format!(
                    "内置 Windows ExifTool 版本检查失败：退出码 {}，版本输出 {:?}，错误 {}",
                    output.status,
                    version,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_dir_all(&directory);
            return Err(error);
        }

        Ok(Self {
            directory,
            executable,
        })
    }
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
impl Drop for EmbeddedExifTool {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[cfg(any(all(target_os = "windows", target_arch = "x86_64"), test))]
fn extract_windows_payload(payload: &[u8], directory: &Path) -> Result<(), String> {
    use std::io::Cursor;

    const PACKAGE_ROOT: &str = "exiftool-13.60_64";
    let mut archive = zip::ZipArchive::new(Cursor::new(payload))
        .map_err(|error| format!("无法读取内置 ExifTool ZIP：{error}"))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("无法读取内置 ExifTool ZIP 项：{error}"))?;
        let entry_path = entry
            .enclosed_name()
            .ok_or_else(|| "内置 ExifTool ZIP 含不安全路径。".to_string())?;
        let relative = entry_path
            .strip_prefix(PACKAGE_ROOT)
            .map_err(|_| "内置 ExifTool ZIP 项不在预期目录内。".to_string())?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let is_executable = relative == Path::new("exiftool(-k).exe");
        if !is_executable && !relative.starts_with("exiftool_files") {
            continue;
        }
        let destination = if is_executable {
            directory.join("exiftool.exe")
        } else {
            directory.join(relative)
        };
        if entry.is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|error| format!("无法创建 ExifTool 运行时目录：{error}"))?;
            continue;
        }
        let parent = destination
            .parent()
            .ok_or_else(|| "内置 ExifTool ZIP 项缺少父目录。".to_string())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("无法创建 ExifTool 运行时目录：{error}"))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|error| format!("无法释放 ExifTool 运行时文件：{error}"))?;
        let copied = std::io::copy(&mut entry, &mut output)
            .map_err(|error| format!("释放 ExifTool 运行时文件失败：{error}"))?;
        if copied != entry.size() {
            return Err(format!(
                "内置 ExifTool ZIP 项长度校验失败：{}",
                destination.display()
            ));
        }
        output
            .sync_all()
            .map_err(|error| format!("无法同步 ExifTool 运行时文件：{error}"))?;
    }
    Ok(())
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn private_windows_directory() -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "Windows LOCALAPPDATA 环境变量不可用。".to_string())?
        .join("PhotoArchiveManager/runtime");
    fs::create_dir_all(&root)
        .map_err(|error| format!("无法创建 Windows 私有运行时目录：{error}"))?;
    for _ in 0..8 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(
            "photo-archive-exiftool-{}-{id}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("无法创建 ExifTool 私有临时目录：{error}")),
        }
    }
    Err("无法分配唯一的 ExifTool 临时目录。".into())
}

#[cfg(any(all(target_os = "windows", target_arch = "x86_64"), test))]
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(target_os = "macos")]
fn private_temp_directory() -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    let root = std::env::temp_dir();
    for _ in 0..8 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(
            "photo-archive-exiftool-{}-{id}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => {
                if let Err(error) = set_private_directory_permissions(&path) {
                    let _ = fs::remove_dir(&path);
                    return Err(format!("无法保护 ExifTool 临时目录：{error}"));
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("无法创建 ExifTool 临时目录：{error}")),
        }
    }
    Err("无法分配唯一的 ExifTool 临时目录。".into())
}

#[cfg(target_os = "macos")]
fn set_private_directory_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(target_os = "macos")]
fn set_private_executable_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(any(
    target_os = "macos",
    all(target_os = "windows", target_arch = "x86_64")
)))]
struct EmbeddedExifTool {
    executable: PathBuf,
}

#[cfg(not(any(
    target_os = "macos",
    all(target_os = "windows", target_arch = "x86_64")
)))]
impl EmbeddedExifTool {
    fn extract() -> Result<Self, String> {
        Err("此构建没有对应平台的自包含 ExifTool 有效载荷。".into())
    }
}

fn parse_record(path: PathBuf, entry: &Value) -> MetadataRecord {
    let error = get_string(entry, "Error");
    let file_type = get_string(entry, "FileType");
    if let Some(error) = error {
        return MetadataRecord::read_error(path, error);
    }
    if !file_type.as_deref().is_some_and(is_supported_file_type) {
        return MetadataRecord {
            path,
            file_type,
            gps_status: MetadataStatus::Unsupported,
            latitude: None,
            longitude: None,
            gps_source: None,
            gps_zero_zero: false,
            time_status: MetadataStatus::Unsupported,
            capture_raw: None,
            capture_normalized: None,
            capture_precision: None,
            capture_has_timezone: false,
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
        };
    }

    let (gps_status, latitude, longitude, gps_source) = parse_gps(entry);
    let time = parse_capture_time(entry, file_type.as_deref().unwrap_or_default());
    MetadataRecord {
        path,
        file_type,
        gps_status,
        latitude,
        longitude,
        gps_zero_zero: latitude == Some(0.0) && longitude == Some(0.0),
        gps_source,
        time_status: time.status,
        capture_raw: time.raw,
        capture_normalized: time.normalized,
        capture_precision: time.precision,
        capture_has_timezone: time.has_timezone,
        capture_source: time.source,
        capture_confidence: time.confidence,
        make: get_string(entry, "Make"),
        model: get_string(entry, "Model"),
        lens: get_string(entry, "LensModel"),
        focal_length: get_string(entry, "FocalLength"),
        exposure_time: get_string(entry, "ExposureTime"),
        iso: get_string(entry, "ISO"),
        altitude: get_string(entry, "GPSAltitude"),
        error: None,
    }
}

fn is_supported_file_type(file_type: &str) -> bool {
    matches!(
        file_type.to_ascii_uppercase().as_str(),
        "JPEG"
            | "HEIC"
            | "HEIF"
            | "MOV"
            | "MP4"
            | "M4V"
            | "TIFF"
            | "PNG"
            | "CR2"
            | "CR3"
            | "NEF"
            | "NRW"
            | "ARW"
            | "DNG"
            | "RAF"
            | "ORF"
            | "RW2"
            | "PEF"
            | "SRW"
            | "3FR"
            | "IIQ"
            | "RWL"
    )
}

fn parse_gps(entry: &Value) -> (MetadataStatus, Option<f64>, Option<f64>, Option<String>) {
    let pair = [
        "GPS",
        "QuickTime",
        "Composite",
        "ItemList",
        "Keys",
        "UserData",
        "XMP",
        "XMP-exif",
    ]
    .iter()
    .find_map(|group| {
        let latitude = exact_tag(entry, group, "GPSLatitude");
        let longitude = exact_tag(entry, group, "GPSLongitude");
        (latitude.is_some() || longitude.is_some()).then(|| {
            (
                latitude,
                longitude,
                exact_tag(entry, group, "GPSLatitudeRef"),
                exact_tag(entry, group, "GPSLongitudeRef"),
                format!("{group}:GPSLatitude / GPSLongitude"),
            )
        })
    })
    .or_else(|| {
        let latitude = preferred_tag(entry, "GPSLatitude", &[]);
        let longitude = preferred_tag(entry, "GPSLongitude", &[]);
        (latitude.is_some() || longitude.is_some()).then(|| {
            (
                latitude,
                longitude,
                preferred_tag(entry, "GPSLatitudeRef", &[]),
                preferred_tag(entry, "GPSLongitudeRef", &[]),
                "GPSLatitude / GPSLongitude".to_string(),
            )
        })
    });
    let latitude_value = pair.as_ref().and_then(|pair| pair.0);
    let longitude_value = pair.as_ref().and_then(|pair| pair.1);
    let latitude_ref = pair.as_ref().and_then(|pair| pair.2).map(value_to_string);
    let longitude_ref = pair.as_ref().and_then(|pair| pair.3).map(value_to_string);
    let gps_coordinates = preferred_named_tag(
        entry,
        "GPSCoordinates",
        &[
            "QuickTime",
            "ItemList",
            "Keys",
            "UserData",
            "XMP",
            "XMP-exif",
            "Composite",
        ],
    );
    let source = pair
        .as_ref()
        .map(|pair| pair.4.clone())
        .or_else(|| gps_coordinates.map(|(tag_key, _)| tag_key.to_string()));

    let (latitude, longitude, malformed) = if latitude_value.is_some() || longitude_value.is_some()
    {
        let latitude = latitude_value.and_then(number_value);
        let longitude = longitude_value.and_then(number_value);
        let latitude = apply_direction(latitude, latitude_ref, 'S');
        let longitude = apply_direction(longitude, longitude_ref, 'W');
        (
            latitude,
            longitude,
            (latitude_value.is_some() && latitude.is_none())
                || (longitude_value.is_some() && longitude.is_none()),
        )
    } else if let Some((_, coordinates)) = gps_coordinates {
        match parse_iso6709(&value_to_string(coordinates)) {
            Some((latitude, longitude)) => (Some(latitude), Some(longitude), false),
            None => (None, None, true),
        }
    } else {
        (None, None, false)
    };

    if malformed {
        return (MetadataStatus::Invalid, latitude, longitude, source);
    }
    match (latitude, longitude) {
        (Some(latitude), Some(longitude))
            if latitude.is_finite()
                && longitude.is_finite()
                && (-90.0..=90.0).contains(&latitude)
                && (-180.0..=180.0).contains(&longitude) =>
        {
            (
                MetadataStatus::Valid,
                Some(latitude),
                Some(longitude),
                source,
            )
        }
        (Some(_), Some(_)) => (MetadataStatus::Invalid, latitude, longitude, source),
        (None, None) if source.is_none() => (MetadataStatus::Missing, None, None, None),
        (None, None) => (MetadataStatus::Invalid, None, None, source),
        _ => (MetadataStatus::Partial, latitude, longitude, source),
    }
}

fn exact_tag<'a>(entry: &'a Value, group: &str, tag: &str) -> Option<&'a Value> {
    entry.as_object()?.get(&format!("{group}:{tag}"))
}

fn apply_direction(value: Option<f64>, reference: Option<String>, negative: char) -> Option<f64> {
    let value = value?;
    match reference.as_deref().map(str::trim) {
        Some(reference) if reference.eq_ignore_ascii_case(&negative.to_string()) => {
            Some(-value.abs())
        }
        Some(reference)
            if reference.eq_ignore_ascii_case(if negative == 'S' { "N" } else { "E" }) =>
        {
            Some(value.abs())
        }
        Some(_) => None,
        None => Some(value),
    }
}

fn parse_iso6709(value: &str) -> Option<(f64, f64)> {
    let value = value.trim().trim_end_matches('/');
    if value.is_empty() {
        return None;
    }
    if matches!(value.as_bytes().first(), Some(b'+') | Some(b'-')) {
        let longitude_start = value[1..]
            .char_indices()
            .find(|(_, character)| matches!(character, '+' | '-'))
            .map(|(index, _)| index + 1)?;
        let altitude_start = value[longitude_start + 1..]
            .char_indices()
            .find(|(_, character)| matches!(character, '+' | '-'))
            .map(|(index, _)| index + longitude_start + 1);
        let latitude = value[..longitude_start].parse::<f64>().ok()?;
        let longitude_end = altitude_start.unwrap_or(value.len());
        let longitude = value[longitude_start..longitude_end].parse::<f64>().ok()?;
        return Some((latitude, longitude));
    }

    let mut values = value
        .split(|character: char| character.is_whitespace() || character == ',')
        .filter(|part| !part.is_empty());
    Some((values.next()?.parse().ok()?, values.next()?.parse().ok()?))
}

struct ParsedTime {
    status: MetadataStatus,
    raw: Option<String>,
    normalized: Option<String>,
    precision: Option<String>,
    has_timezone: bool,
    source: Option<String>,
    confidence: Option<&'static str>,
}

fn parse_capture_time(entry: &Value, file_type: &str) -> ParsedTime {
    let original = preferred_named_tag(entry, "SubSecDateTimeOriginal", &["Composite"])
        .or_else(|| preferred_named_tag(entry, "DateTimeOriginal", &["ExifIFD", "Exif", "IFD0"]));
    let (raw, source, confidence) = if let Some((tag, value)) = original {
        (
            Some(value_to_string(value)),
            Some(tag.to_string()),
            Some("高 · EXIF 原始拍摄时间"),
        )
    } else if is_video_type(file_type) {
        let value = preferred_named_tag(entry, "CreationDate", &["QuickTime"])
            .or_else(|| preferred_named_tag(entry, "CreateDate", &["QuickTime"]));
        if let Some((tag, value)) = value {
            (
                Some(value_to_string(value)),
                Some(tag.to_string()),
                Some("中 · QuickTime 日期字段"),
            )
        } else {
            (None, None, None)
        }
    } else {
        (None, None, None)
    };

    let Some(raw) = raw else {
        return ParsedTime {
            status: MetadataStatus::Missing,
            raw: None,
            normalized: None,
            precision: None,
            has_timezone: false,
            source: None,
            confidence: None,
        };
    };
    let offset = get_string(entry, "OffsetTimeOriginal");
    match normalize_datetime(&raw, offset.as_deref()) {
        Some((normalized, precision, has_timezone)) => ParsedTime {
            status: MetadataStatus::Valid,
            raw: Some(raw),
            normalized: Some(normalized),
            precision: Some(precision),
            has_timezone,
            source,
            confidence,
        },
        None => ParsedTime {
            status: MetadataStatus::Invalid,
            raw: Some(raw),
            normalized: None,
            precision: None,
            has_timezone: false,
            source,
            confidence,
        },
    }
}

fn is_video_type(file_type: &str) -> bool {
    matches!(
        file_type.to_ascii_uppercase().as_str(),
        "MOV" | "MP4" | "M4V"
    )
}

fn normalize_datetime(raw: &str, offset_tag: Option<&str>) -> Option<(String, String, bool)> {
    let raw = raw.trim();
    if !raw.is_ascii() {
        return None;
    }
    let split_at = raw.find(['T', ' '])?;
    let mut date = raw[..split_at].to_string();
    let time_and_zone = raw[split_at + 1..].trim();
    if date.len() == 10 && date.as_bytes().get(4) == Some(&b':') {
        date.replace_range(4..5, "-");
        date.replace_range(7..8, "-");
    }
    if date.len() != 10
        || date.as_bytes().get(4) != Some(&b'-')
        || date.as_bytes().get(7) != Some(&b'-')
    {
        return None;
    }
    let year = date[0..4].parse::<u32>().ok()?;
    let month = date[5..7].parse::<u32>().ok()?;
    let day = date[8..10].parse::<u32>().ok()?;
    let max_day = days_in_month(year, month)?;
    if day == 0 || day > max_day {
        return None;
    }

    let (time, zone) = split_timezone(time_and_zone);
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (time, None),
    };
    let mut clock_parts = clock.split(':');
    let hour = clock_parts.next()?.parse::<u32>().ok()?;
    let minute = clock_parts.next()?.parse::<u32>().ok()?;
    let second = clock_parts.next()?.parse::<u32>().ok()?;
    if clock_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let fraction = fraction.unwrap_or("");
    if (time.contains('.') && fraction.is_empty())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let timezone =
        match zone.or_else(|| offset_tag.map(str::trim).filter(|value| !value.is_empty())) {
            Some("Z") | Some("z") => Some("Z".to_string()),
            Some(value) => Some(normalize_offset(value)?),
            None => None,
        };
    let normalized = format!(
        "{date}T{hour:02}:{minute:02}:{second:02}{}{zone}",
        if fraction.is_empty() {
            String::new()
        } else {
            format!(".{fraction}")
        },
        zone = timezone.as_deref().unwrap_or("")
    );
    let precision = if fraction.is_empty() {
        "秒精度".to_string()
    } else {
        format!("小数秒（{} 位）", fraction.len())
    };
    Some((normalized, precision, timezone.is_some()))
}

pub fn capture_epoch_millis(record: &MetadataRecord) -> Option<i64> {
    if record.time_status != MetadataStatus::Valid || !record.capture_has_timezone {
        return None;
    }
    let normalized = record.capture_normalized.as_deref()?;
    let (date, time_and_zone) = normalized.split_once('T')?;
    let (time, zone) = split_timezone(time_and_zone);
    let zone = zone?;
    let offset_minutes = if zone.eq_ignore_ascii_case("Z") {
        0
    } else {
        let sign = if zone.starts_with('-') { -1i64 } else { 1i64 };
        let digits = zone.get(1..)?.replace(':', "");
        if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let hours = digits[..2].parse::<i64>().ok()?;
        let minutes = digits[2..].parse::<i64>().ok()?;
        if hours > 14 || minutes > 59 || (hours == 14 && minutes != 0) {
            return None;
        }
        sign * (hours * 60 + minutes)
    };

    if !date.is_ascii()
        || date.len() != 10
        || date.as_bytes()[4] != b'-'
        || date.as_bytes()[7] != b'-'
    {
        return None;
    }
    let year = date[0..4].parse::<i64>().ok()?;
    let month = date[5..7].parse::<i64>().ok()?;
    let day = date[8..10].parse::<i64>().ok()?;
    if day <= 0 || day > i64::from(days_in_month(year as u32, month as u32)?) {
        return None;
    }

    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (time, None),
    };
    let mut parts = clock.split(':');
    let hour = parts.next()?.parse::<i64>().ok()?;
    let minute = parts.next()?.parse::<i64>().ok()?;
    let second = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let fraction = fraction.unwrap_or("");
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let milliseconds = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<i64>().ok()? * 100,
        2 => fraction.parse::<i64>().ok()? * 10,
        _ => fraction[..3].parse::<i64>().ok()?,
    };
    let days = days_from_civil(year, month, day);
    Some(
        (days * 86_400 + hour * 3_600 + minute * 60 + second - offset_minutes * 60) * 1000
            + milliseconds,
    )
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_from_march = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn split_timezone(value: &str) -> (&str, Option<&str>) {
    if let Some(index) = value.find(['+', '-']).filter(|index| *index >= 8) {
        return (&value[..index], Some(&value[index..]));
    }
    if value.ends_with('Z') || value.ends_with('z') {
        return (&value[..value.len() - 1], Some(&value[value.len() - 1..]));
    }
    (value, None)
}

fn normalize_offset(value: &str) -> Option<String> {
    let value = value.trim();
    let sign = value.as_bytes().first().copied()?;
    if sign != b'+' && sign != b'-' {
        return None;
    }
    let digits = value[1..].replace(':', "");
    if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let hour = digits[..2].parse::<u32>().ok()?;
    let minute = digits[2..].parse::<u32>().ok()?;
    if hour > 14 || minute > 59 || (hour == 14 && minute != 0) {
        return None;
    }
    Some(format!("{}{hour:02}:{minute:02}", sign as char))
}

fn days_in_month(year: u32, month: u32) -> Option<u32> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        _ => return None,
    })
}

fn preferred_tag<'a>(entry: &'a Value, tag: &str, groups: &[&str]) -> Option<&'a Value> {
    preferred_named_tag(entry, tag, groups).map(|(_, value)| value)
}

fn preferred_named_tag<'a>(
    entry: &'a Value,
    tag: &str,
    groups: &[&str],
) -> Option<(&'a str, &'a Value)> {
    let object = entry.as_object()?;
    for group in groups {
        let key = format!("{group}:{tag}");
        if let Some((key, value)) = object.get_key_value(&key) {
            return Some((key.as_str(), value));
        }
    }
    object.iter().find_map(|(key, value)| {
        (key.rsplit(':').next() == Some(tag)).then_some((key.as_str(), value))
    })
}

fn get_value<'a>(entry: &'a Value, tag: &str) -> Option<&'a Value> {
    preferred_tag(entry, tag, &[])
}

fn get_string(entry: &Value, tag: &str) -> Option<String> {
    get_value(entry, tag).map(value_to_string)
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => value.to_string(),
    }
}

fn number_value(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(string) => string.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn gps_respects_south_and_west_and_keeps_zero_zero_valid() {
        let south_west = json!({
            "GPS:GPSLatitude": 33.0,
            "GPS:GPSLatitudeRef": "S",
            "GPS:GPSLongitude": 151.0,
            "GPS:GPSLongitudeRef": "W"
        });
        let (status, latitude, longitude, _) = parse_gps(&south_west);
        assert_eq!(status, MetadataStatus::Valid);
        assert_eq!(latitude, Some(-33.0));
        assert_eq!(longitude, Some(-151.0));

        let zero_zero = json!({"GPS:GPSLatitude": 0.0, "GPS:GPSLongitude": 0.0});
        let (status, latitude, longitude, _) = parse_gps(&zero_zero);
        assert_eq!(status, MetadataStatus::Valid);
        assert_eq!((latitude, longitude), (Some(0.0), Some(0.0)));
    }

    #[test]
    fn quicktime_iso6709_coordinates_are_parsed_and_range_checked() {
        assert_eq!(
            parse_iso6709("+37.3317-122.0307+000.000/"),
            Some((37.3317, -122.0307))
        );
        let tags = json!({"QuickTime:GPSCoordinates": "+37.3317-122.0307+000.000/"});
        let (status, latitude, longitude, _) = parse_gps(&tags);
        assert_eq!(status, MetadataStatus::Valid);
        assert_eq!(latitude, Some(37.3317));
        assert_eq!(longitude, Some(-122.0307));

        let keys_coordinates = json!({"Keys:GPSCoordinates": "37.3317 -122.0307 0"});
        let (status, latitude, longitude, source) = parse_gps(&keys_coordinates);
        assert_eq!(status, MetadataStatus::Valid);
        assert_eq!(latitude, Some(37.3317));
        assert_eq!(longitude, Some(-122.0307));
        assert_eq!(source.as_deref(), Some("Keys:GPSCoordinates"));

        let out_of_range = json!({"GPS:GPSLatitude": 91.0, "GPS:GPSLongitude": 0.0});
        assert_eq!(parse_gps(&out_of_range).0, MetadataStatus::Invalid);
    }

    #[test]
    fn capture_time_keeps_precision_and_does_not_invent_timezone() {
        assert_eq!(
            normalize_datetime("2018:02:21 12:08:56.21", None),
            Some((
                "2018-02-21T12:08:56.21".into(),
                "小数秒（2 位）".into(),
                false
            ))
        );
        assert_eq!(
            normalize_datetime("2024:02:29 23:59:59", Some("+08:00")),
            Some(("2024-02-29T23:59:59+08:00".into(), "秒精度".into(), true))
        );
        assert!(normalize_datetime("2023:02:29 12:00:00", None).is_none());
        assert!(normalize_datetime("2024:01:01 12:00:00", Some("+15:00")).is_none());
    }

    #[test]
    fn capture_time_utc_key_respects_offsets_and_requires_a_timezone() {
        let timed = |value: &str, has_timezone: bool| {
            let mut record = MetadataRecord::read_error(PathBuf::from("sample.jpg"), "test");
            record.time_status = MetadataStatus::Valid;
            record.capture_normalized = Some(value.into());
            record.capture_has_timezone = has_timezone;
            record
        };
        let utc = timed("2018-02-21T12:08:56.21+00:00", true);
        let offset = timed("2018-02-21T20:08:56.210+08:00", true);
        assert_eq!(capture_epoch_millis(&utc), capture_epoch_millis(&offset));
        assert_eq!(
            capture_epoch_millis(&timed("2018-02-21T12:08:56", false)),
            None
        );
    }

    #[test]
    fn verified_windows_exiftool_payload_extracts_with_expected_version() {
        const ARCHIVE_HASH: &str =
            "548b1f4e0b713cd2a26824b34080c35db4e2d2edd166468eb38b1d55ada90c2e";
        const EXE_HASH: &str = "44b336d7596c3ec86577a5ff6d46e8388a56eb69a15f09987d4ac6cc3abbf75e";
        assert_eq!(sha256_hex(EXIFTOOL_WINDOWS_TEST_PAYLOAD), ARCHIVE_HASH);

        let root = std::env::temp_dir().join(format!(
            "photo-archive-windows-payload-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        extract_windows_payload(EXIFTOOL_WINDOWS_TEST_PAYLOAD, &root).unwrap();

        let executable = std::fs::read(root.join("exiftool.exe")).unwrap();
        assert_eq!(sha256_hex(&executable), EXE_HASH);
        let script = std::fs::read_to_string(root.join("exiftool_files/exiftool.pl")).unwrap();
        assert!(script.contains("my $version = '13.60';"));
        assert!(
            root.join("exiftool_files/Licenses_Strawberry_Perl.zip")
                .is_file()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn windows_exiftool_argfile_preserves_unicode_paths_and_rejects_newlines() {
        let path = PathBuf::from("/photos/旅行 2026/IMG 上海.jpg");
        let arguments = windows_argfile(std::slice::from_ref(&path)).unwrap();
        assert!(arguments.starts_with("-json\n"));
        assert!(arguments.ends_with("/photos/旅行 2026/IMG 上海.jpg\n"));
        assert!(windows_argfile(&[PathBuf::from("/photos/bad\n-config")]).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn embedded_exiftool_runs_with_a_cleared_path_and_reports_unsupported_types() {
        let root = std::env::temp_dir().join(format!(
            "photo-archive-metadata-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let file = root.join("普通文本.txt");
        fs::write(&file, "not an image or video\n").unwrap();

        let (results, cancelled) =
            inspect_files(&[file.clone()], &TaskControl::default(), |_, _| {}).unwrap();

        assert!(!cancelled);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_type.as_deref(), Some("TXT"));
        assert_eq!(results[0].gps_status, MetadataStatus::Unsupported);
        assert_eq!(results[0].time_status, MetadataStatus::Unsupported);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "set PAM_EXIFTOOL_TEST_IMAGE to an ExifTool sample image for an integration smoke test"]
    fn embedded_exiftool_reads_real_gps_and_capture_time_tags() {
        let path = PathBuf::from(std::env::var_os("PAM_EXIFTOOL_TEST_IMAGE").unwrap());
        let (results, cancelled) =
            inspect_files(&[path], &TaskControl::default(), |_, _| {}).unwrap();
        assert!(!cancelled);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_type.as_deref(), Some("JPEG"));
        assert_eq!(results[0].gps_status, MetadataStatus::Valid);
        assert_eq!(results[0].time_status, MetadataStatus::Valid);
        assert!(results[0].latitude.is_some());
        assert!(results[0].longitude.is_some());
        assert!(results[0].capture_raw.is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "set PAM_EXIFTOOL_MOV_TEST_IMAGE to a MOV sample with Keys:GPSCoordinates"]
    fn embedded_exiftool_reads_quicktime_gps_and_creation_date() {
        let path = PathBuf::from(std::env::var_os("PAM_EXIFTOOL_MOV_TEST_IMAGE").unwrap());
        let (results, cancelled) =
            inspect_files(&[path], &TaskControl::default(), |_, _| {}).unwrap();
        assert!(!cancelled);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_type.as_deref(), Some("MOV"));
        assert_eq!(results[0].gps_status, MetadataStatus::Valid);
        assert_eq!(results[0].time_status, MetadataStatus::Valid);
        assert_eq!(results[0].longitude, Some(-122.0307));
        assert!(
            results[0]
                .capture_source
                .as_deref()
                .is_some_and(|source| source.starts_with("QuickTime:"))
        );
    }
}
