#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, SystemTime},
};

mod archive;
mod checkpoint;
mod duplicates;
pub mod files;
mod image_formats;
mod metadata;
mod pipeline;
mod task_control;

use crate::{
    files::FileStamp,
    metadata::{MetadataRecord, MetadataStatus},
    pipeline::{CopyItemResult, RoutedFile, VerifyItemResult},
    task_control::TaskControl,
};
use zeroize::Zeroize;

const STAGES: [&str; 8] = [
    "来源选择",
    "文件分类",
    "分类复制",
    "完整性校验",
    "元数据检查",
    "重复检查",
    "文件备份",
    "加密归档",
];

const LICENSE_NOTICES: &str = include_str!("../resources/THIRD_PARTY_NOTICES.md");
const EXIFTOOL_LICENSE: &str = include_str!("../resources/exiftool/Artistic-License-1.0.pod");
const EXIFTOOL_WINDOWS_LICENSES: &str =
    include_str!("../resources/exiftool/windows-x86_64/THIRD_PARTY_LICENSES.md");
const RUST_DEPENDENCY_LICENSES: &str = include_str!("../resources/licenses/RUST_DEPENDENCIES.md");

#[derive(Clone, Debug)]
struct SourceRoot {
    path: PathBuf,
}

#[derive(Clone, Debug)]
struct FileRecord {
    source_id: usize,
    path: PathBuf,
    relative_path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
    extension: String,
}

#[derive(Clone, Copy, Debug, Default)]
struct ScanProgress {
    files: usize,
    bytes: u64,
    directories: usize,
}

#[derive(Debug, Default)]
struct ScanSummary {
    files: Vec<FileRecord>,
    bytes: u64,
    skipped: usize,
    error_count: usize,
    error_samples: Vec<String>,
    by_extension: BTreeMap<String, usize>,
}

struct CopyPlan {
    items: Vec<RoutedFile>,
    filtered: usize,
    unmapped: usize,
    overlap: usize,
    duplicate_targets: usize,
    existing_targets: usize,
}

enum ScanMessage {
    Progress(ScanProgress),
    Complete(ScanSummary),
    Cancelled(ScanProgress),
}

#[derive(Clone, Debug)]
struct ExtensionRoute {
    extension: String,
    destination: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkKind {
    Copy,
    Verify,
    Metadata,
    Duplicates,
    Backup,
    Archive,
}

enum MetadataInput {
    Files(Vec<PathBuf>),
    Directory(PathBuf),
}

enum WorkMessage {
    CopyProgress {
        completed: usize,
        total: usize,
        bytes: u64,
    },
    CopyComplete {
        results: Vec<CopyItemResult>,
        cancelled: bool,
    },
    VerifyProgress {
        completed: usize,
        total: usize,
    },
    VerifyComplete {
        results: Vec<VerifyItemResult>,
        cancelled: bool,
        coverage: Option<VerifyCoverage>,
    },
    MetadataProgress {
        completed: usize,
        total: usize,
    },
    MetadataComplete {
        results: Vec<MetadataRecord>,
        cancelled: bool,
        error: Option<String>,
    },
    DuplicateProgress {
        completed: usize,
        total: usize,
    },
    DuplicateComplete {
        report: duplicates::DuplicateReport,
        cancelled: bool,
        error: Option<String>,
    },
    BackupProgress {
        completed: usize,
        total: usize,
        bytes: u64,
    },
    BackupComplete {
        copies: Vec<CopyItemResult>,
        verification: Vec<VerifyItemResult>,
        cancelled: bool,
        error: Option<String>,
        verify_only: bool,
    },
    ArchiveProgress {
        completed: usize,
        total: usize,
        bytes: u64,
    },
    ArchiveComplete {
        result: Result<archive::ArchiveResult, String>,
        cancelled: bool,
    },
}

#[derive(Clone, Debug)]
struct VerifyCoverage {
    source_count: usize,
    target_count: usize,
    missing: Vec<String>,
    extra: Vec<String>,
    scan_errors: usize,
}

#[derive(Clone, Copy)]
struct DuplicateOptions {
    time_threshold_seconds: u32,
    phash_threshold: u32,
}

#[derive(Clone, Debug, Default)]
struct BackupMapping {
    source: Option<PathBuf>,
    targets: Vec<PathBuf>,
}

impl Default for DuplicateOptions {
    fn default() -> Self {
        Self {
            time_threshold_seconds: 5,
            phash_threshold: 10,
        }
    }
}

#[derive(Clone, Copy)]
struct ArchiveOptions {
    volume_mb: u32,
}

struct PreviewPixels {
    image: image::RgbaImage,
}

#[derive(Clone, Debug)]
struct FinalManifest {
    root: PathBuf,
    file_sha256: BTreeMap<PathBuf, String>,
}

impl FinalManifest {
    fn from_review(
        root: PathBuf,
        report: &duplicates::DuplicateReport,
        excluded_paths: &std::collections::BTreeSet<PathBuf>,
    ) -> Self {
        let file_sha256 = report
            .file_sha256
            .iter()
            .filter(|(path, _)| !excluded_paths.contains(*path))
            .map(|(path, sha256)| (path.clone(), sha256.clone()))
            .collect();
        Self { root, file_sha256 }
    }

    fn applies_to(&self, source: &Path) -> bool {
        source.starts_with(&self.root) || self.root.starts_with(source)
    }

    fn sha256_for(&self, path: &Path) -> Option<&str> {
        self.file_sha256.get(path).map(String::as_str)
    }
}

impl Default for ArchiveOptions {
    fn default() -> Self {
        Self { volume_mb: 2000 }
    }
}

#[derive(Default)]
struct PhotoArchiveApp {
    stage: usize,
    roots: Vec<SourceRoot>,
    selected_root: Option<usize>,
    scan_receiver: Option<Receiver<ScanMessage>>,
    cancel_scan: Option<TaskControl>,
    scanning: bool,
    scan_progress: ScanProgress,
    last_scan: Option<ScanSummary>,
    routes: Vec<ExtensionRoute>,
    new_extension: String,
    include_system_files: bool,
    copy_results: Vec<CopyItemResult>,
    copy_planned: usize,
    verify_results: Vec<VerifyItemResult>,
    verify_coverage: Option<VerifyCoverage>,
    direct_verify_source: Option<PathBuf>,
    direct_verify_target: Option<PathBuf>,
    metadata_directory: Option<PathBuf>,
    metadata_results: Vec<MetadataRecord>,
    duplicate_directory: Option<PathBuf>,
    duplicate_options: DuplicateOptions,
    duplicate_report: Option<duplicates::DuplicateReport>,
    excluded_final_paths: std::collections::BTreeSet<PathBuf>,
    final_manifest: Option<FinalManifest>,
    review_checkpoint_saved: bool,
    pending_copy_resume: Option<Vec<RoutedFile>>,
    candidate_preview_paths: Option<(PathBuf, PathBuf)>,
    candidate_preview_receiver:
        Option<Receiver<(Result<PreviewPixels, String>, Result<PreviewPixels, String>)>>,
    candidate_preview_loading: bool,
    candidate_preview_textures: Option<(
        Result<eframe::egui::TextureHandle, String>,
        Result<eframe::egui::TextureHandle, String>,
    )>,
    backup_mappings: Vec<BackupMapping>,
    pending_backup_resume: Option<Vec<BackupMapping>>,
    backup_copies: Vec<CopyItemResult>,
    backup_verification: Vec<VerifyItemResult>,
    archive_source: Option<PathBuf>,
    archive_output_directory: Option<PathBuf>,
    pending_archive_resume: Option<checkpoint::SavedArchivePlan>,
    archive_name: String,
    archive_name_custom: bool,
    archive_password: String,
    archive_confirmation: String,
    archive_show_password: bool,
    archive_options: ArchiveOptions,
    work_receiver: Option<Receiver<WorkMessage>>,
    work_control: Option<TaskControl>,
    work_kind: Option<WorkKind>,
    work_running: bool,
    work_completed: usize,
    work_total: usize,
    work_bytes: u64,
    status: String,
    show_handoff: bool,
    show_licenses: bool,
    license_choice: usize,
    handoff_title: String,
    handoff_description: String,
    show_switch_prompt: bool,
    pending_stage: Option<usize>,
}

impl eframe::App for PhotoArchiveApp {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.receive_scan_updates();
        self.receive_work_updates();

        eframe::egui::CentralPanel::default().show(ui, |ui| {
            let available = ui.available_size();
            let content_height = (available.y - 145.0).max(0.0);
            ui.allocate_ui_with_layout(
                eframe::egui::vec2(available.x, content_height),
                eframe::egui::Layout::top_down(eframe::egui::Align::Min),
                |ui| {
                    eframe::egui::ScrollArea::vertical()
                        .id_salt("stage-content")
                        .auto_shrink([true, false])
                        .show(ui, |ui| {
                            ui.heading(format!(
                                "阶段 {} / 8：{}",
                                self.stage + 1,
                                STAGES[self.stage]
                            ));
                            ui.add_space(12.0);
                            if self.stage == 0 {
                                self.source_selection(ui);
                            } else if self.stage == 1 {
                                self.routing_stage(ui);
                            } else if self.stage == 2 {
                                self.copy_stage(ui);
                            } else if self.stage == 3 {
                                self.verification_stage(ui);
                            } else if self.stage == 4 {
                                self.metadata_stage(ui);
                            } else if self.stage == 5 {
                                self.duplicates_stage(ui);
                            } else if self.stage == 6 {
                                self.backup_stage(ui);
                            } else if self.stage == 7 {
                                self.archive_stage(ui);
                            } else {
                                ui.label("未知阶段状态；请从底部导航重新选择阶段。");
                            }
                        });
                },
            );
            ui.separator();
            self.footer(ui, &ctx);
        });

