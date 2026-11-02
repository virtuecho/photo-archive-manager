use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::SystemTime,
};

mod duplicates;
pub mod files;
mod metadata;
mod pipeline;

use crate::{
    files::FileStamp,
    metadata::{MetadataRecord, MetadataStatus},
    pipeline::{CopyItemResult, RoutedFile, VerifyItemResult},
};

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

#[derive(Default)]
struct PhotoArchiveApp {
    stage: usize,
    roots: Vec<SourceRoot>,
    selected_root: Option<usize>,
    scan_receiver: Option<Receiver<ScanMessage>>,
    cancel_scan: Option<Arc<AtomicBool>>,
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
    backup_mappings: Vec<BackupMapping>,
    backup_copies: Vec<CopyItemResult>,
    backup_verification: Vec<VerifyItemResult>,
    work_receiver: Option<Receiver<WorkMessage>>,
    work_cancel: Option<Arc<AtomicBool>>,
    work_kind: Option<WorkKind>,
    work_running: bool,
    work_completed: usize,
    work_total: usize,
    work_bytes: u64,
    status: String,
    show_handoff: bool,
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
                            } else {
                                self.incomplete_stage(ui);
                            }
                        });
                },
            );
            ui.separator();
            self.footer(ui, &ctx);
        });

        if self.show_handoff {
            self.handoff_dialog(&ctx);
        }
        if self.show_switch_prompt {
            self.stage_switch_dialog(&ctx);
        }
        if self.scanning || self.work_running || self.show_handoff || self.show_switch_prompt {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

impl PhotoArchiveApp {
    fn source_selection(&mut self, ui: &mut eframe::egui::Ui) {
        ui.label("添加一个或多个来源目录。扫描只读取常规文件，不会修改来源。符号链接不会被跟随。 ");
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            if ui.button("添加来源文件夹…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    match insert_source_root(&mut self.roots, path.clone()) {
                        Ok(true) => {
                            let canonical = fs::canonicalize(path).ok();
                            self.selected_root = self
                                .roots
                                .iter()
                                .position(|root| Some(&root.path) == canonical.as_ref());
                            self.last_scan = None;
                            self.status = "已添加来源目录。".into();
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
                    can_remove && !self.scanning,
                    eframe::egui::Button::new("移除选中目录"),
                )
                .clicked()
            {
                if let Some(index) = self.selected_root.take() {
                    self.roots.remove(index);
                    self.last_scan = None;
                    self.status = "已移除来源目录。".into();
                }
            }
            if ui
                .add_enabled(
                    !self.roots.is_empty() && !self.scanning,
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

        ui.horizontal(|ui| {
            ui.label("新增扩展名");
            ui.add(
                eframe::egui::TextEdit::singleline(&mut self.new_extension).hint_text("例如 XMP"),
            );
            if ui.button("添加规则").clicked() {
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
                        if ui.button("选择目标文件夹…").clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                match fs::canonicalize(&path) {
                                    Ok(path) => route.destination = Some(path),
                                    Err(error) => {
                                        self.status =
                                            format!("无法解析目标目录 {}：{error}", path.display());
                                    }
                                }
                            }
                        }
                        if !required_extensions.contains(&route.extension)
                            && ui.small_button("移除").clicked()
                        {
                            selected_route = Some(index);
                        }
                    });
                }
            });
        if let Some(index) = selected_route {
            self.routes.remove(index);
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
    }

    fn copy_stage(&mut self, ui: &mut eframe::egui::Ui) {
        ui.label("复制只写入所选目标目录，不修改来源；目标已有同名文件时会报错且不会覆盖。");
        ui.checkbox(
            &mut self.include_system_files,
            "包括系统垃圾文件（.DS_Store、._*、Thumbs.db、desktop.ini 等）",
        );
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
                    self.start_copy(plan.items.clone(), false);
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
                self.start_copy(retry_items.clone(), true);
            }
            if self.work_running
                && self.work_kind == Some(WorkKind::Copy)
                && ui.button("取消（完成当前文件后停止）").clicked()
            {
                if let Some(cancel) = &self.work_cancel {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        });
        if !self.copy_results.is_empty() {
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
            if ui.button("选择来源文件夹…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.direct_verify_source = fs::canonicalize(&path).ok();
                }
            }
            if ui.button("选择目标文件夹…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.direct_verify_target = fs::canonicalize(&path).ok();
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
                if let Some(cancel) = &self.work_cancel {
                    cancel.store(true, Ordering::Relaxed);
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
                if let Some(cancel) = &self.work_cancel {
                    cancel.store(true, Ordering::Relaxed);
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
            if ui.button("选择待检查目录…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    match fs::canonicalize(&path) {
                        Ok(path) => {
                            self.duplicate_directory = Some(path);
                            self.duplicate_report = None;
                            self.excluded_final_paths.clear();
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
                && let Some(cancel) = &self.work_cancel
            {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        if let Some(path) = &self.duplicate_directory {
            ui.weak(format!("所选目录：{}", path.display()));
        }
        ui.horizontal(|ui| {
            ui.add(
                eframe::egui::Slider::new(
                    &mut self.duplicate_options.time_threshold_seconds,
                    0..=60,
                )
                .text("拍摄时间窗口（秒）"),
            );
            ui.add(
                eframe::egui::Slider::new(&mut self.duplicate_options.phash_threshold, 0..=32)
                    .text("pHash 相似阈值（汉明距离）"),
            );
        });
        ui.weak("时区未知的拍摄时间不会参与跨文件比较。视频仍参与 SHA-256 和时间候选，但不计算静态照片 pHash。当前离线解码器仅支持 JPEG/PNG，其余格式会显示原因。");
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
        if ui.button("清除本次排除标记（全部保留）").clicked() {
            self.excluded_final_paths.clear();
        }

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
                                    .checkbox(
                                        &mut exclude,
                                        format!("排除本次最终清单：{}", path.display()),
                                    )
                                    .changed()
                                {
                                    if exclude {
                                        self.excluded_final_paths.insert(path.clone());
                                    } else {
                                        self.excluded_final_paths.remove(path);
                                    }
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
                                        format!(" · pHash 汉明距离 {distance}/63 · {class}")
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
                                .checkbox(
                                    &mut exclude,
                                    format!("排除本次最终清单：{}", candidate.second.display()),
                                )
                                .changed()
                            {
                                if exclude {
                                    self.excluded_final_paths.insert(candidate.second.clone());
                                } else {
                                    self.excluded_final_paths.remove(&candidate.second);
                                }
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
    }

    fn backup_stage(&mut self, ui: &mut eframe::egui::Ui) {
        ui.label("每行将一个本地整理文件夹映射到一个或多个独立备份目标。目标是最终文件夹本身；复制后会重新读取源与每个目标文件并逐项比较 SHA-256。此阶段可单独使用。");
        if !self.excluded_final_paths.is_empty() {
            ui.weak(format!(
                "本次会话携带 {} 个阶段⑥排除标记；路径完全匹配的文件将从备份清单排除。",
                self.excluded_final_paths.len()
            ));
        }
        if ui
            .add_enabled(
                !self.work_running,
                eframe::egui::Button::new("添加本地整理文件夹…"),
            )
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                match fs::canonicalize(&path) {
                    Ok(path) => self.backup_mappings.push(BackupMapping {
                        source: Some(path),
                        targets: Vec::new(),
                    }),
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
                    if ui.button("移除此行").clicked() {
                        remove_mapping = Some(index);
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if ui.button("选择/更改来源…").clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_folder()
                    {
                        match fs::canonicalize(&path) {
                            Ok(path) => mapping.source = Some(path),
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
                        if ui.button("移除目标").clicked() {
                            remove_target = Some(target_index);
                        }
                    });
                }
                if ui.button("+ 添加备份目标文件夹…").clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_folder()
                {
                    match fs::canonicalize(&path) {
                        Ok(path) => {
                            if !mapping.targets.contains(&path) {
                                mapping.targets.push(path);
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
            }
            self.backup_mappings[index] = mapping;
        }
        if let Some(index) = remove_mapping {
            self.backup_mappings.remove(index);
        }

        let ready = self
            .backup_mappings
            .iter()
            .any(|mapping| mapping.source.is_some() && !mapping.targets.is_empty());
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.work_running && ready,
                    eframe::egui::Button::new("开始 / 重试复制并校验"),
                )
                .clicked()
            {
                self.start_backup(false);
            }
            if ui
                .add_enabled(
                    !self.work_running && ready,
                    eframe::egui::Button::new("重新检查现有目标"),
                )
                .clicked()
            {
                self.start_backup(true);
            }
            if self.work_running
                && self.work_kind == Some(WorkKind::Backup)
                && ui.button("取消备份").clicked()
                && let Some(cancel) = &self.work_cancel
            {
                cancel.store(true, Ordering::Relaxed);
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

    fn start_backup(&mut self, verify_only: bool) {
        if self.work_running {
            return;
        }
        let mappings = self.backup_mappings.clone();
        let invalid = backup_mapping_errors(&mappings);
        if !invalid.is_empty() {
            self.status = invalid.join("；");
            return;
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
        } else {
            "正在扫描本地整理目录并准备独立备份目标…".into()
        };
        let excluded = self.excluded_final_paths.clone();
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.work_receiver = Some(receiver);
        self.work_cancel = Some(cancel);
        thread::spawn(move || {
            let mut items = Vec::<RoutedFile>::new();
            let mut error = None;
            let mut excluded_count = 0usize;
            for mapping in mappings {
                if worker_cancel.load(Ordering::Relaxed) {
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
                let summary =
                    match scan_sources(std::slice::from_ref(&source), &worker_cancel, |_| {}) {
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
                    error = Some(format!(
                        "扫描 {} 时有 {} 项读取错误（例如 {}）",
                        source.display(),
                        summary.error_count,
                        summary
                            .error_samples
                            .first()
                            .map(String::as_str)
                            .unwrap_or("未提供")
                    ));
                }
                for file in summary.files {
                    if excluded.contains(&file.path) {
                        excluded_count += mapping.targets.len();
                        continue;
                    }
                    for target in &mapping.targets {
                        items.push(RoutedFile {
                            source: file.path.clone(),
                            destination: target.join(&file.relative_path),
                            expected_source: FileStamp {
                                size: file.size,
                                modified: file.modified,
                            },
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
                        None => "没有可备份文件；请检查来源目录和阶段⑥排除标记。".into(),
                    }),
                    verify_only,
                });
                return;
            }
            if excluded_count > 0 {
                error = Some(match error {
                    Some(error) => {
                        format!("{error}；按阶段⑥决定排除了 {excluded_count} 个目标文件项。")
                    }
                    None => format!("按阶段⑥决定排除了 {excluded_count} 个目标文件项。"),
                });
            }
            let item_count = items.len();
            if verify_only {
                let pairs = items
                    .into_iter()
                    .map(|item| (item.source, item.destination))
                    .collect::<Vec<_>>();
                let (verification, cancelled) =
                    pipeline::verify_batch(pairs, &worker_cancel, |completed, total| {
                        let _ = sender.send(WorkMessage::BackupProgress {
                            completed,
                            total,
                            bytes: 0,
                        });
                    });
                let _ = sender.send(WorkMessage::BackupComplete {
                    copies: Vec::new(),
                    verification,
                    cancelled,
                    error,
                    verify_only,
                });
                return;
            }
            let (copies, cancelled_copy) =
                pipeline::copy_batch(items, &worker_cancel, |completed, total, bytes| {
                    let _ = sender.send(WorkMessage::BackupProgress {
                        completed,
                        total: total.saturating_mul(2),
                        bytes,
                    });
                });
            let pairs = copies
                .iter()
                .filter(|item| item.result.is_ok())
                .map(|item| (item.source.clone(), item.destination.clone()))
                .collect::<Vec<_>>();
            let verify_total = item_count.saturating_mul(2);
            let (verification, cancelled_verify) =
                pipeline::verify_batch(pairs, &worker_cancel, |completed, _| {
                    let _ = sender.send(WorkMessage::BackupProgress {
                        completed: item_count.saturating_add(completed),
                        total: verify_total,
                        bytes: 0,
                    });
                });
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
        self.excluded_final_paths.clear();
        self.work_completed = 0;
        self.work_total = 0;
        self.work_bytes = 0;
        self.work_kind = Some(WorkKind::Duplicates);
        self.work_running = true;
        self.status = "正在重新扫描目录并读取拍摄元数据…".into();
        let threshold = self.duplicate_options.time_threshold_seconds;
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.work_receiver = Some(receiver);
        self.work_cancel = Some(cancel);
        thread::spawn(move || {
            let summary = match scan_sources(&[root], &worker_cancel, |progress| {
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
            if worker_cancel.load(Ordering::Relaxed) {
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
                match metadata::inspect_files(&paths, &worker_cancel, |completed, total| {
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
                duplicates::analyze(&metadata, threshold, &worker_cancel, |completed, total| {
                    let _ = sender.send(WorkMessage::DuplicateProgress { completed, total });
                });
            let _ = sender.send(WorkMessage::DuplicateComplete {
                report,
                cancelled,
                error,
            });
        });
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
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.work_receiver = Some(receiver);
        self.work_cancel = Some(cancel);
        thread::spawn(move || {
            let (paths, mut scan_error) = match input {
                MetadataInput::Files(paths) => (paths, None),
                MetadataInput::Directory(root) => {
                    let scan = scan_sources(&[root], &worker_cancel, |progress| {
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
            let inspected = metadata::inspect_files(&paths, &worker_cancel, |completed, total| {
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
            });
        }
        plan
    }

    fn start_copy(&mut self, items: Vec<RoutedFile>, preserve_existing: bool) {
        if items.is_empty() || self.work_running {
            return;
        }
        if !preserve_existing {
            self.copy_results.clear();
            self.verify_results.clear();
            self.verify_coverage = None;
            self.copy_planned = items.len();
        }
        self.work_completed = 0;
        self.work_total = items.len();
        self.work_bytes = 0;
        self.work_kind = Some(WorkKind::Copy);
        self.work_running = true;
        self.status = format!("正在安全复制 {} 个文件…", items.len());
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.work_receiver = Some(receiver);
        self.work_cancel = Some(cancel);
        thread::spawn(move || {
            let (results, cancelled) =
                pipeline::copy_batch(items, &worker_cancel, |completed, total, bytes| {
                    if completed % 32 == 0 || completed == total {
                        let _ = sender.send(WorkMessage::CopyProgress {
                            completed,
                            total,
                            bytes,
                        });
                    }
                });
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
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.work_receiver = Some(receiver);
        self.work_cancel = Some(cancel);
        thread::spawn(move || {
            let (results, cancelled) =
                pipeline::verify_batch(pairs, &worker_cancel, |completed, total| {
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
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.work_receiver = Some(receiver);
        self.work_cancel = Some(cancel);
        thread::spawn(move || {
            let source = scan_sources(&[source_root.clone()], &worker_cancel, |_| {});
            let target = scan_sources(&[target_root.clone()], &worker_cancel, |_| {});
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
                pipeline::verify_batch(pairs, &worker_cancel, |completed, total| {
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
                    self.work_cancel = None;
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
                    let passed = self
                        .copy_results
                        .iter()
                        .filter(|item| item.result.is_ok())
                        .count();
                    let failed = self.copy_results.len() - passed;
                    if cancelled {
                        self.status = format!(
                            "复制已停止：完成 {passed}，失败 {failed}；其余项目未开始。可重新运行未落盘的项目。 "
                        );
                        self.apply_pending_stage();
                    } else {
                        self.status = format!(
                            "复制阶段结束：成功 {passed}，失败 {failed}。来源文件未修改。 "
                        );
                        self.handoff_title = "分类复制已完成".into();
                        self.handoff_description = format!(
                            "本阶段产生 {} 项复制结果，成功 {} 项、失败 {} 项。目标路径和逐项结果留在本次会话内存中；下一阶段会重新读取源和目标文件计算 SHA-256。",
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
                    self.work_cancel = None;
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
                    self.work_cancel = None;
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
                    self.work_cancel = None;
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
                            "本轮重新扫描并检查了目录。发现 {} 个字节完全相同组、{} 个时间候选、{} 个 SHA-256 读取错误。当前人工排除标记共 {} 项；只影响本次会话，不会删除磁盘文件。{}",
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
                    self.work_cancel = None;
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
                            "备份已取消：复制成功 {copied} 项、失败 {copy_failed} 项；读回校验 PASS {verified} 项、FAIL {verify_failed} 项。未完成项需重新扫描。{warning}"
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
                            "各所选备份目标分别执行了复制和逐文件 SHA-256 读回验证：复制成功 {copied} 项、失败 {copy_failed} 项；校验 PASS {verified} 项、FAIL {verify_failed} 项。结果按目标路径保留在本次会话内存中。{warning}"
                        );
                        self.show_handoff = !self.apply_pending_stage();
                    }
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.work_running = false;
                    self.work_kind = None;
                    self.work_receiver = None;
                    self.work_cancel = None;
                    self.status = "后台处理意外结束；请检查结果并重新运行。".into();
                    break;
                }
            }
        }
    }

    fn incomplete_stage(&self, ui: &mut eframe::egui::Ui) {
        ui.label(
            "此阶段尚未实现。底部导航可进入任意阶段；当前已接入来源扫描、扩展名路由、流式复制和逐项 SHA-256 校验。",
        );
        ui.add_space(8.0);
        ui.label(match self.stage {
            4 => "后续将接入离线 ExifTool，并支持本阶段独立选择目录。",
            5 => "重复扫描、SHA-256、可信时间候选和受支持格式 pHash 已接入；图片预览、HEIC/RAW 全格式解码与 Final Manifest 联动仍未完成。",
            6 => "按来源文件夹配置多个备份目标、独立复制并逐文件 SHA-256 读回校验已接入；断点恢复和暂停尚未实现。",
            7 => "后续将接入加密 7z、文件名加密、分卷和完整性测试。",
            _ => "",
        });
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
        });
        ui.separator();
        ui.horizontal(|ui| {
            if self.scanning {
                ui.spinner();
                ui.label(format!(
                    "阶段任务：已发现 {} 个文件 / {}",
                    self.scan_progress.files,
                    format_bytes(self.scan_progress.bytes)
                ));
            } else if self.work_running {
                ui.spinner();
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
                    None => "准备阶段任务…".into(),
                };
                ui.label(format!("阶段任务：{operation}"));
            } else if self.last_scan.is_some() {
                ui.label("阶段状态：已扫描（来源文件清单保存在当前会话内存中）");
            } else {
                ui.label("阶段状态：未开始");
            }
            ui.with_layout(
                eframe::egui::Layout::right_to_left(eframe::egui::Align::Center),
                |ui| {
                    if ui.button("下一阶段 →").clicked() {
                        self.request_stage((self.stage + 1).min(STAGES.len() - 1));
                    }
                    if ui.button("← 上一阶段").clicked() {
                        self.request_stage(self.stage.saturating_sub(1));
                    }
                    if self.scanning && ui.button("取消扫描").clicked() {
                        if let Some(cancel) = &self.cancel_scan {
                            cancel.store(true, Ordering::Relaxed);
                            self.status = "正在取消扫描…".into();
                        }
                    }
                    if self.work_running && ui.button("取消当前任务").clicked() {
                        if let Some(cancel) = &self.work_cancel {
                            cancel.store(true, Ordering::Relaxed);
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
                    .animate(true)
                    .text("正在枚举文件"),
            );
        }
        let _ = ctx;
    }

    fn start_scan(&mut self) {
        let roots: Vec<PathBuf> = self.roots.iter().map(|root| root.path.clone()).collect();
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.scan_receiver = Some(receiver);
        self.cancel_scan = Some(cancel);
        self.scanning = true;
        self.scan_progress = ScanProgress::default();
        self.last_scan = None;
        self.status = "正在读取目录清单…".into();
        thread::spawn(move || {
            let summary = scan_sources(&roots, &worker_cancel, |progress| {
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
                self.excluded_final_paths.clear();
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

    fn request_stage(&mut self, stage: usize) {
        if stage == self.stage {
            return;
        }
        if self.scanning || self.work_running {
            self.pending_stage = Some(stage);
            self.show_switch_prompt = true;
        } else {
            self.stage = stage;
        }
    }

    fn apply_pending_stage(&mut self) -> bool {
        self.show_switch_prompt = false;
        if let Some(stage) = self.pending_stage.take() {
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
                if let Some(cancel) = &self.cancel_scan {
                    cancel.store(true, Ordering::Relaxed);
                }
                if let Some(cancel) = &self.work_cancel {
                    cancel.store(true, Ordering::Relaxed);
                }
                self.status = "已请求安全停止；当前文件完成后切换阶段。".into();
            }
            _ => {}
        }
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

fn scan_sources(
    roots: &[PathBuf],
    cancel: &AtomicBool,
    mut report: impl FnMut(ScanProgress),
) -> Result<ScanSummary, ScanProgress> {
    let mut summary = ScanSummary::default();
    let mut progress = ScanProgress::default();
    for (source_id, root) in roots.iter().enumerate() {
        let mut pending = vec![root.clone()];
        while let Some(directory) = pending.pop() {
            if cancel.load(Ordering::Relaxed) {
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
                if cancel.load(Ordering::Relaxed) {
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
            Ok(Box::new(PhotoArchiveApp::default()))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

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
    fn scanner_counts_files_and_groups_extensions() {
        let root = temp_dir();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("nested/one.jpg"), b"123").unwrap();
        fs::write(root.join("two.JPG"), b"45").unwrap();
        fs::write(root.join("README"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("nested"), root.join("nested-link")).unwrap();

        let summary = scan_sources(&[root.clone()], &AtomicBool::new(false), |_| {}).unwrap();

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
        let cancel = AtomicBool::new(true);

        assert!(scan_sources(&[root.clone()], &cancel, |_| {}).is_err());

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
        let summary = scan_sources(&[source.clone()], &AtomicBool::new(false), |_| {}).unwrap();
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
                    })
            })
            .collect();
        let (copies, cancelled) =
            pipeline::copy_batch(items, &AtomicBool::new(false), |_, _, _| {});
        assert!(!cancelled);
        assert_eq!(copies.len(), 2);
        assert!(copies.iter().all(|item| item.result.is_ok()));
        let pairs = copies
            .iter()
            .map(|item| (item.source.clone(), item.destination.clone()))
            .collect();
        let (verification, cancelled) =
            pipeline::verify_batch(pairs, &AtomicBool::new(false), |_, _| {});
        assert!(!cancelled);
        assert_eq!(verification.len(), 2);
        assert!(
            verification
                .iter()
                .all(|item| item.result.as_ref().is_ok_and(|result| result.matches))
        );

        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod archive_poc;
