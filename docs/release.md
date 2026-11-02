# 本机构建与发布状态

## 当前可复现构建

macOS arm64 构建脚本要求本机 Rust 工具链，并用锁定的 Cargo 依赖运行格式检查、默认测试及 Release 构建：

```sh
./scripts/build-macos-arm64.sh
```

输出文件为 `target/aarch64-apple-darwin/release/photo-archive-manager`。它是一个 Mach-O 可执行文件，包含本机 macOS arm64 的 ExifTool 有效载荷；归档功能使用进程内 Rust 7z 引擎。运行程序不需要从 `PATH` 安装或查找 ExifTool、7-Zip、Perl 或 Python。

Windows x86_64 可在本机安装 Rust GNU target 与 MinGW-w64 后运行：

```sh
./scripts/build-windows-x86_64.sh
```

输出文件为 `target/x86_64-pc-windows-gnu/release/photo-archive-manager.exe`。本机生成的 PE32+ GUI 程序约 21 MB，嵌入官方 ExifTool 13.60 x64 ZIP；启动元数据检查时会在 `%LOCALAPPDATA%/PhotoArchiveManager/runtime` 下校验并释放 ExifTool、Perl 与 DLL，然后清理。构建脚本检查 GUI 子系统以及 PE 导入表中没有 MinGW runtime DLL。该产物仍需在 Windows 上实机启动与离线全流程验证。

超过默认单卷上限的大文件验收需显式运行，因为它会完整流式处理至少 2 GB：

```sh
cargo test --release archive::tests::archives_a_single_file_larger_than_the_default_volume_limit -- --ignored --nocapture
```

## 发布判定

当前产物仍是开发构建，不是已批准的最终发布包。没有在断网干净系统上完成安装依赖为零的启动和全流程验收，也没有签名/公证；本机 GUI 文件选择器、Dock 和 Gatekeeper 的裸 Mach-O 分发行为尚未确认。Windows 产物只在 macOS 上交叉编译，尚无 Windows 实机运行证据；macOS Intel 没有对应 ExifTool 有效载荷或 Release 构建。

因此，本仓库目前不能宣称达到交接方案要求的 Windows/macOS 单文件、自包含、离线最终交付定义。请在获得目标平台有效载荷、干净系统验收和分发验证前，把 `target/` 产物视为开发文件。

## 已知限制

- ExifTool 已有 macOS arm64 与 Windows x86_64 嵌入构建；Windows 包装路径通过上游哈希、Zip 解包、释放 EXE 哈希和版本文本检查，但没有 Windows 实机验证。macOS Intel 需补齐有效载荷并构建。
- 重复检查支持 JPEG/PNG/HEIC/HEIF pHash 与并排预览；HEIF 使用内置纯 Rust Heifer 解码器，并限制为 5000 万像素。当前只用编码生成的 HEIC 样本验证，需补真实手机样本验收；没有 RAW 解码或可导出的 Final Manifest。携带阶段⑥结果后，Final Manifest 的路径、SHA-256 和排除决定会保存到本机私有检查点，启动时恢复；备份/归档会重新扫描并复核哈希。检查点不保存密码、GPS 或旧 PASS 状态。
- 阶段⑤/⑥完整重检、来源重扫、路由/过滤修改、重复制、审核决定或备份映射修改会清除已知依赖结果。应用不实时监视外部文件改动；用户需重新扫描/校验，任务会重新读取文件并核对 SHA-256；阶段⑤复查确认先前异常已有效时会标记 `RESOLVED`。
- 任务支持当前进程内在安全边界暂停/恢复；关闭程序后可恢复阶段③未完成的分类复制计划、阶段⑦备份映射、阶段⑧归档输入和隔离还原选项，以及阶段⑥审核清单。阶段③会重验已有目标；阶段⑦会重新扫描来源并验证已有目标；阶段⑧要求重新输入密码，并重新扫描、从头重建和验证，不保存旧 PASS。归档先写临时单文件再切卷，峰值临时空间可能接近最终归档体积的两倍；严格还原校验还需要一份输入内容大小的临时空间；中断残留不会自动删除，已有输出不会覆盖。多卷最终提交遇到一般错误会回滚，但系统崩溃时多文件名无法作为一个原子事务提交。
- 归档已在本机用官方 7-Zip 26.05 验证小卷互操作及 2,000,000,001 字节单文件的测试、提取和 SHA-256；未在 Windows、macOS Intel 或干净系统上做同等验收。程序提供可选隔离目录还原和磁盘重读 SHA-256 校验，会临时占用至少一份还原数据大小的空间。

发布候选版的双平台断网/干净机器操作步骤见 [`docs/acceptance-checklist.md`](acceptance-checklist.md)。清单中的复选框必须以实际运行记录和证据为准；交叉编译不算目标系统运行验收。
- 程序擦除自己持有的密码字符串；`sevenz-rust2` 内部密码缓冲区不保证主动擦零。
- Rust 锁定依赖图的许可证文本和 attribution 已由 cargo-about 生成并嵌入窗口；Windows ExifTool/Strawberry Perl 发行包随附的 license 文本已提取到窗口和原始 ZIP 中。macOS ExifTool/PAR payload 的完整打包模块 attribution 仍需单独核验。生成说明见 `resources/THIRD_PARTY_NOTICES.md`。

完整阶段现状及证据见 [`implementation-status.md`](implementation-status.md)。