        self.candidate_preview_dialog(&ctx);
        if self.show_handoff {
            self.handoff_dialog(&ctx);
        }
        if self.show_switch_prompt {
            self.stage_switch_dialog(&ctx);
        }
        if self.show_licenses {
            self.license_dialog(&ctx);
        }
        if self.scanning
            || self.work_running
            || self.candidate_preview_loading
            || self.show_handoff
            || self.show_switch_prompt
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

impl PhotoArchiveApp {
    fn source_selection(&mut self, ui: &mut eframe::egui::Ui) {
        ui.label("添加一个或多个来源目录。扫描只读取常规文件，不会修改来源。符号链接不会被跟随。 ");
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.scanning && !self.work_running,
                    eframe::egui::Button::new("添加来源文件夹…"),
                )
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    match insert_source_root(&mut self.roots, path.clone()) {
                        Ok(true) => {
                            let canonical = fs::canonicalize(path).ok();
                            self.selected_root = self
                                .roots
                                .iter()
                                .position(|root| Some(&root.path) == canonical.as_ref());
                            self.invalidate_copy_plan();
                            self.last_scan = None;
                            self.status = "已添加来源目录；旧扫描与依赖结果已清除。".into();
                        }
                        Ok(false) => {
                            self.status = "该目录已包含在现有来源中，无需重复添加。".into()
                        }
                        Err(error) => self.status = error,
                    }
                }
            }
            let can_remove = self
                .selected_root
                .is_some_and(|index| index < self.roots.len());
            if ui
                .add_enabled(
                    can_remove && !self.scanning && !self.work_running,
                    eframe::egui::Button::new("移除选中目录"),
                )
                .clicked()
            {
                if let Some(index) = self.selected_root.take() {
                    self.roots.remove(index);
                    self.invalidate_copy_plan();
                    self.last_scan = None;
                    self.status = "已移除来源目录；旧扫描与依赖结果已清除。".into();
                }
            }
            if ui
                .add_enabled(
                    !self.roots.is_empty() && !self.scanning && !self.work_running,
                    eframe::egui::Button::new("扫描来源"),
                )
                .clicked()
            {
                self.start_scan();
            }
        });

        ui.add_space(8.0);
        ui.label("来源目录");
        eframe::egui::ScrollArea::vertical()
            .id_salt("source-roots")
            .max_height(180.0)
            .show(ui, |ui| {
                if self.roots.is_empty() {
                    ui.weak("尚未添加目录");
                }
                let mut selected_index = None;
                for (index, root) in self.roots.iter().enumerate() {
                    if ui
                        .selectable_label(
                            self.selected_root == Some(index),
                            root.path.display().to_string(),
                        )
                        .clicked()
                    {
                        selected_index = Some(index);
                    }
                }
                if let Some(index) = selected_index {
                    self.selected_root = Some(index);
                }
            });

        ui.add_space(10.0);
        if self.scanning {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!(
                    "正在扫描：{} 个文件，{}，已访问 {} 个目录",
                    self.scan_progress.files,
                    format_bytes(self.scan_progress.bytes),
                    self.scan_progress.directories
                ));
            });
        }
        if let Some(summary) = &self.last_scan {
            ui.separator();
            ui.heading("本次扫描结果");
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("文件：{}", summary.files.len()));
                ui.label(format!("总大小：{}", format_bytes(summary.bytes)));
                ui.label(format!("跳过：{}", summary.skipped));
                ui.label(format!("无法读取：{}", summary.error_count));
            });
            ui.add_space(4.0);
            ui.label("扩展名统计");
            eframe::egui::ScrollArea::vertical()
                .id_salt("scan-summary")
                .max_height(260.0)
                .show(ui, |ui| {
                    for (extension, count) in &summary.by_extension {
                        ui.label(format!("{extension}: {count}"));
                    }
                    if summary.by_extension.is_empty() {
                        ui.weak("没有可读取的常规文件");
                    }
                    ui.separator();
                    ui.label("清单预览（前 8 项）");
                    for file in summary.files.iter().take(8) {
                        let modified = file
                            .modified
                            .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
                            .map(|time| time.as_secs().to_string())
                            .unwrap_or_else(|| "不可用".into());
                        ui.label(format!(
                            "来源 {} · {} · {} · {} · 修改时间 Unix {}",
                            file.source_id + 1,
                            file.relative_path.display(),
                            file.extension,
                            format_bytes(file.size),
                            modified
                        ));
                        ui.weak(file.path.display().to_string());
                    }
                });
            if let Some(error) = summary.error_samples.first() {
                ui.colored_label(
                    eframe::egui::Color32::from_rgb(140, 85, 0),
                    format!("示例错误：{error}"),
                );
            }
        }
        if !self.status.is_empty() {
            ui.add_space(6.0);
            ui.label(&self.status);
        }
    }

    fn routing_stage(&mut self, ui: &mut eframe::egui::Ui) {
        ui.label("为每种已扫描扩展名选择最终目标文件夹。多种扩展名可指向同一文件夹；未知扩展名也会列出，不会自动丢弃。");
        let Some(summary) = &self.last_scan else {
            ui.add_space(8.0);
            ui.weak("此会话还没有来源清单。请先在①添加目录并扫描，然后返回本阶段。");
            return;
        };
        if summary.files.is_empty() {
            ui.add_space(8.0);
            ui.weak("扫描清单为空，没有可路由的文件。");
            return;
        }

        let mut routes_changed = false;
        ui.horizontal(|ui| {
            ui.label("新增扩展名");
            ui.add_enabled(
                !self.work_running,
                eframe::egui::TextEdit::singleline(&mut self.new_extension).hint_text("例如 XMP"),
            );
            if ui
                .add_enabled(!self.work_running, eframe::egui::Button::new("添加规则"))
                .clicked()
            {
                let extension = self
                    .new_extension
                    .trim()
                    .trim_start_matches('.')
                    .to_ascii_uppercase();
                if !extension.is_empty()
                    && !self.routes.iter().any(|route| route.extension == extension)
                {
                    self.routes.push(ExtensionRoute {
                        extension,
                        destination: None,
                    });
                    self.routes.sort_by(|a, b| a.extension.cmp(&b.extension));
                    self.new_extension.clear();
                    routes_changed = true;
                }
            }
        });
        ui.add_space(6.0);
        let required_extensions: std::collections::BTreeSet<String> = summary
            .files
            .iter()
            .filter(|file| !is_system_junk(&file.relative_path))
            .map(|file| file.extension.clone())
            .collect();
        let mut selected_route = None;
        eframe::egui::ScrollArea::vertical()
            .id_salt("routing-rules")
            .max_height(420.0)
            .show(ui, |ui| {
                for (index, route) in self.routes.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        let count = summary
                            .files
                            .iter()
                            .filter(|file| {
                                file.extension == route.extension
                                    && !is_system_junk(&file.relative_path)
                            })
                            .count();
                        ui.label(format!("{} · {} 个文件", route.extension, count));
                        ui.label(
                            route
                                .destination
                                .as_ref()
                                .map(|path| path.display().to_string())
                                .unwrap_or_else(|| "未设置目标".into()),
                        );
                        if ui
                            .add_enabled(
                                !self.work_running,
                                eframe::egui::Button::new("选择目标文件夹…"),
                            )
                            .clicked()
                        {
                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                match fs::canonicalize(&path) {
                                    Ok(path) => {
                                        if route.destination.as_ref() != Some(&path) {
                                            route.destination = Some(path);
                                            routes_changed = true;
                                        }
                                    }
                                    Err(error) => {
                                        self.status =
                                            format!("无法解析目标目录 {}：{error}", path.display());
                                    }
                                }
                            }
                        }
                        if !required_extensions.contains(&route.extension)
                            && ui
                                .add_enabled(!self.work_running, eframe::egui::Button::new("移除"))
                                .clicked()
                        {
                            selected_route = Some(index);
                        }
                    });
                }
            });
        if let Some(index) = selected_route {
            self.routes.remove(index);
            routes_changed = true;
        }
        ui.add_space(8.0);
        let missing = required_extensions
            .iter()
            .filter(|extension| {
                !self
                    .routes
                    .iter()
                    .any(|route| route.extension == **extension && route.destination.is_some())
            })
            .count();
        if missing == 0 {
            ui.colored_label(
                eframe::egui::Color32::DARK_GREEN,
                "所有已扫描扩展名都已映射。",
            );
        } else {
            ui.colored_label(
                eframe::egui::Color32::DARK_RED,
                format!(
                    "还有 {missing} 种已扫描扩展名未设置目标；这些文件会保留在清单并跳过复制。"
                ),
            );
        }
        if routes_changed {
            self.invalidate_copy_plan();
        }
    }

    fn copy_stage(&mut self, ui: &mut eframe::egui::Ui) {
        ui.label("复制只写入所选目标目录，不修改来源；新复制遇到已有目标会失败且不覆盖，恢复任务只会把重新校验一致的目标认作已完成。");
        if self.last_scan.is_none() {
            if let Some(items) = self.pending_copy_resume.as_ref() {
                ui.add_space(8.0);
                ui.label(format!(
                    "发现上次未完成的文件复制：{} 项。恢复时会重新读取源与已有目标；不匹配的目标不会覆盖。",
                    items.len()
                ));
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !self.work_running,
                            eframe::egui::Button::new("恢复上次复制任务"),
                        )
                        .clicked()
                    {
                        self.resume_pending_copy();
                    }
                    if ui
                        .add_enabled(
                            !self.work_running,
                            eframe::egui::Button::new("丢弃恢复记录"),
                        )
                        .clicked()
                    {
                        self.discard_pending_copy();
                    }
                });
            } else {
                ui.add_space(8.0);
                ui.weak("此会话还没有来源清单。请先在①添加目录并扫描，然后返回本阶段。");
            }
            self.copy_results_ui(ui);
            return;
        }
        if ui
            .add_enabled(
                !self.work_running,
                eframe::egui::Checkbox::new(
                    &mut self.include_system_files,
                    "包括系统垃圾文件（.DS_Store、._*、Thumbs.db、desktop.ini 等）",
                ),
            )
            .changed()
        {
            self.invalidate_copy_plan();
        }
        let plan = self.copy_plan();
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("可复制：{}", plan.items.len()));
            ui.label(format!("系统文件过滤：{}", plan.filtered));
            ui.label(format!("未映射：{}", plan.unmapped));
            ui.label(format!("路径重叠：{}", plan.overlap));
            ui.label(format!("目标路径冲突：{}", plan.duplicate_targets));
            ui.label(format!("目标已存在：{}", plan.existing_targets));
        });
        if plan.overlap > 0 || plan.duplicate_targets > 0 {
            ui.colored_label(
                eframe::egui::Color32::DARK_RED,
                "检测到来源/目标重叠或多个来源映射到同一目标路径。请调整目录或路由后再开始。",
            );
        }
        if plan.unmapped > 0 {
            ui.colored_label(
                eframe::egui::Color32::from_rgb(140, 85, 0),
                "未映射文件不会被复制。可返回②为这些扩展名指定目标目录。",
            );
        }
        let successful: std::collections::BTreeSet<(PathBuf, PathBuf)> = self
            .copy_results
            .iter()
            .filter(|item| item.result.is_ok())
            .map(|item| (item.source.clone(), item.destination.clone()))
            .collect();
        let retry_items: Vec<RoutedFile> = plan
            .items
            .iter()
            .filter(|item| !successful.contains(&(item.source.clone(), item.destination.clone())))
            .cloned()
            .collect();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let blocked = self.work_running
                || plan.items.is_empty()
                || plan.overlap > 0
                || plan.duplicate_targets > 0;
            if self.copy_results.is_empty() {
                if ui
                    .add_enabled(!blocked, eframe::egui::Button::new("开始安全复制"))
                    .clicked()
                {
                    self.start_copy(plan.items.clone(), false, false);
                }
            }
            if !self.copy_results.is_empty()
                && ui
                    .add_enabled(
                        !self.work_running
                            && !retry_items.is_empty()
                            && plan.overlap == 0
                            && plan.duplicate_targets == 0,
                        eframe::egui::Button::new("继续 / 重试未通过项"),
                    )
                    .clicked()
            {
                self.start_copy(retry_items.clone(), true, false);
            }
            if self.work_running
                && self.work_kind == Some(WorkKind::Copy)
                && ui.button("取消（完成当前文件后停止）").clicked()
            {
                if let Some(control) = &self.work_control {
                    control.cancel();
                }
            }
        });
        self.copy_results_ui(ui);
    }

    fn copy_results_ui(&self, ui: &mut eframe::egui::Ui) {
        if self.copy_results.is_empty() {
            return;
        }
        let passed = self
            .copy_results
            .iter()
            .filter(|item| item.result.is_ok())
            .count();
        let failed = self.copy_results.len() - passed;
        ui.separator();
        ui.label(format!(
            "复制结果：成功 {passed}，失败 {failed}，计划 {}。",
            self.copy_planned
        ));
        for item in self.copy_results.iter().take(12) {
            if let Err(error) = &item.result {
                ui.colored_label(
                    eframe::egui::Color32::DARK_RED,
                    format!(
                        "{} → {}：{error}",
                        item.source.display(),
                        item.destination.display()
                    ),
                );
            }
        }
        if self.copy_results.len() > 12 {
            ui.weak(format!("另有 {} 项未展开。", self.copy_results.len() - 12));
        }
    }

    fn verification_stage(&mut self, ui: &mut eframe::egui::Ui) {
        ui.label("重新读取源文件和目标文件，逐项比较文件大小与 SHA-256。复制时生成的源哈希不会代替目标复读。");
        let valid_pairs = self
            .copy_results
            .iter()
            .filter(|item| item.result.is_ok())
            .count();
        ui.horizontal(|ui| {
            ui.label(format!("可复核的复制项：{valid_pairs}"));
            if ui
                .add_enabled(
                    !self.work_running && valid_pairs > 0,
                    eframe::egui::Button::new("重新校验已复制文件"),
                )
                .clicked()
            {
                self.start_verify();
            }
        });
        ui.add_space(8.0);
        ui.separator();
        ui.label("独立校验：选择一个来源文件夹和一个整理后文件夹，按相对路径匹配并核对文件数、缺失/多余文件及 SHA-256。");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.work_running,
                    eframe::egui::Button::new("选择来源文件夹…"),
                )
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    let selected = fs::canonicalize(&path).ok();
                    if self.direct_verify_source != selected {
                        self.direct_verify_source = selected;
                        self.clear_verify_results();
                    }
                }
            }
            if ui
                .add_enabled(
                    !self.work_running,
                    eframe::egui::Button::new("选择目标文件夹…"),
                )
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    let selected = fs::canonicalize(&path).ok();
                    if self.direct_verify_target != selected {
                        self.direct_verify_target = selected;
                        self.clear_verify_results();
                    }
                }
            }
            if ui
                .add_enabled(
                    !self.work_running
                        && self.direct_verify_source.is_some()
                        && self.direct_verify_target.is_some(),
                    eframe::egui::Button::new("独立校验这两个文件夹"),
                )
                .clicked()
            {
                self.start_direct_verify();
            }
        });
        if let Some(path) = &self.direct_verify_source {
            ui.weak(format!("来源：{}", path.display()));
        }
        if let Some(path) = &self.direct_verify_target {
            ui.weak(format!("目标：{}", path.display()));
        }
        if self.work_running && self.work_kind == Some(WorkKind::Verify) {
            if ui.button("取消校验").clicked() {
                if let Some(control) = &self.work_control {
                    control.cancel();
                }
            }
        }
        if let Some(coverage) = &self.verify_coverage {
            ui.separator();
            ui.label(format!(
                "目录清单：来源 {} 项，目标 {} 项；缺失 {}，多余 {}，无法读取 {}。",
                coverage.source_count,
                coverage.target_count,
                coverage.missing.len(),
                coverage.extra.len(),
                coverage.scan_errors
            ));
            for missing in coverage.missing.iter().take(5) {
                ui.colored_label(
                    eframe::egui::Color32::DARK_RED,
                    format!("目标缺失：{missing}"),
                );
            }
            for extra in coverage.extra.iter().take(5) {
                ui.colored_label(
                    eframe::egui::Color32::from_rgb(140, 85, 0),
                    format!("目标多余：{extra}"),
                );
            }
        }
        if !self.verify_results.is_empty() {
            ui.weak(&self.status);
            let matching = self
                .verify_results
                .iter()
                .filter(|item| item.result.as_ref().is_ok_and(|result| result.matches))
                .count();
            let failed = self.verify_results.len() - matching;
            ui.separator();
            ui.label(format!(
                "复读校验：一致 {matching}，不一致或读取失败 {failed}。"
            ));
            for item in self.verify_results.iter().take(12) {
                match &item.result {
                    Ok(result) if result.matches => {}
                    Ok(result) => {
                        ui.colored_label(
                            eframe::egui::Color32::DARK_RED,
                            format!(
                                "哈希/大小不一致：{} → {}（源 {} B，目标 {} B）",
                                item.source.display(),
                                item.destination.display(),
                                result.source_size,
                                result.destination_size
                            ),
                        );
                    }
                    Err(error) => {
                        ui.colored_label(
                            eframe::egui::Color32::DARK_RED,
                            format!(
                                "{} → {}：{error}",
                                item.source.display(),
                                item.destination.display()
                            ),
                        );
                    }
                };
            }
        }
    }

    fn metadata_stage(&mut self, ui: &mut eframe::egui::Ui) {
        ui.label("用随程序内置的 ExifTool 重新读取当前文件；不使用文件系统时间代替拍摄时间，也不改写照片。");
        ui.weak(metadata::embedded_tool_version());
        if !metadata::embedded_tool_available() {
            ui.colored_label(
                eframe::egui::Color32::DARK_RED,
                "此构建没有可验证的自包含 ExifTool。阶段⑤在本平台不可用；不会调用系统 PATH 中的工具。",
            );
            return;
        }

        let copied_files = self
            .copy_results
            .iter()
            .filter(|item| item.result.is_ok())
            .count();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.work_running && copied_files > 0,
                    eframe::egui::Button::new(format!("检查本次已复制文件（{copied_files}）")),
                )
                .clicked()
            {
                self.start_metadata_for_copies();
            }
            if ui
                .add_enabled(
                    !self.work_running,
                    eframe::egui::Button::new("选择当前整理目录…"),
                )
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    match fs::canonicalize(&path) {
                        Ok(path) => {
                            self.metadata_directory = Some(path);
                            self.metadata_results.clear();
                        }
                        Err(error) => {
                            self.status = format!("无法读取所选目录 {}：{error}", path.display());
                        }
                    }
                }
            }
            if ui
                .add_enabled(
                    !self.work_running && self.metadata_directory.is_some(),
                    eframe::egui::Button::new("扫描目录并检查 / 重新检查"),
                )
                .clicked()
            {
                self.start_metadata_for_directory();
            }
            if self.work_running
                && self.work_kind == Some(WorkKind::Metadata)
                && ui.button("取消元数据检查").clicked()
            {
                if let Some(control) = &self.work_control {
                    control.cancel();
                }
            }
        });
        if let Some(path) = &self.metadata_directory {
            ui.weak(format!("所选目录：{}", path.display()));
        }
        ui.add_space(8.0);

        if !self.status.is_empty() {
            ui.label(&self.status);
        }
        if !self.metadata_results.is_empty() {
            let valid_gps = self
                .metadata_results
                .iter()
                .filter(|item| item.gps_status == MetadataStatus::Valid)
                .count();
            let valid_time = self
                .metadata_results
                .iter()
                .filter(|item| item.time_status == MetadataStatus::Valid)
                .count();
            let read_errors = self
                .metadata_results
                .iter()
                .filter(|item| {
                    item.gps_status == MetadataStatus::ReadError
                        || item.time_status == MetadataStatus::ReadError
                })
                .count();
            ui.separator();
            ui.label(format!(
                "已检查 {} 项 · GPS 有效 {valid_gps} · 拍摄时间有效 {valid_time} · 读取错误 {read_errors}",
                self.metadata_results.len()
            ));
            eframe::egui::ScrollArea::vertical()
                .id_salt("metadata-results")
                .max_height(360.0)
                .show(ui, |ui| {
                    for item in self.metadata_results.iter().take(80) {
                        ui.group(|ui| {
                            ui.label(item.path.display().to_string());
                            ui.horizontal_wrapped(|ui| {
                                ui.label(format!(
                                    "类型：{}",
                                    item.file_type.as_deref().unwrap_or("未知")
                                ));
                                ui.label(format!("GPS：{}", item.gps_status.label()));
                                if let (Some(latitude), Some(longitude)) =
                                    (item.latitude, item.longitude)
                                {
                                    ui.label(format!("{latitude:.7}, {longitude:.7}"));
                                }
                                if item.gps_zero_zero {
                                    ui.colored_label(
                                        eframe::egui::Color32::from_rgb(140, 85, 0),
                                        "坐标为 0,0，请人工确认",
                                    );
                                }
                                if let Some(source) = &item.gps_source {
                                    ui.weak(source);
                                }
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.label(format!("拍摄时间：{}", item.time_status.label()));
                                if let Some(value) = &item.capture_normalized {
                                    ui.label(value);
                                    ui.weak(item.capture_precision.as_deref().unwrap_or(""));
                                    ui.weak(if item.capture_has_timezone {
                                        "含时区"
                                    } else {
                                        "时区未知"
                                    });
                                }
                                if let Some(source) = &item.capture_source {
                                    ui.weak(source);
                                }
                                if let Some(confidence) = item.capture_confidence {
                                    ui.weak(confidence);
                                }
                                if let Some(raw) = &item.capture_raw {
                                    ui.weak(format!("原始值：{raw}"));
                                }
                            });
                            let camera = [
                                item.make.as_deref(),
                                item.model.as_deref(),
                                item.lens.as_deref(),
                            ]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" · ");
                            if !camera.is_empty() {
                                ui.weak(camera);
                            }
                            let details = [
                                item.focal_length
                                    .as_ref()
                                    .map(|value| format!("焦距 {value}")),
                                item.exposure_time
                                    .as_ref()
                                    .map(|value| format!("曝光 {value}")),
                                item.iso.as_ref().map(|value| format!("ISO {value}")),
                                item.altitude.as_ref().map(|value| format!("海拔 {value}")),
                            ]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" · ");
                            if !details.is_empty() {
                                ui.weak(details);
                            }
                            if let Some(error) = &item.error {
                                ui.colored_label(eframe::egui::Color32::DARK_RED, error);
                            }
                        });
                    }
                    if self.metadata_results.len() > 80 {
                        ui.weak(format!(
                            "另有 {} 项未展开。",
                            self.metadata_results.len() - 80
                        ));
                    }
                });
        }
    }

    fn duplicates_stage(&mut self, ui: &mut eframe::egui::Ui) {
        ui.label("重新扫描所选目录并读取可信拍摄时间。SHA-256 相同表示字节完全相同；时间接近与 pHash 只产生人工审核候选，不会自动删除文件。");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.work_running,
                    eframe::egui::Button::new("选择待检查目录…"),
                )
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    match fs::canonicalize(&path) {
                        Ok(path) => {
                            self.duplicate_directory = Some(path);
                            self.duplicate_report = None;
                            self.clear_saved_review();
                            self.clear_candidate_preview();
                        }
                        Err(error) => {
                            self.status = format!("无法读取所选目录 {}：{error}", path.display());
                        }
                    }
                }
            }
            if ui
                .add_enabled(
                    !self.work_running && self.duplicate_directory.is_some(),
                    eframe::egui::Button::new("完整重新扫描并检查"),
                )
                .clicked()
            {
                self.start_duplicate_scan();
            }
            if self.work_running
                && self.work_kind == Some(WorkKind::Duplicates)
                && ui.button("取消重复检查").clicked()
                && let Some(control) = &self.work_control
            {
                control.cancel();
            }
        });
        if let Some(path) = &self.duplicate_directory {
            ui.weak(format!("所选目录：{}", path.display()));
        }
        let mut duplicate_options_changed = false;
        ui.horizontal(|ui| {
            duplicate_options_changed |= ui
                .add_enabled(
                    !self.work_running,
                    eframe::egui::Slider::new(
                        &mut self.duplicate_options.time_threshold_seconds,
                        0..=60,
                    )
                    .text("拍摄时间窗口（秒）"),
                )
                .changed();
            duplicate_options_changed |= ui
                .add_enabled(
                    !self.work_running,
                    eframe::egui::Slider::new(&mut self.duplicate_options.phash_threshold, 0..=64)
                        .text("pHash 相似阈值（汉明距离）"),
                )
                .changed();
        });
        if duplicate_options_changed {
            self.duplicate_report = None;
            self.clear_candidate_preview();
            self.clear_saved_review();
            self.status = "重复检查阈值已更改；请重新扫描后再携带审核结果。".into();
        }
        ui.weak("时区未知的拍摄时间不会参与跨文件比较。视频仍参与 SHA-256 和时间候选，但不计算静态照片 pHash。离线预览和 pHash 支持 JPEG、PNG、HEIC、HEIF；RAW 或解码失败的格式会显示原因。");
        if !self.status.is_empty() {
            ui.label(&self.status);
        }

        let Some(report) = &self.duplicate_report else {
            return;
        };
        ui.separator();
        ui.label(format!(
            "精确重复组 {} · 时间候选 {} · 有效拍摄时间缺少时区 {} 项 · 因设备不同跳过 {} 对 · 已标记排除 {} 项",
            report.exact_groups.len(),
            report.time_candidates.len(),
            report.unknown_timezone_count,
            report.different_device_pairs_skipped,
            self.excluded_final_paths.len()
        ));
        if report.time_candidates_truncated {
            ui.colored_label(
                eframe::egui::Color32::from_rgb(160, 90, 0),
                "候选超过 100,000 对，结果已截断。请缩小目录范围后重新扫描。",
            );
        }
        if !report.hash_errors.is_empty() {
            ui.colored_label(
                eframe::egui::Color32::DARK_RED,
                format!("{} 个文件读取 SHA-256 失败。", report.hash_errors.len()),
            );
        }
        let mut review_changed = false;
        if ui
            .add_enabled(
                !self.work_running && !self.excluded_final_paths.is_empty(),
                eframe::egui::Button::new("清除本次排除标记（全部保留）"),
            )
            .clicked()
            && !self.excluded_final_paths.is_empty()
        {
            self.excluded_final_paths.clear();
            review_changed = true;
        }

        let mut preview_request = None;
        eframe::egui::ScrollArea::vertical()
            .id_salt("duplicate-report")
            .max_height(480.0)
            .show(ui, |ui| {
                if !report.exact_groups.is_empty() {
                    ui.heading("字节完全相同");
                    for group in &report.exact_groups {
                        ui.group(|ui| {
                            ui.label(format!(
                                "SHA-256 {} · {} 个路径",
                                group.sha256,
                                group.paths.len()
                            ));
                            for path in &group.paths {
                                let mut exclude = self.excluded_final_paths.contains(path);
                                if ui
                                    .add_enabled(
                                        !self.work_running,
                                        eframe::egui::Checkbox::new(
                                            &mut exclude,
                                            format!("排除本次最终清单：{}", path.display()),
                                        ),
                                    )
                                    .changed()
                                {
                                    if exclude {
                                        self.excluded_final_paths.insert(path.clone());
                                    } else {
                                        self.excluded_final_paths.remove(path);
                                    }
                                    review_changed = true;
                                }
                            }
                        });
                    }
                }
                if !report.time_candidates.is_empty() {
                    ui.add_space(8.0);
                    ui.heading("拍摄时间候选（仅供人工复核）");
                    for candidate in &report.time_candidates {
                        ui.group(|ui| {
                            if ui.button("并排预览（JPEG/PNG）").clicked() {
                                preview_request =
                                    Some((candidate.first.clone(), candidate.second.clone()));
                            }
                            ui.label(format!(
                                "时间差 {:.3} 秒{}",
                                candidate.delta_millis as f64 / 1000.0,
                                candidate.phash_distance.map_or_else(
                                    || " · 未计算 pHash".to_string(),
                                    |distance| {
                                        let class =
                                            if distance <= self.duplicate_options.phash_threshold {
                                                "视觉相似候选"
                                            } else {
                                                "视觉差异较大"
                                            };
                                        format!(" · pHash 汉明距离 {distance}/64 · {class}")
                                    }
                                )
                            ));
                            ui.label(format!(
                                "A：{} · {}",
                                candidate.first.display(),
                                candidate.first_capture_time
                            ));
                            ui.weak(format!("SHA-256 {}", candidate.first_sha256));
                            ui.label(format!(
                                "B：{} · {}",
                                candidate.second.display(),
                                candidate.second_capture_time
                            ));
                            ui.weak(format!("SHA-256 {}", candidate.second_sha256));
                            if let Some(note) = &candidate.phash_note {
                                ui.colored_label(eframe::egui::Color32::from_rgb(160, 90, 0), note);
                            }
                            let mut exclude = self.excluded_final_paths.contains(&candidate.second);
                            if ui
                                .add_enabled(
                                    !self.work_running,
                                    eframe::egui::Checkbox::new(
                                        &mut exclude,
                                        format!("排除本次最终清单：{}", candidate.second.display()),
                                    ),
                                )
                                .changed()
                            {
                                if exclude {
                                    self.excluded_final_paths.insert(candidate.second.clone());
                                } else {
                                    self.excluded_final_paths.remove(&candidate.second);
                                }
                                review_changed = true;
                            }
                        });
                    }
                }
                for (path, error) in report.hash_errors.iter().take(50) {
                    ui.colored_label(
                        eframe::egui::Color32::DARK_RED,
                        format!("SHA-256 失败：{}：{error}", path.display()),
                    );
                }
                if report.hash_errors.len() > 50 {
                    ui.weak(format!(
                        "另有 {} 个读取错误未展开。",
                        report.hash_errors.len() - 50
                    ));
                }
            });
        if review_changed && self.final_manifest.is_some() {
            self.refresh_carried_review();
        }
        if let Some((first, second)) = preview_request {
            self.start_candidate_preview(first, second);
        }
    }

    fn backup_stage(&mut self, ui: &mut eframe::egui::Ui) {
        ui.label("每行将一个本地整理文件夹映射到一个或多个独立备份目标。目标是最终文件夹本身；复制后会重新读取源与每个目标文件并逐项比较 SHA-256。此阶段可单独使用。");
        if !self.excluded_final_paths.is_empty() {
            ui.weak(format!(
                "本次会话携带 {} 个阶段⑥排除标记；路径完全匹配的文件将从备份清单排除。",
                self.excluded_final_paths.len()
            ));
        }
        if let Some((count, root)) = self.final_manifest.as_ref().map(|manifest| {
            (
                manifest.file_sha256.len(),
                manifest.root.display().to_string(),
            )
        }) {
            let checkpoint_note = if self.review_checkpoint_saved {
                "审核决定保存在本机私有检查点。"
            } else {
                "审核决定仅在当前运行中可用；检查点未保存。"
            };
            let mut clear_review = false;
            ui.horizontal_wrapped(|ui| {
                ui.weak(format!(
                    "阶段⑥ Final Manifest：{} 个已确认文件；与 {} 重叠的备份来源只处理这些路径并核对 SHA-256。{}",
                    count, root, checkpoint_note
                ));
                if ui.button("清除审核清单").clicked() {
                    clear_review = true;
                }
            });
            if clear_review {
                self.clear_saved_review();
                self.status = "已清除本机阶段⑥审核清单；后续备份将重新扫描所选目录。".into();
            }
        }
        let mut mappings_changed = false;
        if ui
            .add_enabled(
                !self.work_running,
                eframe::egui::Button::new("添加本地整理文件夹…"),
            )
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                match fs::canonicalize(&path) {
                    Ok(path) => {
                        self.backup_mappings.push(BackupMapping {
                            source: Some(path),
                            targets: Vec::new(),
                        });
                        mappings_changed = true;
                    }
                    Err(error) => {
                        self.status = format!("无法读取所选来源 {}：{error}", path.display());
                    }
                }
            }
        }

        let mut remove_mapping = None;
        for index in 0..self.backup_mappings.len() {
            let mut mapping = self.backup_mappings[index].clone();
            let mut remove_target = None;
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.strong(format!("来源文件夹 {}", index + 1));
                    if ui
                        .add_enabled(!self.work_running, eframe::egui::Button::new("移除此行"))
                        .clicked()
                    {
                        remove_mapping = Some(index);
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            !self.work_running,
                            eframe::egui::Button::new("选择/更改来源…"),
                        )
                        .clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_folder()
                    {
                        match fs::canonicalize(&path) {
                            Ok(path) => {
                                if mapping.source.as_ref() != Some(&path) {
                                    mapping.source = Some(path);
                                    mappings_changed = true;
                                }
                            }
                            Err(error) => {
                                self.status = format!("无法读取来源 {}：{error}", path.display());
                            }
                        }
                    }
                    ui.label(mapping.source.as_deref().map_or_else(
                        || "尚未选择来源".to_string(),
                        |path| path.display().to_string(),
                    ));
                });
                for (target_index, target) in mapping.targets.iter().enumerate() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("备份目标：{}", target.display()));
                        if ui
                            .add_enabled(!self.work_running, eframe::egui::Button::new("移除目标"))
                            .clicked()
                        {
                            remove_target = Some(target_index);
                        }
                    });
                }
                if ui
                    .add_enabled(
                        !self.work_running,
                        eframe::egui::Button::new("+ 添加备份目标文件夹…"),
                    )
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_folder()
                {
                    match fs::canonicalize(&path) {
                        Ok(path) => {
                            if !mapping.targets.contains(&path) {
                                mapping.targets.push(path);
                                mappings_changed = true;
                            }
                        }
                        Err(error) => {
                            self.status = format!("无法读取备份目标 {}：{error}", path.display());
                        }
                    }
                }
            });
            if let Some(target_index) = remove_target {
                mapping.targets.remove(target_index);
                mappings_changed = true;
            }
            self.backup_mappings[index] = mapping;
        }
        if let Some(index) = remove_mapping {
            self.backup_mappings.remove(index);
            mappings_changed = true;
        }
        if mappings_changed {
            self.clear_backup_results();
            self.pending_backup_resume = None;
            let checkpoint_error = checkpoint::clear_pending_backup().err();
            self.status = checkpoint_error.map_or_else(
                || "备份映射已修改；旧备份校验结果已清除。".into(),
                |error| format!("备份映射已修改，旧结果已清除；删除过期检查点失败：{error}"),
            );
        }

        if let Some(mappings) = self.pending_backup_resume.as_ref() {
            let target_count = mappings
                .iter()
                .map(|mapping| mapping.targets.len())
                .sum::<usize>();
            ui.separator();
            ui.label(format!(
                "发现上次未完成的备份任务：{} 个来源、{target_count} 个目标。恢复会重新扫描来源，并校验已有目标；不会覆盖不匹配的文件。",
                mappings.len()
            ));
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.work_running,
                        eframe::egui::Button::new("重新扫描并恢复备份"),
                    )
                    .clicked()
                {
                    self.resume_pending_backup();
                }
                if ui
                    .add_enabled(
                        !self.work_running,
                        eframe::egui::Button::new("丢弃备份恢复记录"),
                    )
                    .clicked()
                {
                    self.discard_pending_backup();
                }
            });
        }

        let ready = self
            .backup_mappings
            .iter()
            .any(|mapping| mapping.source.is_some() && !mapping.targets.is_empty());
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.work_running && ready && self.pending_backup_resume.is_none(),
                    eframe::egui::Button::new("开始 / 重试复制并校验"),
                )
                .clicked()
            {
                self.start_backup(false, false);
            }
            if ui
                .add_enabled(
                    !self.work_running && ready,
                    eframe::egui::Button::new("重新检查现有目标"),
                )
                .clicked()
            {
                self.start_backup(true, false);
            }
            if self.work_running
                && self.work_kind == Some(WorkKind::Backup)
                && ui.button("取消备份").clicked()
                && let Some(control) = &self.work_control
            {
                control.cancel();
            }
        });
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
        if !self.backup_verification.is_empty() {
            ui.separator();
            ui.heading("每个备份目标的当前校验");
            for mapping in &self.backup_mappings {
                for target in &mapping.targets {
                    let copy_rows = self
                        .backup_copies
                        .iter()
                        .filter(|item| item.destination.starts_with(target));
                    let mut copy_ok = 0usize;
                    let mut copy_failed = 0usize;
                    for item in copy_rows {
                        if item.result.is_ok() {
                            copy_ok += 1;
                        } else {
                            copy_failed += 1;
                        }
                    }
                    let rows = self
                        .backup_verification
                        .iter()
                        .filter(|item| item.destination.starts_with(target));
                    let mut passed = 0usize;
                    let mut failed = 0usize;
                    let mut examined = 0usize;
                    for item in rows {
                        examined += 1;
                        if item.result.as_ref().is_ok_and(|result| result.matches) {
                            passed += 1;
                        } else {
                            failed += 1;
                        }
                    }
                    ui.label(format!(
                        "{} · 复制成功 {copy_ok} / 失败 {copy_failed} · 读回校验 {passed} PASS / {failed} FAIL（{examined} 项）",
                        target.display(),
                    ));
                }
            }
            eframe::egui::ScrollArea::vertical()
                .id_salt("backup-results")
                .max_height(300.0)
                .show(ui, |ui| {
                    for item in self
                        .backup_copies
                        .iter()
                        .filter(|item| item.result.is_err())
                        .take(50)
                    {
                        ui.group(|ui| {
                            ui.colored_label(eframe::egui::Color32::DARK_RED, "复制失败");
                            ui.label(format!("源：{}", item.source.display()));
                            ui.label(format!("目标：{}", item.destination.display()));
                            if let Err(error) = &item.result {
                                ui.colored_label(eframe::egui::Color32::DARK_RED, error);
                            }
                        });
                    }
                    for item in self.backup_verification.iter().take(100) {
                        let matching = item.result.as_ref().is_ok_and(|result| result.matches);
                        ui.group(|ui| {
                            ui.colored_label(
                                if matching {
                                    eframe::egui::Color32::DARK_GREEN
                                } else {
                                    eframe::egui::Color32::DARK_RED
                                },
                                if matching { "PASS" } else { "FAIL" },
                            );
                            ui.label(format!("源：{}", item.source.display()));
                            ui.label(format!("目标：{}", item.destination.display()));
                            if let Ok(result) = &item.result
                                && !result.matches
                            {
                                ui.weak(format!(
                                    "源 {} / {} · 目标 {} / {}",
                                    result.source_size,
                                    result.source_sha256,
                                    result.destination_size,
                                    result.destination_sha256
                                ));
                            } else if let Err(error) = &item.result {
                                ui.colored_label(eframe::egui::Color32::DARK_RED, error);
                            }
                        });
                    }
                    if self.backup_verification.len() > 100 {
                        ui.weak(format!(
                            "另有 {} 项未展开。",
                            self.backup_verification.len() - 100
                        ));
                    }
                });
        }
    }

    fn archive_stage(&mut self, ui: &mut eframe::egui::Ui) {
        let mut archive_inputs_changed = false;
        ui.label("选择一个本地文件夹，生成内部含同名顶层目录的加密 7z Store 归档；完成后逐项核对归档清单并流式重算 SHA-256。");
        ui.colored_label(
            eframe::egui::Color32::from_rgb(40, 105, 40),
            "超过单卷大小时会输出 .7z.001/.002… 卷集；它们是一份逻辑归档，请保持所有卷同目录并确保卷齐全。",
        );
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.work_running,
                    eframe::egui::Button::new("选择归档来源文件夹…"),
                )
                .clicked()
                && let Some(path) = rfd::FileDialog::new().pick_folder()
            {
                match fs::canonicalize(&path) {
                    Ok(path) => {
                        let default_name = default_archive_name(&path);
                        archive_inputs_changed |= self.archive_source.as_ref() != Some(&path);
                        self.archive_source = Some(path);
                        if !self.archive_name_custom {
                            self.archive_name = default_name;
                        }
                    }
                    Err(error) => {
                        self.status = format!("无法读取归档来源 {}：{error}", path.display());
                    }
                }
            }
            ui.label(self.archive_source.as_deref().map_or_else(
                || "尚未选择来源".to_string(),
                |path| path.display().to_string(),
            ));
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.work_running,
                    eframe::egui::Button::new("选择输出目录…"),
                )
                .clicked()
                && let Some(path) = rfd::FileDialog::new().pick_folder()
            {
                match fs::canonicalize(&path) {
                    Ok(path) => {
                        archive_inputs_changed |=
                            self.archive_output_directory.as_ref() != Some(&path);
                        self.archive_output_directory = Some(path);
                    }
                    Err(error) => {
                        self.status = format!("无法读取归档输出目录 {}：{error}", path.display());
                    }
                }
            }
            ui.label(self.archive_output_directory.as_deref().map_or_else(
                || "尚未选择输出目录".to_string(),
                |path| path.display().to_string(),
            ));
        });
        ui.horizontal(|ui| {
            ui.label("归档名称（默认日期为 UTC，可修改）");
            if ui
                .add_enabled(
                    !self.work_running,
                    eframe::egui::TextEdit::singleline(&mut self.archive_name),
                )
                .changed()
            {
                self.archive_name_custom = true;
                archive_inputs_changed = true;
            }
            ui.label("（自动补 .7z）");
        });
        archive_inputs_changed |= ui
            .add_enabled(
                !self.work_running,
                eframe::egui::Slider::new(&mut self.archive_options.volume_mb, 1..=2000)
                    .text("单卷上限（十进制 MB）"),
            )
            .changed();
        if archive_inputs_changed {
            self.invalidate_pending_archive();
        }
        ui.weak("2000 MB = 2,000,000,000 字节。密码只用于当前归档任务；不会写入文件名、进程参数或报告。归档采用 Store（不压缩）、AES-256 和加密文件名。");
        if !self.excluded_final_paths.is_empty() {
            ui.weak(format!(
                "携带了 {} 个阶段⑥排除标记；归档扫描遇到同一绝对路径时会从输入清单排除。",
                self.excluded_final_paths.len()
            ));
        }
        if let (Some(source), Some(manifest)) = (&self.archive_source, self.final_manifest.as_ref())
            && manifest.applies_to(source)
        {
            let count = manifest.file_sha256.len();
            let checkpoint_note = if self.review_checkpoint_saved {
                "审核决定保存在本机私有检查点。"
            } else {
                "审核决定仅在当前运行中可用；检查点未保存。"
            };
            let mut clear_review = false;
            ui.horizontal_wrapped(|ui| {
                ui.weak(format!(
                    "阶段⑥ Final Manifest：只归档其中 {} 个已确认文件，并核对其 SHA-256；{}",
                    count, checkpoint_note
                ));
                if ui.button("清除审核清单").clicked() {
                    clear_review = true;
                }
            });
            if clear_review {
                self.clear_saved_review();
                self.status = "已清除本机阶段⑥审核清单；归档将按所选目录重新建立输入清单。".into();
            }
        }
        ui.horizontal(|ui| {
            ui.label("密码");
            ui.add_enabled(
                !self.work_running,
                eframe::egui::TextEdit::singleline(&mut self.archive_password)
                    .password(!self.archive_show_password)
                    .desired_width(240.0),
            );
            ui.label("确认密码");
            ui.add_enabled(
                !self.work_running,
                eframe::egui::TextEdit::singleline(&mut self.archive_confirmation)
                    .password(!self.archive_show_password)
                    .desired_width(240.0),
            );
            ui.add_enabled(
                !self.work_running,
                eframe::egui::Checkbox::new(&mut self.archive_show_password, "显示"),
            );
            if ui
                .add_enabled(!self.work_running, eframe::egui::Button::new("清空密码"))
                .clicked()
            {
                self.archive_password.zeroize();
                self.archive_confirmation.zeroize();
            }
        });
        let ready = self.archive_source.is_some()
            && self.archive_output_directory.is_some()
            && !self.archive_name.trim().is_empty()
            && !self.archive_password.is_empty()
            && self.archive_password == self.archive_confirmation;
        ui.horizontal(|ui| {
            if self.pending_archive_resume.is_some() {
                if ui
                    .add_enabled(
                        !self.work_running && ready,
                        eframe::egui::Button::new("重新扫描并重建归档"),
                    )
                    .clicked()
                {
                    self.start_archive();
                }
                if ui
                    .add_enabled(
                        !self.work_running,
                        eframe::egui::Button::new("丢弃恢复记录"),
                    )
                    .clicked()
                {
                    self.discard_pending_archive();
                }
            } else if ui
                .add_enabled(
                    !self.work_running && ready,
                    eframe::egui::Button::new("创建并验证加密归档"),
                )
                .clicked()
            {
                self.start_archive();
            }
            if self.work_running
                && self.work_kind == Some(WorkKind::Archive)
                && ui.button("取消归档").clicked()
                && let Some(control) = &self.work_control
            {
                control.cancel();
            }
        });
        if self.pending_archive_resume.is_some() {
            ui.weak("检测到未完成归档。恢复会重新扫描并从头创建、验证；密码未保存，请重新输入。中断留下的临时文件不会自动删除；若目标归档已存在，程序不会覆盖，请更换名称或先处理残留文件。");
        }
        if !self.archive_password.is_empty()
            && !self.archive_confirmation.is_empty()
            && self.archive_password != self.archive_confirmation
        {
            ui.colored_label(eframe::egui::Color32::DARK_RED, "两次输入的密码不一致。");
        }
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
    }

    fn start_archive(&mut self) {
        if self.work_running {
            return;
        }
        let (Some(source), Some(output_directory)) = (
            self.archive_source.clone(),
            self.archive_output_directory.clone(),
        ) else {
            self.status = "请先选择归档来源文件夹和输出目录。".into();
            return;
        };
        if self.archive_password.is_empty() || self.archive_password != self.archive_confirmation {
            self.status = "归档密码不能为空，且两次输入必须一致。".into();
            return;
        }
        let name = self.archive_name.trim().to_string();
        let plan = checkpoint::SavedArchivePlan {
            source: source.clone(),
            output_directory: output_directory.clone(),
            name: name.clone(),
            volume_mb: self.archive_options.volume_mb,
            manifest_fingerprint: self.archive_manifest_fingerprint(&source),
        };
        let was_resume = self.pending_archive_resume.is_some();
        if let Err(error) = checkpoint::save_pending_archive(&plan) {
            self.status = format!("无法保存归档恢复检查点，未开始归档：{error}");
            return;
        }
        self.pending_archive_resume = Some(plan);
        let max_bytes = u64::from(self.archive_options.volume_mb) * 1_000_000;
        let excluded_paths = self.excluded_final_paths.clone();
        let included_sha256 = self
            .final_manifest
            .as_ref()
            .filter(|manifest| manifest.applies_to(&source))
            .map(|manifest| manifest.file_sha256.clone());
        let mut password = std::mem::take(&mut self.archive_password);
        self.archive_confirmation.zeroize();
        self.archive_show_password = false;
        self.work_completed = 0;
        self.work_total = 0;
        self.work_bytes = 0;
        self.work_kind = Some(WorkKind::Archive);
        self.work_running = true;
        self.status = if was_resume {
            "正在重新扫描来源并从头重建、验证加密归档…".into()
        } else {
            "正在扫描、建立哈希清单并创建临时加密归档…".into()
        };
        let (sender, receiver) = mpsc::channel();
        let control = TaskControl::default();
        let worker_control = control.clone();
        self.work_receiver = Some(receiver);
        self.work_control = Some(control);
        thread::spawn(move || {
            let result = archive::create_and_verify(
                &source,
                &output_directory,
                &name,
                &password,
                max_bytes,
                &excluded_paths,
                included_sha256.as_ref(),
                &worker_control,
                |completed, total, bytes| {
                    let _ = sender.send(WorkMessage::ArchiveProgress {
                        completed,
                        total,
                        bytes,
                    });
                },
            );
            password.zeroize();
            let _ = sender.send(WorkMessage::ArchiveComplete {
                result,
                cancelled: worker_control.is_cancelled(),
            });
        });
    }

    fn start_backup(&mut self, verify_only: bool, resume_existing: bool) {
        if self.work_running {
            return;
        }
        let mappings = self.backup_mappings.clone();
        let invalid = backup_mapping_errors(&mappings);
        if !invalid.is_empty() {
            if resume_existing {
                self.pending_backup_resume = Some(mappings);
            }
            self.status = invalid.join("；");
            return;
        }
        if !verify_only {
            let saved = mappings
                .iter()
                .filter_map(saved_backup_mapping)
                .collect::<Vec<_>>();
            if let Err(error) = checkpoint::save_pending_backup(&saved) {
                if resume_existing {
                    self.pending_backup_resume = Some(mappings);
                }
                self.status = format!("无法保存备份恢复检查点，未开始备份：{error}");
                return;
            }
        }
        if !verify_only {
            self.backup_copies.clear();
        }
        self.backup_verification.clear();
        self.work_completed = 0;
        self.work_total = 0;
        self.work_bytes = 0;
        self.work_kind = Some(WorkKind::Backup);
        self.work_running = true;
        self.status = if verify_only {
            "正在重新扫描源目录与现有备份目标…".into()
        } else if resume_existing {
            "正在重新扫描备份来源并续跑；已有目标会重新校验…".into()
        } else {
            "正在扫描本地整理目录并准备独立备份目标…".into()
        };
        let excluded = self.excluded_final_paths.clone();
        let final_manifest = self.final_manifest.clone();
        let (sender, receiver) = mpsc::channel();
        let control = TaskControl::default();
        let worker_control = control.clone();
        self.work_receiver = Some(receiver);
        self.work_control = Some(control);
        thread::spawn(move || {
            let mut items = Vec::<RoutedFile>::new();
            let mut expected_by_target =
                BTreeMap::<PathBuf, (PathBuf, std::collections::BTreeSet<PathBuf>)>::new();
            let mut error = None;
            let mut excluded_count = 0usize;
            let mut manifest_filtered_count = 0usize;
            for mapping in mappings {
                if worker_control.is_cancelled() {
                    let _ = sender.send(WorkMessage::BackupComplete {
                        copies: Vec::new(),
                        verification: Vec::new(),
                        cancelled: true,
                        error,
                        verify_only,
                    });
                    return;
                }
                let Some(source) = mapping.source else {
                    continue;
                };
                let scoped_manifest = final_manifest
                    .as_ref()
                    .filter(|manifest| manifest.applies_to(&source));
                let summary =
                    match scan_sources(std::slice::from_ref(&source), &worker_control, |_| {}) {
                        Ok(summary) => summary,
                        Err(_) => {
                            let _ = sender.send(WorkMessage::BackupComplete {
                                copies: Vec::new(),
                                verification: Vec::new(),
                                cancelled: true,
                                error,
                                verify_only,
                            });
                            return;
                        }
                    };
                if summary.error_count > 0 {
                    append_task_error(
                        &mut error,
                        format!(
                            "扫描 {} 时有 {} 项读取错误（例如 {}）",
                            source.display(),
                            summary.error_count,
                            summary
                                .error_samples
                                .first()
                                .map(String::as_str)
                                .unwrap_or("未提供")
                        ),
                    );
                }
                if let Some(manifest) = scoped_manifest {
                    let actual_paths = summary
                        .files
                        .iter()
                        .map(|file| file.path.clone())
                        .collect::<std::collections::BTreeSet<_>>();
                    let missing = manifest
                        .file_sha256
                        .keys()
                        .filter(|path| path.starts_with(&source) && !actual_paths.contains(*path))
                        .count();
                    if missing > 0 {
                        append_task_error(
                            &mut error,
                            format!("最终清单中有 {missing} 个文件已不存在或无法扫描"),
                        );
                    }
                }
                for file in summary.files {
                    if !worker_control.checkpoint() {
                        let _ = sender.send(WorkMessage::BackupComplete {
                            copies: Vec::new(),
                            verification: Vec::new(),
                            cancelled: true,
                            error,
                            verify_only,
                        });
                        return;
                    }
                    let expected_sha256 =
                        scoped_manifest.and_then(|manifest| manifest.sha256_for(&file.path));
                    if scoped_manifest.is_some() && expected_sha256.is_none() {
                        manifest_filtered_count += mapping.targets.len();
                        continue;
                    }
                    if excluded.contains(&file.path) {
                        excluded_count += mapping.targets.len();
                        continue;
                    }
                    for target in &mapping.targets {
                        expected_by_target
                            .entry(target.clone())
                            .or_insert_with(|| (source.clone(), std::collections::BTreeSet::new()))
                            .1
                            .insert(file.relative_path.clone());
                        items.push(RoutedFile {
                            source: file.path.clone(),
                            destination: target.join(&file.relative_path),
                            expected_source: FileStamp {
                                size: file.size,
                                modified: file.modified,
                            },
                            expected_sha256: expected_sha256.map(str::to_string),
                        });
                    }
                }
            }
            if items.is_empty() {
                let _ = sender.send(WorkMessage::BackupComplete {
                    copies: Vec::new(),
                    verification: Vec::new(),
                    cancelled: false,
                    error: Some(match error {
                        Some(error) => format!("{error}；没有可备份文件。"),
                        None => "没有可备份文件；请检查来源目录、最终清单和阶段⑥排除标记。".into(),
                    }),
                    verify_only,
                });
                return;
            }
            if excluded_count > 0 {
                append_task_error(
                    &mut error,
                    format!("按阶段⑥决定排除了 {excluded_count} 个目标文件项"),
                );
            }
            if manifest_filtered_count > 0 {
                append_task_error(
                    &mut error,
                    format!("最终清单之外的 {manifest_filtered_count} 个目标文件项未备份"),
                );
            }
            let item_count = items.len();
            if verify_only {
                let expected_sha256 = items
                    .iter()
                    .filter_map(|item| {
                        item.expected_sha256
                            .as_ref()
                            .map(|sha256| (item.source.clone(), sha256.clone()))
                    })
                    .collect::<BTreeMap<_, _>>();
                let pairs = items
                    .into_iter()
                    .map(|item| (item.source, item.destination))
                    .collect::<Vec<_>>();
                let (mut verification, cancelled) =
                    pipeline::verify_batch(pairs, &worker_control, |completed, total| {
                        let _ = sender.send(WorkMessage::BackupProgress {
                            completed,
                            total,
                            bytes: 0,
                        });
                    });
                for item in &mut verification {
                    let changed = expected_sha256.get(&item.source).is_some_and(|expected| {
                        item.result
                            .as_ref()
                            .is_ok_and(|result| &result.source_sha256 != expected)
                    });
                    if changed {
                        item.result =
                            Err("源文件 SHA-256 与阶段⑥最终清单不符；请重新检查重复项。".into());
                    }
                }
                let mut verification = verification;
                let (extra_results, target_error) =
                    inspect_backup_targets(&expected_by_target, &worker_control);
                verification.extend(extra_results);
                if let Some(target_error) = target_error {
                    error = Some(match error {
                        Some(error) => format!("{error}；{target_error}"),
                        None => target_error,
                    });
                }
                let _ = sender.send(WorkMessage::BackupComplete {
                    copies: Vec::new(),
                    verification,
                    cancelled,
                    error,
                    verify_only,
                });
                return;
            }
            let progress = |completed: usize, total: usize, bytes: u64| {
                let _ = sender.send(WorkMessage::BackupProgress {
                    completed,
                    total: total.saturating_mul(2),
                    bytes,
                });
            };
            let (copies, cancelled_copy) = if resume_existing {
                pipeline::resume_copy_batch(items, &worker_control, progress)
            } else {
                pipeline::copy_batch(items, &worker_control, progress)
            };
            let pairs = copies
                .iter()
                .filter(|item| item.result.is_ok())
                .map(|item| (item.source.clone(), item.destination.clone()))
                .collect::<Vec<_>>();
            let verify_total = item_count.saturating_mul(2);
            let (verification, cancelled_verify) =
                pipeline::verify_batch(pairs, &worker_control, |completed, _| {
                    let _ = sender.send(WorkMessage::BackupProgress {
                        completed: item_count.saturating_add(completed),
                        total: verify_total,
                        bytes: 0,
                    });
                });
            let mut verification = verification;
            let (extra_results, target_error) =
                inspect_backup_targets(&expected_by_target, &worker_control);
            verification.extend(extra_results);
            if let Some(target_error) = target_error {
                error = Some(match error {
                    Some(error) => format!("{error}；{target_error}"),
                    None => target_error,
                });
            }
            let _ = sender.send(WorkMessage::BackupComplete {
                copies,
                verification,
                cancelled: cancelled_copy || cancelled_verify,
                error,
                verify_only,
            });
        });
    }

    fn start_duplicate_scan(&mut self) {
        if self.work_running {
            return;
        }
        let Some(root) = self.duplicate_directory.clone() else {
            self.status = "请先选择待检查目录。".into();
            return;
        };
        self.duplicate_report = None;
        self.clear_saved_review();
        self.clear_candidate_preview();
        self.work_completed = 0;
        self.work_total = 0;
        self.work_bytes = 0;
        self.work_kind = Some(WorkKind::Duplicates);
        self.work_running = true;
        self.status = "正在重新扫描目录并读取拍摄元数据…".into();
        let threshold = self.duplicate_options.time_threshold_seconds;
        let (sender, receiver) = mpsc::channel();
        let control = TaskControl::default();
        let worker_control = control.clone();
        self.work_receiver = Some(receiver);
        self.work_control = Some(control);
        thread::spawn(move || {
            let summary = match scan_sources(&[root], &worker_control, |progress| {
                let _ = sender.send(WorkMessage::DuplicateProgress {
                    completed: progress.files,
                    total: progress.files,
                });
            }) {
                Ok(summary) => summary,
                Err(_) => {
                    let _ = sender.send(WorkMessage::DuplicateComplete {
                        report: duplicates::DuplicateReport::default(),
                        cancelled: true,
                        error: None,
                    });
                    return;
                }
            };
            if worker_control.is_cancelled() {
                let _ = sender.send(WorkMessage::DuplicateComplete {
                    report: duplicates::DuplicateReport::default(),
                    cancelled: true,
                    error: None,
                });
                return;
            }
            let paths = summary
                .files
                .into_iter()
                .map(|file| file.path)
                .collect::<Vec<_>>();
            let mut error = (summary.error_count > 0)
                .then(|| format!("目录扫描有 {} 项无法读取。", summary.error_count));
            let _ = sender.send(WorkMessage::DuplicateProgress {
                completed: 0,
                total: paths.len(),
            });
            let metadata =
                match metadata::inspect_files(&paths, &worker_control, |completed, total| {
                    let _ = sender.send(WorkMessage::DuplicateProgress { completed, total });
                }) {
                    Ok((records, cancelled)) => {
                        if cancelled {
                            let _ = sender.send(WorkMessage::DuplicateComplete {
                                report: duplicates::DuplicateReport::default(),
                                cancelled: true,
                                error,
                            });
                            return;
                        }
                        records
                    }
                    Err(metadata_error) => {
                        error = Some(match error {
                            Some(scan_error) => {
                                format!("{scan_error} 元数据不可用：{metadata_error}")
                            }
                            None => format!("拍摄时间不可用：{metadata_error}"),
                        });
                        paths
                            .iter()
                            .cloned()
                            .map(|path| MetadataRecord::unsupported(path, metadata_error.clone()))
                            .collect()
                    }
                };
            let (report, cancelled) =
                duplicates::analyze(&metadata, threshold, &worker_control, |completed, total| {
                    let _ = sender.send(WorkMessage::DuplicateProgress { completed, total });
                });
            let _ = sender.send(WorkMessage::DuplicateComplete {
                report,
                cancelled,
                error,
            });
        });
    }

    fn candidate_preview_dialog(&mut self, ctx: &eframe::egui::Context) {
        let completed = self
            .candidate_preview_receiver
            .as_ref()
            .map(Receiver::try_recv);
        match completed {
            Some(Ok((first, second))) => {
                self.candidate_preview_receiver = None;
                self.candidate_preview_loading = false;
                if let Some((first_path, second_path)) = &self.candidate_preview_paths {
                    self.candidate_preview_textures = Some((
                        candidate_preview_texture(ctx, first_path, first),
                        candidate_preview_texture(ctx, second_path, second),
                    ));
                }
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.candidate_preview_receiver = None;
                self.candidate_preview_loading = false;
                if self.candidate_preview_paths.is_some() {
                    let error = "预览任务意外结束，请重新打开预览。".to_string();
                    self.candidate_preview_textures = Some((Err(error.clone()), Err(error)));
                }
            }
            Some(Err(TryRecvError::Empty)) | None => {}
        }
        let Some((first_path, second_path)) = self.candidate_preview_paths.clone() else {
            return;
        };
        let mut open = true;
        eframe::egui::Window::new("候选照片并排预览")
            .open(&mut open)
            .resizable(true)
            .default_width(960.0)
            .show(ctx, |ui| {
                if let Some((first_texture, second_texture)) = &self.candidate_preview_textures {
                    ui.horizontal_top(|ui| {
                        show_candidate_preview(ui, &first_path, first_texture);
                        show_candidate_preview(ui, &second_path, second_texture);
                    });
                } else {
                    ui.spinner();
                    ui.label("正在读取图片并生成预览…");
                }
            });
        if !open {
            self.clear_candidate_preview();
        }
    }

    fn start_candidate_preview(&mut self, first: PathBuf, second: PathBuf) {
        let (sender, receiver) = mpsc::channel();
        self.candidate_preview_paths = Some((first.clone(), second.clone()));
        self.candidate_preview_receiver = Some(receiver);
        self.candidate_preview_loading = true;
        self.candidate_preview_textures = None;
        thread::spawn(move || {
            let result = (
                decode_candidate_preview(&first),
                decode_candidate_preview(&second),
            );
            let _ = sender.send(result);
        });
    }

    fn clear_candidate_preview(&mut self) {
        self.candidate_preview_paths = None;
        self.candidate_preview_receiver = None;
        self.candidate_preview_loading = false;
        self.candidate_preview_textures = None;
    }

    fn start_metadata_for_copies(&mut self) {
        let paths: Vec<PathBuf> = self
            .copy_results
            .iter()
            .filter(|item| item.result.is_ok())
            .map(|item| item.destination.clone())
            .collect();
        if paths.is_empty() {
            self.status = "没有本次成功复制的目标文件；请选择当前整理目录。".into();
            return;
        }
        self.start_metadata(MetadataInput::Files(paths));
    }

    fn start_metadata_for_directory(&mut self) {
        let Some(path) = self.metadata_directory.clone() else {
            self.status = "请先选择当前整理目录。".into();
            return;
        };
        self.start_metadata(MetadataInput::Directory(path));
    }

    fn start_metadata(&mut self, input: MetadataInput) {
        if self.work_running || !metadata::embedded_tool_available() {
            return;
        }
        self.metadata_results.clear();
        self.work_completed = 0;
        self.work_total = 0;
        self.work_bytes = 0;
        self.work_kind = Some(WorkKind::Metadata);
        self.work_running = true;
        self.status = "正在准备 ExifTool 元数据检查…".into();
        let (sender, receiver) = mpsc::channel();
        let control = TaskControl::default();
        let worker_control = control.clone();
        self.work_receiver = Some(receiver);
        self.work_control = Some(control);
        thread::spawn(move || {
            let (paths, mut scan_error) = match input {
                MetadataInput::Files(paths) => (paths, None),
                MetadataInput::Directory(root) => {
                    let scan = scan_sources(&[root], &worker_control, |progress| {
                        let _ = sender.send(WorkMessage::MetadataProgress {
                            completed: 0,
                            total: progress.files,
                        });
                    });
                    match scan {
                        Ok(summary) => {
                            let warning = (summary.error_count > 0).then(|| {
                                format!(
                                    "目录扫描有 {} 项无法读取；示例：{}",
                                    summary.error_count,
                                    summary
                                        .error_samples
                                        .first()
                                        .map(String::as_str)
                                        .unwrap_or("未提供")
                                )
                            });
                            (
                                summary.files.into_iter().map(|file| file.path).collect(),
                                warning,
                            )
                        }
                        Err(_) => {
                            let _ = sender.send(WorkMessage::MetadataComplete {
                                results: Vec::new(),
                                cancelled: true,
                                error: None,
                            });
                            return;
                        }
                    }
                }
            };

            let total = paths.len();
            let _ = sender.send(WorkMessage::MetadataProgress {
                completed: 0,
                total,
            });
            let inspected = metadata::inspect_files(&paths, &worker_control, |completed, total| {
                let _ = sender.send(WorkMessage::MetadataProgress { completed, total });
            });
            let (results, cancelled) = match inspected {
                Ok((results, cancelled)) => (results, cancelled),
                Err(error) => {
                    scan_error = Some(match scan_error {
                        Some(scan_error) => format!("{scan_error}；{error}"),
                        None => error,
                    });
                    (Vec::new(), false)
                }
            };
            let _ = sender.send(WorkMessage::MetadataComplete {
                results,
                cancelled,
                error: scan_error,
            });
        });
    }

    fn copy_plan(&self) -> CopyPlan {
        let mut plan = CopyPlan {
            items: Vec::new(),
            filtered: 0,
            unmapped: 0,
            overlap: 0,
            duplicate_targets: 0,
            existing_targets: 0,
        };
        let Some(summary) = &self.last_scan else {
            return plan;
        };
        let mut seen_destinations = std::collections::BTreeSet::new();
        for file in &summary.files {
            if !self.include_system_files && is_system_junk(&file.relative_path) {
                plan.filtered += 1;
                continue;
            }
            let Some(destination_root) = self
                .routes
                .iter()
                .find(|route| route.extension == file.extension)
                .and_then(|route| route.destination.as_ref())
            else {
                plan.unmapped += 1;
                continue;
            };
            if self.roots.iter().any(|root| {
                destination_root.starts_with(&root.path) || root.path.starts_with(destination_root)
            }) {
                plan.overlap += 1;
                continue;
            }
            let destination = destination_root.join(&file.relative_path);
            let destination_key = destination.to_string_lossy().to_ascii_lowercase();
            if !seen_destinations.insert(destination_key) {
                plan.duplicate_targets += 1;
                continue;
            }
            if fs::symlink_metadata(&destination).is_ok() {
                plan.existing_targets += 1;
            }
            plan.items.push(RoutedFile {
                source: file.path.clone(),
                destination,
                expected_source: FileStamp {
                    size: file.size,
                    modified: file.modified,
                },
                expected_sha256: None,
            });
        }
        plan
    }

    fn start_copy(
        &mut self,
        items: Vec<RoutedFile>,
        preserve_existing: bool,
        resume_existing: bool,
    ) {
        if items.is_empty() || self.work_running {
            return;
        }
        let saved_items = items.iter().map(saved_copy_item).collect::<Vec<_>>();
        if let Err(error) = checkpoint::save_pending_copy(&saved_items) {
            if resume_existing {
                self.pending_copy_resume = Some(items);
            }
            self.status = format!("无法保存文件复制恢复检查点，未开始复制：{error}");
            return;
        }
        self.invalidate_after_copy_change();
        if !preserve_existing {
            self.copy_results.clear();
            self.copy_planned = items.len();
        }
        self.work_completed = 0;
        self.work_total = items.len();
        self.work_bytes = 0;
        self.work_kind = Some(WorkKind::Copy);
        self.work_running = true;
        self.status = format!("正在安全复制 {} 个文件…", items.len());
        let (sender, receiver) = mpsc::channel();
        let control = TaskControl::default();
        let worker_control = control.clone();
        self.work_receiver = Some(receiver);
        self.work_control = Some(control);
        thread::spawn(move || {
            let progress = |completed, total, bytes| {
                if completed % 32 == 0 || completed == total {
                    let _ = sender.send(WorkMessage::CopyProgress {
                        completed,
                        total,
                        bytes,
                    });
                }
            };
            let (results, cancelled) = if resume_existing {
                pipeline::resume_copy_batch(items, &worker_control, progress)
            } else {
                pipeline::copy_batch(items, &worker_control, progress)
            };
            let _ = sender.send(WorkMessage::CopyComplete { results, cancelled });
        });
    }

    fn start_verify(&mut self) {
        if self.work_running {
            return;
        }
        let pairs: Vec<(PathBuf, PathBuf)> = self
            .copy_results
            .iter()
            .filter(|item| item.result.is_ok())
            .map(|item| (item.source.clone(), item.destination.clone()))
            .collect();
        if pairs.is_empty() {
            self.status = "没有成功复制的文件可供校验。".into();
            return;
        }
        self.verify_results.clear();
        self.verify_coverage = None;
        self.work_completed = 0;
        self.work_total = pairs.len();
        self.work_bytes = 0;
        self.work_kind = Some(WorkKind::Verify);
        self.work_running = true;
        self.status = format!("正在重新读取并校验 {} 对文件…", pairs.len());
        let (sender, receiver) = mpsc::channel();
        let control = TaskControl::default();
        let worker_control = control.clone();
        self.work_receiver = Some(receiver);
        self.work_control = Some(control);
        thread::spawn(move || {
            let (results, cancelled) =
                pipeline::verify_batch(pairs, &worker_control, |completed, total| {
                    if completed % 32 == 0 || completed == total {
                        let _ = sender.send(WorkMessage::VerifyProgress { completed, total });
                    }
                });
            let _ = sender.send(WorkMessage::VerifyComplete {
                results,
                cancelled,
                coverage: None,
            });
        });
    }

    fn start_direct_verify(&mut self) {
        if self.work_running {
            return;
        }
        let (Some(source_root), Some(target_root)) = (
            self.direct_verify_source.clone(),
            self.direct_verify_target.clone(),
        ) else {
            self.status = "请先选择源目录和目标目录。".into();
            return;
        };
        if source_root.starts_with(&target_root) || target_root.starts_with(&source_root) {
            self.status = "独立校验的源和目标目录不能相同或互相嵌套；请选择互不重叠的目录。".into();
            return;
        }
        self.verify_results.clear();
        self.verify_coverage = None;
        self.work_completed = 0;
        self.work_total = 0;
        self.work_bytes = 0;
        self.work_kind = Some(WorkKind::Verify);
        self.work_running = true;
        self.status = "正在独立扫描并核对源目录与目标目录…".into();
        let (sender, receiver) = mpsc::channel();
        let control = TaskControl::default();
        let worker_control = control.clone();
        self.work_receiver = Some(receiver);
        self.work_control = Some(control);
        thread::spawn(move || {
            let source = scan_sources(&[source_root.clone()], &worker_control, |_| {});
            let target = scan_sources(&[target_root.clone()], &worker_control, |_| {});
            let (Ok(source), Ok(target)) = (source, target) else {
                let _ = sender.send(WorkMessage::VerifyComplete {
                    results: Vec::new(),
                    cancelled: true,
                    coverage: None,
                });
                return;
            };
            let source_files: BTreeMap<PathBuf, PathBuf> = source
                .files
                .iter()
                .map(|file| (file.relative_path.clone(), file.path.clone()))
                .collect();
            let target_files: BTreeMap<PathBuf, PathBuf> = target
                .files
                .iter()
                .map(|file| (file.relative_path.clone(), file.path.clone()))
                .collect();
            let missing: Vec<String> = source_files
                .keys()
                .filter(|relative| !target_files.contains_key(*relative))
                .map(|relative| relative.display().to_string())
                .collect();
            let extra: Vec<String> = target_files
                .keys()
                .filter(|relative| !source_files.contains_key(*relative))
                .map(|relative| relative.display().to_string())
                .collect();
            let pairs: Vec<(PathBuf, PathBuf)> = source_files
                .iter()
                .filter_map(|(relative, source)| {
                    target_files
                        .get(relative)
                        .map(|target| (source.clone(), target.clone()))
                })
                .collect();
            let coverage = VerifyCoverage {
                source_count: source_files.len(),
                target_count: target_files.len(),
                missing,
                extra,
                scan_errors: source.error_count + target.error_count,
            };
            let (results, cancelled) =
                pipeline::verify_batch(pairs, &worker_control, |completed, total| {
                    if completed % 32 == 0 || completed == total {
                        let _ = sender.send(WorkMessage::VerifyProgress { completed, total });
                    }
                });
            let _ = sender.send(WorkMessage::VerifyComplete {
                results,
                cancelled,
                coverage: Some(coverage),
            });
        });
    }

    fn receive_work_updates(&mut self) {
        let Some(receiver) = &self.work_receiver else {
            return;
        };
        loop {
            match receiver.try_recv() {
                Ok(WorkMessage::CopyProgress {
                    completed,
                    total,
                    bytes,
                }) => {
                    self.work_completed = completed;
                    self.work_total = total;
                    self.work_bytes = bytes;
                }
                Ok(WorkMessage::CopyComplete { results, cancelled }) => {
                    self.work_running = false;
                    self.work_kind = None;
                    self.work_receiver = None;
                    self.work_control = None;
                    for result in results {
                        if let Some(existing) = self.copy_results.iter_mut().find(|existing| {
                            existing.source == result.source
                                && existing.destination == result.destination
                        }) {
                            *existing = result;
                        } else {
                            self.copy_results.push(result);
                        }
                    }
                    if !cancelled && self.copy_results.iter().all(|item| item.result.is_ok()) {
                        self.pending_copy_resume = None;
                        if let Err(error) = checkpoint::clear_pending_copy() {
                            self.status = format!(
                                "复制已完成，但无法清除恢复检查点：{error}。下次启动时会重新核对已存在目标。"
                            );
                        }
                    } else {
                        self.pending_copy_resume = checkpoint::load_pending_copy()
                            .ok()
                            .flatten()
                            .and_then(|saved| {
                                saved
                                    .into_iter()
                                    .map(restored_copy_item)
                                    .collect::<std::io::Result<Vec<_>>>()
                                    .ok()
                            });
                    }
                    let passed = self
                        .copy_results
                        .iter()
                        .filter(|item| item.result.is_ok())
                        .count();
                    let failed = self.copy_results.len() - passed;
                    if cancelled {
                        self.status = format!(
                            "复制已停止：完成 {passed}，失败 {failed}；剩余项目未开始。恢复计划已保存，可在重启后继续并复核已存在目标。"
                        );
                        self.apply_pending_stage();
                    } else {
                        self.status = format!(
                            "复制阶段结束：成功 {passed}，失败 {failed}。来源文件未修改。 "
                        );
                        self.handoff_title = "分类复制已完成".into();
                        self.handoff_description = format!(
                            "本阶段产生 {} 项复制结果，成功 {} 项、失败 {} 项。未完成复制计划保存在本机私有检查点；下次启动可选择恢复，已有目标会重新读取并校验。下一阶段仍会重新读取源和目标文件计算 SHA-256。",
                            self.copy_results.len(),
                            passed,
                            failed
                        );
                        self.show_handoff = !self.apply_pending_stage();
                    }
                    break;
                }
                Ok(WorkMessage::VerifyProgress { completed, total }) => {
                    self.work_completed = completed;
                    self.work_total = total;
                }
                Ok(WorkMessage::VerifyComplete {
                    results,
                    cancelled,
                    coverage,
                }) => {
                    self.work_running = false;
                    self.work_kind = None;
                    self.work_receiver = None;
                    self.work_control = None;
                    self.verify_results = results;
                    self.verify_coverage = coverage;
                    let matching = self
                        .verify_results
                        .iter()
                        .filter(|item| item.result.as_ref().is_ok_and(|result| result.matches))
                        .count();
                    let failed = self.verify_results.len() - matching;
                    if cancelled {
                        self.status = format!(
                            "校验已取消：完成 {} 项，一致 {} 项。",
                            self.verify_results.len(),
                            matching
                        );
                        self.apply_pending_stage();
                    } else {
                        let coverage_pass = self.verify_coverage.as_ref().is_none_or(|coverage| {
                            coverage.missing.is_empty()
                                && coverage.extra.is_empty()
                                && coverage.scan_errors == 0
                                && coverage.source_count == coverage.target_count
                        });
                        let passed = failed == 0 && coverage_pass;
                        self.status = format!(
                            "校验完成：{}；一致 {matching} 项，不一致或读取失败 {failed} 项。",
                            if passed { "PASS" } else { "FAIL" }
                        );
                        self.handoff_title = "完整性校验已完成".into();
                        self.handoff_description = format!(
                            "已重新读取 {} 对源/目标文件并比较大小与 SHA-256；一致 {} 项，不一致或读取失败 {} 项。",
                            self.verify_results.len(),
                            matching,
                            failed
                        );
                        self.show_handoff = !self.apply_pending_stage();
                    }
                    break;
                }
                Ok(WorkMessage::MetadataProgress { completed, total }) => {
                    self.work_completed = completed;
                    self.work_total = total;
                }
                Ok(WorkMessage::MetadataComplete {
                    results,
                    cancelled,
                    error,
                }) => {
                    self.work_running = false;
                    self.work_kind = None;
                    self.work_receiver = None;
                    self.work_control = None;
                    self.metadata_results = results;
                    let passed = self
                        .metadata_results
                        .iter()
                        .filter(|item| {
                            item.gps_status == MetadataStatus::Valid
                                && item.time_status == MetadataStatus::Valid
                        })
                        .count();
                    let read_errors = self
                        .metadata_results
                        .iter()
                        .filter(|item| item.gps_status == MetadataStatus::ReadError)
                        .count();
                    let warnings = self
                        .metadata_results
                        .len()
                        .saturating_sub(passed + read_errors);

                    if cancelled {
                        self.status = format!(
                            "元数据检查已取消：已完成 {} 项；未检查项目可重新运行。",
                            self.metadata_results.len()
                        );
                        self.apply_pending_stage();
                    } else if self.metadata_results.is_empty() && error.is_some() {
                        self.status = format!(
                            "元数据检查未能启动：{}",
                            error.as_deref().unwrap_or("未知错误")
                        );
                        self.apply_pending_stage();
                    } else {
                        self.status = format!(
                            "元数据检查完成：{} 项 PASS，{} 项需人工查看，{} 项读取错误。{}",
                            passed,
                            warnings,
                            read_errors,
                            error.as_deref().unwrap_or("")
                        );
                        self.handoff_title = "元数据检查已完成".into();
                        self.handoff_description = format!(
                            "已逐项读取 {} 个当前文件的真实元数据：GPS 与拍摄时间均有效 {} 项；警告或不支持 {} 项；读取错误 {} 项。EXIF、GPS 和拍摄时间结果只保留在本次会话内存中。{}",
                            self.metadata_results.len(),
                            passed,
                            warnings,
                            read_errors,
                            error.as_deref().unwrap_or("")
                        );
                        self.show_handoff = !self.apply_pending_stage();
                    }
                    break;
                }
                Ok(WorkMessage::DuplicateProgress { completed, total }) => {
                    self.work_completed = completed;
                    self.work_total = total;
                }
                Ok(WorkMessage::DuplicateComplete {
                    report,
                    cancelled,
                    error,
                }) => {
                    self.work_running = false;
                    self.work_kind = None;
                    self.work_receiver = None;
                    self.work_control = None;
                    let exact_groups = report.exact_groups.len();
                    let candidates = report.time_candidates.len();
                    let hash_errors = report.hash_errors.len();
                    self.duplicate_report = Some(report);
                    if cancelled {
                        self.status = "重复检查已取消；未完成的文件需重新扫描。".into();
                        self.apply_pending_stage();
                    } else {
                        let warning = error.unwrap_or_default();
                        self.status = format!(
                            "重复检查完成：{} 个精确重复组，{} 个时间候选，{} 个 SHA-256 读取错误。{}",
                            exact_groups, candidates, hash_errors, warning
                        );
                        self.handoff_title = "重复检查已完成".into();
                        self.handoff_description = format!(
                            "本轮重新扫描并检查了目录。发现 {} 个字节完全相同组、{} 个时间候选、{} 个 SHA-256 读取错误。携带时会把成功计算 SHA-256 且未排除的路径组成 Final Manifest，并将路径、哈希和排除决定保存到本机私有检查点；重启后可恢复审核决定，阶段⑦/⑧仍会重新扫描并核对内容。当前人工排除标记共 {} 项；不会删除磁盘文件。{}",
                            exact_groups,
                            candidates,
                            hash_errors,
                            self.excluded_final_paths.len(),
                            warning
                        );
                        self.show_handoff = !self.apply_pending_stage();
                    }
                    break;
                }
                Ok(WorkMessage::BackupProgress {
                    completed,
                    total,
                    bytes,
                }) => {
                    self.work_completed = completed;
                    self.work_total = total;
                    self.work_bytes = bytes;
                }
                Ok(WorkMessage::BackupComplete {
                    copies,
                    verification,
                    cancelled,
                    error,
                    verify_only,
                }) => {
                    self.work_running = false;
                    self.work_kind = None;
                    self.work_receiver = None;
                    self.work_control = None;
                    let backup_incomplete = cancelled
                        || backup_error_requires_retry(error.as_deref())
                        || copies.iter().any(|item| item.result.is_err())
                        || verification
                            .iter()
                            .any(|item| !item.result.as_ref().is_ok_and(|result| result.matches));
                    if !verify_only {
                        if backup_incomplete {
                            self.pending_backup_resume = Some(self.backup_mappings.clone());
                        } else {
                            self.pending_backup_resume = None;
                            if let Err(error) = checkpoint::clear_pending_backup() {
                                self.status = format!(
                                    "备份已完成，但无法清除恢复检查点：{error}。下次启动会重新扫描并核对目标。"
                                );
                            }
                        }
                    }
                    let copied = copies.iter().filter(|item| item.result.is_ok()).count();
                    let copy_failed = copies.len().saturating_sub(copied);
                    let verified = verification
                        .iter()
                        .filter(|item| item.result.as_ref().is_ok_and(|result| result.matches))
                        .count();
                    let verify_failed = verification.len().saturating_sub(verified);
                    if !verify_only {
                        self.backup_copies = copies;
                    }
                    self.backup_verification = verification;
                    let warning = error.unwrap_or_default();
                    if cancelled {
                        self.status = format!(
                            "备份已取消：复制成功 {copied} 项、失败 {copy_failed} 项；读回校验 PASS {verified} 项、FAIL {verify_failed} 项。恢复记录已保存，下次可重新扫描并继续。{warning}"
                        );
                        self.apply_pending_stage();
                    } else if verify_only {
                        self.status = format!(
                            "备份重新检查完成：PASS {verified} 项，FAIL {verify_failed} 项。{warning}"
                        );
                        self.handoff_title = "备份重新检查已完成".into();
                        self.handoff_description = format!(
                            "已重新扫描所选本地目录和目标，并重新读取 {} 项源/目标 SHA-256。PASS {verified} 项、FAIL {verify_failed} 项。{warning}",
                            self.backup_verification.len()
                        );
                        self.show_handoff = !self.apply_pending_stage();
                    } else {
                        self.status = format!(
                            "备份完成：复制成功 {copied} 项、失败 {copy_failed} 项；独立读回校验 PASS {verified} 项、FAIL {verify_failed} 项。{warning}"
                        );
                        self.handoff_title = "文件备份已完成".into();
                        self.handoff_description = format!(
                            "各所选备份目标分别执行了复制和逐文件 SHA-256 读回验证：复制成功 {copied} 项、失败 {copy_failed} 项；校验 PASS {verified} 项、FAIL {verify_failed} 项。未完成映射会保存在本机私有检查点；下次可重新扫描源并校验已有目标。{warning}"
                        );
                        self.show_handoff = !self.apply_pending_stage();
                    }
                    break;
                }
                Ok(WorkMessage::ArchiveProgress {
                    completed,
                    total,
                    bytes,
                }) => {
                    self.work_completed = completed;
                    self.work_total = total;
                    self.work_bytes = bytes;
                }
                Ok(WorkMessage::ArchiveComplete { result, cancelled }) => {
                    self.work_running = false;
                    self.work_kind = None;
                    self.work_receiver = None;
                    self.work_control = None;
                    match result {
                        Ok(result) => {
                            let recovery_warning = match checkpoint::clear_pending_archive() {
                                Ok(()) => {
                                    self.pending_archive_resume = None;
                                    String::new()
                                }
                                Err(error) => format!("归档完成，但无法清理恢复记录：{error}。"),
                            };
                            self.status = format!(
                                "归档完成并通过逐文件 SHA-256 还原流校验：{} 个文件，{} 卷，输入 {}，归档 {}。{}{}",
                                result.file_count,
                                result.volume_count,
                                format_bytes(result.total_bytes),
                                format_bytes(result.archive_bytes),
                                result.cleanup_warning.as_deref().unwrap_or(""),
                                recovery_warning
                            );
                            self.handoff_title = "加密归档已完成".into();
                            self.handoff_description = format!(
                                "已在临时目录完成归档清单和逐文件 SHA-256 读取校验，并防覆盖提交卷集：{}。验证文件 {} 项；输入 {}；总归档大小 {}。这套卷对应一个逻辑 7z 归档。{}{}",
                                result
                                    .volume_paths
                                    .iter()
                                    .map(|path| path.display().to_string())
                                    .collect::<Vec<_>>()
                                    .join("、"),
                                result.file_count,
                                format_bytes(result.total_bytes),
                                format_bytes(result.archive_bytes),
                                result.cleanup_warning.as_deref().unwrap_or(""),
                                recovery_warning
                            );
                            self.show_handoff = !self.apply_pending_stage();
                        }
                        Err(error) => {
                            self.status = if cancelled {
                                format!("归档已取消，临时文件已清理（如可访问）：{error}")
                            } else {
                                format!("归档未提交：{error}")
                            };
                            self.apply_pending_stage();
                        }
                    }
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.work_running = false;
                    self.work_kind = None;
                    self.work_receiver = None;
                    self.work_control = None;
                    self.status = "后台处理意外结束；请检查结果并重新运行。".into();
                    break;
                }
            }
        }
    }

    fn footer(&mut self, ui: &mut eframe::egui::Ui, ctx: &eframe::egui::Context) {
        ui.horizontal(|ui| {
            for (index, label) in STAGES.iter().enumerate() {
                let marker = if index == self.stage { "●" } else { "○" };
                if ui
                    .selectable_label(
                        index == self.stage,
                        format!("{} {}\n{}", marker, index + 1, label),
                    )
                    .clicked()
                {
                    self.request_stage(index);
                }
            }
            if ui.button("开源许可").clicked() {
                self.show_licenses = true;
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            if self.scanning {
                ui.spinner();
                let paused = self
                    .cancel_scan
                    .as_ref()
                    .is_some_and(TaskControl::is_paused);
                ui.label(if paused {
                    format!(
                        "阶段任务：扫描暂停中 · 已发现 {} 个文件 / {}",
                        self.scan_progress.files,
                        format_bytes(self.scan_progress.bytes)
                    )
                } else {
                    format!(
                        "阶段任务：已发现 {} 个文件 / {}",
                        self.scan_progress.files,
                        format_bytes(self.scan_progress.bytes)
                    )
                });
            } else if self.work_running {
                ui.spinner();
                if self
                    .work_control
                    .as_ref()
                    .is_some_and(TaskControl::is_paused)
                {
                    ui.label("任务暂停中（在安全处理边界生效）。");
                }
                let operation = match self.work_kind {
                    Some(WorkKind::Copy) => format!(
                        "复制 {} / {} 项，已写入 {}",
                        self.work_completed,
                        self.work_total,
                        format_bytes(self.work_bytes)
                    ),
                    Some(WorkKind::Verify) => {
                        format!("校验 {} / {} 项", self.work_completed, self.work_total)
                    }
                    Some(WorkKind::Metadata) => format!(
                        "读取元数据 {} / {} 项",
                        self.work_completed, self.work_total
                    ),
                    Some(WorkKind::Duplicates) => {
                        format!("重复检查 {} / {} 项", self.work_completed, self.work_total)
                    }
                    Some(WorkKind::Backup) => format!(
                        "复制并校验 {} / {} 项，已写入 {}",
                        self.work_completed,
                        self.work_total,
                        format_bytes(self.work_bytes)
                    ),
                    Some(WorkKind::Archive) => format!(
                        "归档与验证 {} / {} 项，已处理 {}",
                        self.work_completed,
                        self.work_total,
                        format_bytes(self.work_bytes)
                    ),
                    None => "准备阶段任务…".into(),
                };
                ui.label(format!("阶段任务：{operation}"));
            } else if self.pending_copy_resume.is_some() {
                ui.label("阶段状态：发现可恢复的分类复制任务");
            } else if self.pending_backup_resume.is_some() {
                ui.label("阶段状态：发现可恢复的备份任务");
            } else if self.last_scan.is_some() {
                ui.label("阶段状态：已扫描（来源文件清单保存在当前会话内存中）");
            } else {
                ui.label("阶段状态：未开始");
            }
            ui.with_layout(
                eframe::egui::Layout::right_to_left(eframe::egui::Align::Center),
                |ui| {
                    if (self.scanning || self.work_running)
                        && let Some(control) =
                            self.cancel_scan.as_ref().or(self.work_control.as_ref())
                    {
                        let paused = control.is_paused();
                        if ui
                            .button(if paused {
                                "恢复任务"
                            } else {
                                "暂停任务"
                            })
                            .clicked()
                        {
                            control.set_paused(!paused);
                        }
                    }
                    if ui.button("下一阶段 →").clicked() {
                        self.request_stage((self.stage + 1).min(STAGES.len() - 1));
                    }
                    if ui.button("← 上一阶段").clicked() {
                        self.request_stage(self.stage.saturating_sub(1));
                    }
                    if self.scanning && ui.button("取消扫描").clicked() {
                        if let Some(control) = &self.cancel_scan {
                            control.cancel();
                            self.status = "正在取消扫描…".into();
                        }
                    }
                    if self.work_running && ui.button("取消当前任务").clicked() {
                        if let Some(control) = &self.work_control {
                            control.cancel();
                        }
                    }
                },
            );
        });
        if self.work_running && self.work_total > 0 {
            ui.add(
                eframe::egui::ProgressBar::new(self.work_completed as f32 / self.work_total as f32)
                    .text(format!("{} / {}", self.work_completed, self.work_total)),
            );
        } else if self.scanning {
            ui.add(
                eframe::egui::ProgressBar::new(0.0)
                    .animate(
                        !self
                            .cancel_scan
                            .as_ref()
                            .is_some_and(TaskControl::is_paused),
                    )
                    .text(
                        if self
                            .cancel_scan
                            .as_ref()
                            .is_some_and(TaskControl::is_paused)
                        {
                            "扫描暂停中"
                        } else {
                            "正在枚举文件"
                        },
                    ),
            );
        }
        let _ = ctx;
    }

    fn start_scan(&mut self) {
        let roots: Vec<PathBuf> = self.roots.iter().map(|root| root.path.clone()).collect();
        self.invalidate_copy_plan();
        let (sender, receiver) = mpsc::channel();
        let control = TaskControl::default();
        let worker_control = control.clone();
        self.scan_receiver = Some(receiver);
        self.cancel_scan = Some(control);
        self.scanning = true;
        self.final_manifest = None;
        self.scan_progress = ScanProgress::default();
        self.last_scan = None;
        self.status = "正在读取目录清单…".into();
        thread::spawn(move || {
            let summary = scan_sources(&roots, &worker_control, |progress| {
                let _ = sender.send(ScanMessage::Progress(progress));
            });
            match summary {
                Ok(summary) => {
                    let _ = sender.send(ScanMessage::Complete(summary));
                }
                Err(progress) => {
                    let _ = sender.send(ScanMessage::Cancelled(progress));
                }
            }
        });
    }

    fn sync_routes(&mut self) {
        let Some(summary) = &self.last_scan else {
            return;
        };
        for extension in summary
            .files
            .iter()
            .filter(|file| !is_system_junk(&file.relative_path))
            .map(|file| &file.extension)
        {
            if !self
                .routes
                .iter()
                .any(|route| route.extension == *extension)
            {
                self.routes.push(ExtensionRoute {
                    extension: extension.clone(),
                    destination: None,
                });
            }
        }
        self.routes.sort_by(|a, b| a.extension.cmp(&b.extension));
    }

    fn restore_saved_review(&mut self) {
        match checkpoint::load() {
            Ok(Some(saved)) => {
                let count = saved.file_sha256.len();
                let excluded = saved.excluded_paths.len();
                self.final_manifest = Some(FinalManifest {
                    root: saved.root,
                    file_sha256: saved.file_sha256,
                });
                self.excluded_final_paths = saved.excluded_paths;
                self.review_checkpoint_saved = true;
                self.status = format!(
                    "已从本机私有检查点恢复阶段⑥人工审核：{count} 个已确认文件、{excluded} 个排除决定。备份或归档前仍会重新读取并核对 SHA-256。"
                );
            }
            Ok(None) => self.review_checkpoint_saved = false,
            Err(error) => {
                self.review_checkpoint_saved = false;
                self.status = format!(
                    "无法恢复上次阶段⑥检查点：{error}。请重新执行重复检查；现有文件未被修改。"
                );
            }
        }
    }

    fn restore_pending_copy(&mut self) {
        match checkpoint::load_pending_copy() {
            Ok(Some(saved)) => match saved
                .into_iter()
                .map(restored_copy_item)
                .collect::<std::io::Result<Vec<_>>>()
            {
                Ok(items) => {
                    let count = items.len();
                    self.pending_copy_resume = Some(items);
                    self.stage = 2;
                    self.status = format!(
                        "发现上次未完成的分类复制任务（{count} 项）。请在阶段③选择恢复或丢弃；恢复会重新核对已有目标。"
                    );
                }
                Err(error) => {
                    self.status =
                        format!("无法读取上次分类复制任务：{error}。记录仍保留供检查或丢弃。");
                }
            },
            Ok(None) => self.pending_copy_resume = None,
            Err(error) => {
                self.status = format!("无法读取上次分类复制检查点：{error}。现有文件未被修改。");
            }
        }
    }

    fn restore_pending_backup(&mut self) {
        match checkpoint::load_pending_backup() {
            Ok(Some(saved)) => {
                let mappings = saved
                    .into_iter()
                    .map(restored_backup_mapping)
                    .collect::<Vec<_>>();
                self.backup_mappings = mappings.clone();
                self.pending_backup_resume = Some(mappings);
                if self.pending_copy_resume.is_none() {
                    self.stage = 6;
                    self.status =
                        "发现上次未完成的备份任务。阶段⑦可选择重新扫描并恢复；已有目标会重新校验。"
                            .into();
                } else {
                    self.status = "发现未完成的分类复制和备份任务；可从阶段③或⑦分别恢复。".into();
                }
            }
            Ok(None) => self.pending_backup_resume = None,
            Err(error) => {
                self.status = format!("无法读取上次备份检查点：{error}。现有文件未被修改。");
            }
        }
    }

    fn restore_pending_archive(&mut self) {
        match checkpoint::load_pending_archive() {
            Ok(Some(plan)) => {
                self.archive_source = Some(plan.source.clone());
                self.archive_output_directory = Some(plan.output_directory.clone());
                self.archive_name = plan.name.clone();
                self.archive_name_custom = true;
                self.archive_options.volume_mb = plan.volume_mb;
                if plan.manifest_fingerprint == self.archive_manifest_fingerprint(&plan.source) {
                    self.pending_archive_resume = Some(plan);
                    if self.pending_copy_resume.is_none() && self.pending_backup_resume.is_none() {
                        self.stage = 7;
                        self.status =
                            "发现上次未完成的加密归档。阶段⑧可重新扫描来源并从头重建；请重新输入密码。"
                                .into();
                    } else {
                        self.status =
                            "另有未完成的加密归档恢复记录；阶段⑧可重新扫描并从头重建，密码需重新输入。"
                                .into();
                    }
                } else {
                    self.pending_archive_resume = None;
                    let cleanup_warning = checkpoint::clear_pending_archive()
                        .err()
                        .map(|error| format!("无法清除旧检查点：{error}。"))
                        .unwrap_or_default();
                    self.status = format!(
                        "已恢复归档输入，但阶段⑥审核范围与中断前不同或不可用；旧恢复记录已作废。请核对当前审核清单后重新开始。{cleanup_warning}"
                    );
                }
            }
            Ok(None) => self.pending_archive_resume = None,
            Err(error) => {
                self.status = format!("无法读取上次加密归档检查点：{error}。现有文件未被修改。");
            }
        }
    }

    fn archive_manifest_fingerprint(&self, source: &Path) -> Option<String> {
        let manifest = self
            .final_manifest
            .as_ref()
            .filter(|manifest| manifest.applies_to(source))?;
        let data = serde_json::to_vec(&(
            &manifest.root,
            &manifest.file_sha256,
            &self.excluded_final_paths,
        ))
        .ok()?;
        let mut fingerprint = String::with_capacity(64);
        for byte in Sha256::digest(data) {
            use std::fmt::Write;
            write!(fingerprint, "{byte:02x}").expect("writing to a string cannot fail");
        }
        Some(fingerprint)
    }

    fn discard_pending_archive(&mut self) {
        self.pending_archive_resume = None;
        match checkpoint::clear_pending_archive() {
            Ok(()) => {
                self.status = "已丢弃加密归档恢复记录；可能存在的临时文件和已有归档未删除。".into()
            }
            Err(error) => {
                self.status = format!("无法删除加密归档检查点：{error}；现有文件未修改。")
            }
        }
    }

    fn invalidate_pending_archive(&mut self) {
        if self.pending_archive_resume.take().is_some()
            && let Err(error) = checkpoint::clear_pending_archive()
        {
            self.status = format!(
                "归档输入已更改，但无法清除旧恢复检查点：{error}。重新开始时会尝试覆盖检查点。"
            );
        }
    }

    fn resume_pending_backup(&mut self) {
        let Some(mappings) = self.pending_backup_resume.take() else {
            return;
        };
        self.backup_mappings = mappings;
        self.start_backup(false, true);
    }

    fn discard_pending_backup(&mut self) {
        self.pending_backup_resume = None;
        match checkpoint::clear_pending_backup() {
            Ok(()) => self.status = "已丢弃备份恢复记录；现有目标文件未删除。".into(),
            Err(error) => {
                self.status = format!("无法删除备份恢复检查点：{error}；现有文件未修改。")
            }
        }
    }

    fn resume_pending_copy(&mut self) {
        let Some(items) = self.pending_copy_resume.take() else {
            return;
        };
        self.stage = 2;
        self.start_copy(items, false, true);
    }

    fn discard_pending_copy(&mut self) {
        self.pending_copy_resume = None;
        match checkpoint::clear_pending_copy() {
            Ok(()) => self.status = "已丢弃未完成复制的恢复记录；现有目标文件未删除。".into(),
            Err(error) => {
                self.status = format!("无法删除未完成复制检查点：{error}；现有目标文件未修改。")
            }
        }
    }

    fn persist_saved_review(&mut self) {
        self.invalidate_pending_archive();
        self.clear_backup_results();
        let Some(manifest) = &self.final_manifest else {
            self.clear_saved_review();
            return;
        };
        let review = checkpoint::SavedReview {
            root: manifest.root.clone(),
            file_sha256: manifest.file_sha256.clone(),
            excluded_paths: self.excluded_final_paths.clone(),
        };
        match checkpoint::save(&review) {
            Ok(()) => self.review_checkpoint_saved = true,
            Err(error) => {
                self.review_checkpoint_saved = false;
                self.status = format!(
                    "阶段⑥审核在本次运行中已携带，但无法写入本机私有恢复检查点：{error}。应用退出后需要重新审核。"
                );
            }
        }
    }

    fn clear_saved_review(&mut self) {
        self.final_manifest = None;
        self.review_checkpoint_saved = false;
        self.excluded_final_paths.clear();
        self.clear_backup_results();
        self.pending_backup_resume = None;
        if let Err(error) = checkpoint::clear_pending_backup() {
            self.status = format!("无法清除过期备份检查点：{error}");
        }
        self.pending_archive_resume = None;
        if let Err(error) = checkpoint::clear_pending_archive() {
            self.status = format!("无法清除过期归档检查点：{error}");
        }
        if let Err(error) = checkpoint::clear() {
            self.status = format!("无法清除本机阶段⑥恢复检查点：{error}");
        }
    }

    fn refresh_carried_review(&mut self) {
        let (Some(root), Some(report)) = (
            self.duplicate_directory.clone(),
            self.duplicate_report.as_ref(),
        ) else {
            return;
        };
        self.final_manifest = Some(FinalManifest::from_review(
            root,
            report,
            &self.excluded_final_paths,
        ));
        self.persist_saved_review();
        if self.review_checkpoint_saved {
            self.status = "阶段⑥最终清单已更新；此前备份校验结果已清除。".into();
        }
    }

    fn clear_backup_results(&mut self) {
        self.backup_copies.clear();
        self.backup_verification.clear();
    }

    fn clear_verify_results(&mut self) {
        self.verify_results.clear();
        self.verify_coverage = None;
        self.status = "校验路径已更改；旧校验结果已清除。".into();
    }

    fn invalidate_after_copy_change(&mut self) {
        self.clear_verify_results();
        self.metadata_results.clear();
        self.duplicate_report = None;
        self.clear_candidate_preview();
        self.clear_saved_review();
    }

    fn invalidate_copy_plan(&mut self) {
        self.copy_results.clear();
        self.copy_planned = 0;
        self.pending_copy_resume = None;
        if let Err(error) = checkpoint::clear_pending_copy() {
            self.status = format!("无法清除过期复制检查点：{error}");
        }
        self.invalidate_after_copy_change();
        self.status = "来源、路由或过滤设置已更改；依赖的旧结果已清除。".into();
    }

    fn receive_scan_updates(&mut self) {
        let Some(receiver) = &self.scan_receiver else {
            return;
        };
        loop {
            match receiver.try_recv() {
                Ok(ScanMessage::Progress(progress)) => self.scan_progress = progress,
                Ok(ScanMessage::Complete(summary)) => {
                    self.scan_progress = ScanProgress {
                        files: summary.files.len(),
                        bytes: summary.bytes,
                        directories: self.scan_progress.directories,
                    };
                    self.scanning = false;
                    self.status = format!("扫描完成：{} 个文件。", summary.files.len());
                    self.last_scan = Some(summary);
                    self.sync_routes();
                    self.scan_receiver = None;
                    self.cancel_scan = None;
                    self.handoff_title = "来源扫描已完成".into();
                    self.handoff_description = "下一阶段可接收来源路径、文件清单、大小、修改时间和扩展名。数据只在本次运行内存中传递。".into();
                    self.show_handoff = !self.apply_pending_stage();
                    break;
                }
                Ok(ScanMessage::Cancelled(progress)) => {
                    self.scan_progress = progress;
                    self.scanning = false;
                    self.scan_receiver = None;
                    self.cancel_scan = None;
                    self.status = "扫描已取消；本次未保存不完整清单。".into();
                    self.apply_pending_stage();
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.scanning = false;
                    self.scan_receiver = None;
                    self.cancel_scan = None;
                    self.status = "扫描线程意外结束；请重新扫描。".into();
                    break;
                }
            }
        }
    }

    fn handoff_dialog(&mut self, ctx: &eframe::egui::Context) {
        let mut action = None;
        eframe::egui::Window::new(&self.handoff_title)
            .collapsible(false)
            .resizable(false)
            .anchor(eframe::egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(&self.handoff_description);
                ui.add_space(8.0);
                let carry_button = eframe::egui::Button::new(
                    eframe::egui::RichText::new("携带本阶段信息并进入下一阶段")
                        .color(eframe::egui::Color32::WHITE),
                )
                .fill(eframe::egui::Color32::from_rgb(32, 105, 174));
                if ui.add(carry_button).clicked() {
                    action = Some(0);
                }
                if ui.button("不携带信息并进入下一阶段").clicked() {
                    action = Some(1);
                }
                if ui.button("停留在本阶段").clicked() {
                    action = Some(2);
                }
                if ui.button("退出流程").clicked() {
                    self.show_handoff = false;
                    ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
                }
            });
        match action {
            Some(0) => {
                self.show_handoff = false;
                if self.stage == 5 {
                    self.refresh_carried_review();
                }
                self.stage = (self.stage + 1).min(STAGES.len() - 1);
            }
            Some(1) => {
                self.show_handoff = false;
                self.last_scan = None;
                self.routes.clear();
                self.copy_results.clear();
                self.verify_results.clear();
                self.verify_coverage = None;
                self.direct_verify_source = None;
                self.direct_verify_target = None;
                self.metadata_directory = None;
                self.metadata_results.clear();
                self.duplicate_report = None;
                self.clear_saved_review();
                self.backup_mappings.clear();
                self.backup_copies.clear();
                self.backup_verification.clear();
                self.status = "未携带来源清单；下一阶段需重新选择输入。".into();
                self.stage = (self.stage + 1).min(STAGES.len() - 1);
            }
            Some(2) => self.show_handoff = false,
            _ => {}
        }
    }

    fn license_dialog(&mut self, ctx: &eframe::egui::Context) {
        let mut open = self.show_licenses;
        eframe::egui::Window::new("开源许可与第三方声明")
            .open(&mut open)
            .default_size([760.0, 580.0])
            .resizable(true)
            .show(ctx, |ui| {
                let choices = [
                    "第三方声明",
                    "ExifTool · Artistic License 1.0",
                    "Windows ExifTool / Perl 许可文本",
                    "Rust 依赖完整许可清单",
                ];
                eframe::egui::ComboBox::from_label("查看文本")
                    .selected_text(choices[self.license_choice.min(choices.len() - 1)])
                    .show_ui(ui, |ui| {
                        for (index, label) in choices.iter().enumerate() {
                            ui.selectable_value(&mut self.license_choice, index, *label);
                        }
                    });
                let text = match self.license_choice {
                    1 => EXIFTOOL_LICENSE,
                    2 => EXIFTOOL_WINDOWS_LICENSES,
                    3 => RUST_DEPENDENCY_LICENSES,
                    _ => LICENSE_NOTICES,
                };
                eframe::egui::ScrollArea::vertical()
                    .max_height(500.0)
                    .show(ui, |ui| {
                        ui.add(eframe::egui::Label::new(text).wrap().selectable(true));
                    });
            });
        self.show_licenses = open;
    }

    fn request_stage(&mut self, stage: usize) {
        if stage == self.stage {
            return;
        }
        if self.scanning || self.work_running {
            self.pending_stage = Some(stage);
            self.show_switch_prompt = true;
        } else {
            if self.stage == 7 && stage != 7 {
                self.archive_password.zeroize();
                self.archive_confirmation.zeroize();
                self.archive_show_password = false;
            }
            self.clear_candidate_preview();
            self.stage = stage;
        }
    }

    fn apply_pending_stage(&mut self) -> bool {
        self.show_switch_prompt = false;
        if let Some(stage) = self.pending_stage.take() {
            self.clear_candidate_preview();
            self.stage = stage;
            true
        } else {
            false
        }
    }

    fn stage_switch_dialog(&mut self, ctx: &eframe::egui::Context) {
        let mut action = None;
        eframe::egui::Window::new("当前阶段仍在处理")
            .collapsible(false)
            .resizable(false)
            .anchor(eframe::egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("当前操作会在安全边界停止（扫描条目之间，或复制完当前文件后）。暂停并切换尚未实现。");
                ui.add_space(8.0);
                if ui.button("继续当前处理").clicked() {
                    action = Some(false);
                }
                if ui.button("取消当前处理并切换阶段").clicked() {
                    action = Some(true);
                }
            });
        match action {
            Some(false) => {
                self.show_switch_prompt = false;
                self.pending_stage = None;
            }
            Some(true) => {
                self.show_switch_prompt = false;
                if let Some(control) = &self.cancel_scan {
                    control.cancel();
                }
                if let Some(control) = &self.work_control {
                    control.cancel();
                }
                self.status = "已请求安全停止；当前文件完成后切换阶段。".into();
            }
            _ => {}
        }
    }
}

impl Drop for PhotoArchiveApp {
    fn drop(&mut self) {
        self.archive_password.zeroize();
        self.archive_confirmation.zeroize();
    }
}

fn insert_source_root(roots: &mut Vec<SourceRoot>, path: PathBuf) -> Result<bool, String> {
    let canonical = fs::canonicalize(&path)
        .map_err(|error| format!("无法解析目录路径 {}：{error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!("所选路径不是目录：{}", canonical.display()));
    }
    if roots.iter().any(|root| canonical.starts_with(&root.path)) {
        return Ok(false);
    }
    roots.retain(|root| !root.path.starts_with(&canonical));
    roots.push(SourceRoot { path: canonical });
    roots.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(true)
}

fn backup_mapping_errors(mappings: &[BackupMapping]) -> Vec<String> {
    let mut errors = std::collections::BTreeSet::new();
    let sources = mappings
        .iter()
        .filter_map(|mapping| mapping.source.as_ref())
        .collect::<Vec<_>>();
    let targets = mappings
        .iter()
        .flat_map(|mapping| mapping.targets.iter())
        .collect::<Vec<_>>();

    for mapping in mappings {
        let Some(source) = &mapping.source else {
            errors.insert("有一行尚未选择本地来源文件夹".to_string());
            continue;
        };
        if !source.is_dir() {
            errors.insert(format!("来源目录不存在或不可访问：{}", source.display()));
        }
        if mapping.targets.is_empty() {
            errors.insert(format!("{} 尚无备份目标", source.display()));
        }
        for target in &mapping.targets {
            if !target.is_dir() {
                errors.insert(format!("备份目标不存在或不可访问：{}", target.display()));
            }
            if source == target {
                errors.insert(format!("{} 与目标相同，没有独立备份副本", source.display()));
            }
        }
    }

    for (index, first) in sources.iter().enumerate() {
        for second in sources.iter().skip(index + 1) {
            if first.starts_with(second) || second.starts_with(first) {
                errors.insert(format!(
                    "来源目录互相包含，会重复纳入文件：{} 与 {}",
                    first.display(),
                    second.display()
                ));
            }
        }
        for target in &targets {
            if first.starts_with(target) || target.starts_with(first) {
                errors.insert(format!(
                    "来源与备份目标互相重叠：{} 与 {}；为避免递归或把备份当来源，请选择不重叠的目录",
                    first.display(),
                    target.display()
                ));
            }
        }
    }

    for (index, first) in targets.iter().enumerate() {
        for second in targets.iter().skip(index + 1) {
            if first != second && (first.starts_with(second) || second.starts_with(first)) {
                errors.insert(format!(
                    "备份目标互相包含：{} 与 {}；请选择互不嵌套的目标目录",
                    first.display(),
                    second.display()
                ));
            }
        }
    }
    errors.into_iter().collect()
}

fn append_task_error(error: &mut Option<String>, message: impl Into<String>) {
    let message = message.into();
    *error = Some(match error.take() {
        Some(existing) => format!("{existing}；{message}"),
        None => message,
    });
}

fn backup_error_requires_retry(error: Option<&str>) -> bool {
    error.is_some_and(|error| {
        error.split('；').any(|part| {
            !part.starts_with("按阶段⑥决定排除了") && !part.starts_with("最终清单之外的")
        })
    })
}

fn inspect_backup_targets(
    expected_by_target: &BTreeMap<PathBuf, (PathBuf, std::collections::BTreeSet<PathBuf>)>,
    control: &TaskControl,
) -> (Vec<VerifyItemResult>, Option<String>) {
    let mut results = Vec::new();
    let mut warnings = Vec::new();
    for (target, (source, expected_relative_paths)) in expected_by_target {
        if !control.checkpoint() {
            break;
        }
        match scan_sources(std::slice::from_ref(target), control, |_| {}) {
            Ok(summary) => {
                if summary.error_count > 0 {
                    warnings.push(format!(
                        "扫描备份目标 {} 时有 {} 项读取错误",
                        target.display(),
                        summary.error_count
                    ));
                    results.push(VerifyItemResult {
                        source: source.clone(),
                        destination: target.clone(),
                        result: Err(format!(
                            "目标目录扫描有 {} 项读取错误；示例：{}",
                            summary.error_count,
                            summary
                                .error_samples
                                .first()
                                .map(String::as_str)
                                .unwrap_or("未提供")
                        )),
                    });
                }
                for file in summary.files {
                    if !expected_relative_paths.contains(&file.relative_path) {
                        results.push(VerifyItemResult {
                            source: source.clone(),
                            destination: file.path,
                            result: Err("目标包含不在本次来源清单中的额外文件。".into()),
                        });
                    }
                }
            }
            Err(_) if control.is_cancelled() => break,
            Err(_) => {
                warnings.push(format!("无法完成目标扫描：{}", target.display()));
                results.push(VerifyItemResult {
                    source: source.clone(),
                    destination: target.clone(),
                    result: Err("目标扫描未完成；请检查磁盘连接和目录权限。".into()),
                });
            }
        }
    }
    let warning = (!warnings.is_empty()).then(|| warnings.join("；"));
    (results, warning)
}

fn scan_sources(
    roots: &[PathBuf],
    control: &TaskControl,
    mut report: impl FnMut(ScanProgress),
) -> Result<ScanSummary, ScanProgress> {
    let mut summary = ScanSummary::default();
    let mut progress = ScanProgress::default();
    for (source_id, root) in roots.iter().enumerate() {
        let mut pending = vec![root.clone()];
        while let Some(directory) = pending.pop() {
            if !control.checkpoint() {
                return Err(progress);
            }
            progress.directories += 1;
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) => {
                    record_scan_error(&mut summary, &directory, error);
                    continue;
                }
            };
            for entry in entries {
                if !control.checkpoint() {
                    return Err(progress);
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        record_scan_error(&mut summary, &directory, error);
                        continue;
                    }
                };
                let path = entry.path();
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        record_scan_error(&mut summary, &path, error);
                        continue;
                    }
                };
                let file_type = metadata.file_type();
                if file_type.is_symlink() {
                    summary.skipped += 1;
                } else if file_type.is_dir() {
                    pending.push(path);
                } else if file_type.is_file() {
                    let size = metadata.len();
                    let extension = path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .filter(|extension| !extension.is_empty())
                        .map(str::to_ascii_uppercase)
                        .unwrap_or_else(|| "(无扩展名)".into());
                    let relative_path = path
                        .strip_prefix(root)
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|_| path.clone());
                    summary.bytes = summary.bytes.saturating_add(size);
                    *summary.by_extension.entry(extension.clone()).or_default() += 1;
                    summary.files.push(FileRecord {
                        source_id,
                        path,
                        relative_path,
                        size,
                        modified: metadata.modified().ok(),
                        extension,
                    });
                    progress.files += 1;
                    progress.bytes = progress.bytes.saturating_add(size);
                    if progress.files % 128 == 0 {
                        report(progress);
                    }
                } else {
                    summary.skipped += 1;
                }
            }
            if progress.directories % 64 == 0 {
                report(progress);
            }
        }
    }
    summary.files.sort_by(|a, b| a.path.cmp(&b.path));
    report(progress);
    Ok(summary)
}

fn record_scan_error(summary: &mut ScanSummary, path: &Path, error: impl std::fmt::Display) {
    summary.error_count += 1;
    if summary.error_samples.len() < 10 {
        summary
            .error_samples
            .push(format!("{}：{error}", path.display()));
    }
}

fn is_system_junk(relative_path: &Path) -> bool {
    let in_system_directory = relative_path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        name.eq_ignore_ascii_case(".Spotlight-V100")
            || name.eq_ignore_ascii_case(".Trashes")
            || name.eq_ignore_ascii_case(".fseventsd")
    });
    let file_name = relative_path.file_name().map(|name| name.to_string_lossy());
    in_system_directory
        || file_name.is_some_and(|name| {
            name.eq_ignore_ascii_case(".DS_Store")
                || name.eq_ignore_ascii_case("Thumbs.db")
                || name.eq_ignore_ascii_case("desktop.ini")
                || name.starts_with("._")
        })
}

fn default_archive_name(source: &Path) -> String {
    let folder = source
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "Archive".into());
    format!("{folder} {}", utc_task_date())
}

fn utc_task_date() -> String {
    let unix_days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400;
    let days = i64::try_from(unix_days).unwrap_or(i64::MAX);
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}_{month:02}_{day:02}")
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn decode_candidate_preview(path: &Path) -> Result<PreviewPixels, String> {
    use image::ImageDecoder;

    image_formats::register_heif_decoder();
    let reader = image::ImageReader::open(path)
        .map_err(|error| format!("无法打开预览文件：{error}"))?
        .with_guessed_format()
        .map_err(|error| format!("无法识别图片格式：{error}"))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| format!("当前预览仅支持 JPEG/PNG：{error}"))?;
    let orientation = decoder
        .orientation()
        .map_err(|error| format!("无法读取图片方向：{error}"))?;
    let mut decoded = image::DynamicImage::from_decoder(decoder)
        .map_err(|error| format!("图片解码失败：{error}"))?;
    decoded.apply_orientation(orientation);
    Ok(PreviewPixels {
        image: decoded.thumbnail(640, 640).to_rgba8(),
    })
}

fn candidate_preview_texture(
    ctx: &eframe::egui::Context,
    path: &Path,
    preview: Result<PreviewPixels, String>,
) -> Result<eframe::egui::TextureHandle, String> {
    let rgba = preview?.image;
    let size = [rgba.width() as usize, rgba.height() as usize];
    let pixels = eframe::egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    Ok(ctx.load_texture(
        path.display().to_string(),
        pixels,
        eframe::egui::TextureOptions::LINEAR,
    ))
}

fn show_candidate_preview(
    ui: &mut eframe::egui::Ui,
    path: &Path,
    preview: &Result<eframe::egui::TextureHandle, String>,
) {
    ui.vertical(|ui| {
        ui.label(path.display().to_string());
        match preview {
            Ok(texture) => {
                let size = texture.size_vec2();
                let scale = (440.0 / size.x.max(size.y).max(1.0)).min(1.0);
                ui.image((texture.id(), size * scale));
            }
            Err(error) => {
                ui.colored_label(eframe::egui::Color32::from_rgb(160, 90, 0), error);
            }
        }
    });
}

fn saved_copy_item(item: &RoutedFile) -> checkpoint::SavedCopyItem {
    let (modified_before_epoch, modified_seconds, modified_nanos) =
        match item.expected_source.modified {
            Some(time) => {
                let (before_epoch, duration) = match time.duration_since(SystemTime::UNIX_EPOCH) {
                    Ok(duration) => (false, duration),
                    Err(error) => (true, error.duration()),
                };
                (
                    before_epoch,
                    Some(duration.as_secs()),
                    Some(duration.subsec_nanos()),
                )
            }
            None => (false, None, None),
        };
    checkpoint::SavedCopyItem {
        source: item.source.clone(),
        destination: item.destination.clone(),
        expected_size: item.expected_source.size,
        modified_before_epoch,
        modified_seconds,
        modified_nanos,
        expected_sha256: item.expected_sha256.clone(),
    }
}

fn restored_copy_item(item: checkpoint::SavedCopyItem) -> std::io::Result<RoutedFile> {
    let modified = match (item.modified_seconds, item.modified_nanos) {
        (Some(seconds), Some(nanos)) => {
            let duration = Duration::new(seconds, nanos);
            let time = if item.modified_before_epoch {
                SystemTime::UNIX_EPOCH.checked_sub(duration)
            } else {
                SystemTime::UNIX_EPOCH.checked_add(duration)
            }
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "复制任务修改时间超出系统范围",
                )
            })?;
            Some(time)
        }
        (None, None) => None,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "复制任务修改时间字段不完整",
            ));
        }
    };
    Ok(RoutedFile {
        source: item.source,
        destination: item.destination,
        expected_source: FileStamp {
            size: item.expected_size,
            modified,
        },
        expected_sha256: item.expected_sha256,
    })
}

fn saved_backup_mapping(mapping: &BackupMapping) -> Option<checkpoint::SavedBackupMapping> {
    Some(checkpoint::SavedBackupMapping {
        source: mapping.source.clone()?,
        targets: mapping.targets.clone(),
    })
}

fn restored_backup_mapping(mapping: checkpoint::SavedBackupMapping) -> BackupMapping {
    BackupMapping {
        source: Some(mapping.source),
        targets: mapping.targets,
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1080.0, 720.0])
            .with_min_inner_size([820.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Photo Archive Manager",
        options,
        Box::new(|creation_context| {
            creation_context
                .egui_ctx
                .set_visuals(eframe::egui::Visuals::light());
            let mut app = PhotoArchiveApp::default();
            app.restore_saved_review();
            app.restore_pending_copy();
            app.restore_pending_backup();
            app.restore_pending_archive();
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn carried_manifest_reflects_current_exclusions() {
        let root = PathBuf::from("/photos");
        let kept = root.join("keep.jpg");
        let excluded = root.join("exclude.jpg");
        let report = duplicates::DuplicateReport {
            file_sha256: BTreeMap::from([
                (kept.clone(), "a".repeat(64)),
                (excluded.clone(), "b".repeat(64)),
            ]),
            ..Default::default()
        };
        let manifest = FinalManifest::from_review(
            root.clone(),
            &report,
            &std::collections::BTreeSet::from([excluded.clone()]),
        );

        assert_eq!(manifest.root, root);
        assert_eq!(manifest.file_sha256.len(), 1);
        assert!(manifest.file_sha256.contains_key(&kept));
        assert!(!manifest.file_sha256.contains_key(&excluded));
    }

    fn temp_dir() -> PathBuf {
        let suffix = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "photo-archive-manager-test-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn accepted_backup_exclusions_do_not_keep_an_incomplete_checkpoint() {
        assert!(!backup_error_requires_retry(Some(
            "按阶段⑥决定排除了 2 个目标文件项；最终清单之外的 1 个目标文件项未备份"
        )));
        assert!(backup_error_requires_retry(Some(
            "扫描照片目录时有 1 项读取错误"
        )));
        assert!(!backup_error_requires_retry(None));
    }

    #[test]
    fn pending_copy_plan_preserves_source_stamp() {
        let root = temp_dir();
        let source = root.join("source.jpg");
        fs::write(&source, b"resume sample").unwrap();
        let item = RoutedFile {
            source: source.clone(),
            destination: root.join("destination.jpg"),
            expected_source: files::file_stamp(&source).unwrap(),
            expected_sha256: Some("a".repeat(64)),
        };

        let restored = restored_copy_item(saved_copy_item(&item)).unwrap();

        assert_eq!(restored.source, item.source);
        assert_eq!(restored.destination, item.destination);
        assert_eq!(restored.expected_source, item.expected_source);
        assert_eq!(restored.expected_sha256, item.expected_sha256);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn candidate_preview_scales_supported_images_and_reports_unsupported_files() {
        let root = temp_dir();
        let image_path = root.join("preview.png");
        image::RgbaImage::from_fn(1200, 800, |x, y| image::Rgba([x as u8, y as u8, 0, 255]))
            .save(&image_path)
            .unwrap();

        let preview = decode_candidate_preview(&image_path).unwrap();
        let (width, height) = preview.image.dimensions();
        assert_eq!(width, 640);
        assert!(height <= 640);

        let heif_path = root.join("generated.heic");
        let heif_pixels = (0..64 * 64)
            .flat_map(|index| {
                let value = (index % 256) as u8;
                [value, value.wrapping_mul(2), 255 - value, 255]
            })
            .collect::<Vec<_>>();
        let heif =
            heifer::encode_rgba8(64, 64, &heif_pixels, &heifer::EncodeOptions::default()).unwrap();
        fs::write(&heif_path, heif).unwrap();
        let heif_preview = decode_candidate_preview(&heif_path).unwrap();
        assert_eq!(heif_preview.image.dimensions(), (640, 640));
        assert!(duplicates::perceptual_hash(&heif_path).is_ok());

        let unsupported = root.join("unsupported.bin");
        fs::write(&unsupported, b"not an image").unwrap();
        let error = decode_candidate_preview(&unsupported).err().unwrap();
        assert!(error.contains("JPEG/PNG"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scanner_counts_files_and_groups_extensions() {
        let root = temp_dir();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("nested/one.jpg"), b"123").unwrap();
        fs::write(root.join("two.JPG"), b"45").unwrap();
        fs::write(root.join("README"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("nested"), root.join("nested-link")).unwrap();

        let summary = scan_sources(&[root.clone()], &TaskControl::default(), |_| {}).unwrap();

        assert_eq!(summary.files.len(), 3);
        assert_eq!(summary.bytes, 6);
        assert_eq!(summary.by_extension["JPG"], 2);
        assert_eq!(summary.by_extension["(无扩展名)"], 1);
        #[cfg(unix)]
        assert_eq!(summary.skipped, 1);
        assert!(
            summary
                .files
                .iter()
                .all(|file| file.path.starts_with(&root))
        );
        assert!(summary.files.iter().all(|file| file.source_id == 0));
        assert!(summary.files.iter().all(|file| file.size > 0));
        assert!(
            summary
                .files
                .iter()
                .all(|file| !file.relative_path.as_os_str().is_empty())
        );
        assert!(summary.files.iter().all(|file| file.modified.is_some()));
        assert!(summary.files.iter().all(|file| !file.extension.is_empty()));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scanner_can_be_cancelled_without_a_partial_manifest() {
        let root = temp_dir();
        fs::write(root.join("one.jpg"), b"1").unwrap();
        let control = TaskControl::default();
        control.cancel();

        assert!(scan_sources(&[root.clone()], &control, |_| {}).is_err());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn junk_filter_is_narrow_and_keeps_sidecars_and_hidden_files() {
        assert!(is_system_junk(Path::new(".DS_Store")));
        assert!(is_system_junk(Path::new("._IMG.HEIC")));
        assert!(is_system_junk(Path::new("nested/Thumbs.db")));
        assert!(is_system_junk(Path::new(".Spotlight-V100/Store/V2/file")));
        assert!(!is_system_junk(Path::new("IMG.XMP")));
        assert!(!is_system_junk(Path::new("IMG.AAE")));
        assert!(!is_system_junk(Path::new(".private/notes.txt")));
    }

    #[test]
    fn adding_parent_root_replaces_nested_root_and_ignores_duplicates() {
        let parent = temp_dir();
        let child = parent.join("child");
        fs::create_dir(&child).unwrap();
        let mut roots = Vec::new();

        assert!(insert_source_root(&mut roots, child.clone()).unwrap());
        assert!(!insert_source_root(&mut roots, child).unwrap());
        assert!(insert_source_root(&mut roots, parent.clone()).unwrap());
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, fs::canonicalize(&parent).unwrap());

        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn backup_target_overlap_is_blocked_but_multiple_disjoint_targets_are_valid() {
        let root = temp_dir();
        let source = root.join("Library/RAW");
        let target_one = root.join("BackupA/RAW");
        let target_two = root.join("BackupB/RAW");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target_one).unwrap();
        fs::create_dir_all(&target_two).unwrap();

        let valid = [BackupMapping {
            source: Some(source.clone()),
            targets: vec![target_one, target_two],
        }];
        assert!(backup_mapping_errors(&valid).is_empty());

        let overlapping = [BackupMapping {
            source: Some(source.clone()),
            targets: vec![source.join("backup")],
        }];
        assert!(!backup_mapping_errors(&overlapping).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_revalidates_each_independent_destination() {
        let root = temp_dir();
        let source = root.join("Library/Photos");
        let target_one = root.join("BackupA/Photos");
        let target_two = root.join("BackupB/Photos");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target_one).unwrap();
        fs::create_dir_all(&target_two).unwrap();
        fs::write(source.join("IMG-中文.JPG"), b"photo bytes").unwrap();
        let summary = scan_sources(&[source.clone()], &TaskControl::default(), |_| {}).unwrap();
        let items = summary
            .files
            .iter()
            .flat_map(|file| {
                [&target_one, &target_two]
                    .into_iter()
                    .map(|target| RoutedFile {
                        source: file.path.clone(),
                        destination: target.join(&file.relative_path),
                        expected_source: FileStamp {
                            size: file.size,
                            modified: file.modified,
                        },
                        expected_sha256: None,
                    })
            })
            .collect();
        let (copies, cancelled) =
            pipeline::copy_batch(items, &TaskControl::default(), |_, _, _| {});
        assert!(!cancelled);
        assert_eq!(copies.len(), 2);
        assert!(copies.iter().all(|item| item.result.is_ok()));
        let pairs = copies
            .iter()
            .map(|item| (item.source.clone(), item.destination.clone()))
            .collect();
        let (verification, cancelled) =
            pipeline::verify_batch(pairs, &TaskControl::default(), |_, _| {});
        assert!(!cancelled);
        assert_eq!(verification.len(), 2);
        assert!(
            verification
                .iter()
                .all(|item| item.result.as_ref().is_ok_and(|result| result.matches))
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_recheck_reports_unexpected_target_files() {
        let root = temp_dir();
        let source = root.join("Library/Photos");
        let target = root.join("Backup/Photos");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(source.join("IMG.JPG"), b"expected").unwrap();
        fs::write(target.join("IMG.JPG"), b"expected").unwrap();
        fs::write(target.join("old.jpg"), b"unexpected").unwrap();
        let expected = BTreeMap::from([(
            target.clone(),
            (
                source.clone(),
                std::collections::BTreeSet::from([PathBuf::from("IMG.JPG")]),
            ),
        )]);

        let (results, warning) = inspect_backup_targets(&expected, &TaskControl::default());

        assert!(warning.is_none());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].destination, target.join("old.jpg"));
        assert!(results[0].result.as_ref().unwrap_err().contains("额外文件"));
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod archive_poc;
