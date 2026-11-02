# 构建与发布说明

**[English](release.md) | 简体中文**

本文介绍如何重新构建当前平台包以及产物位置。所有平台完成干净设备验收前，这些产物都属于开发构建。

## 一条命令构建

在 Apple Silicon Mac 上运行以下命令。主机需要安装 macOS arm64、macOS Intel 和 Windows x86_64 的 Rust target、MinGW-w64，以及 Python 3.11 或更高版本：

```sh
./scripts/build-releases.sh
```

脚本会依次执行各平台构建入口。入口会检查格式、运行锁定的 Rust 测试套件并构建优化二进制文件，之后创建 `.app` 应用包和 ZIP。macOS arm64 构建入口要求 Apple Silicon macOS 主机；Windows 为交叉编译，仍须在 Windows 上验证。

## 固定产物位置

编译二进制文件会写入以下 Cargo target 路径：

| 平台 | 二进制文件路径 |
| --- | --- |
| macOS Apple Silicon | `target/aarch64-apple-darwin/release/photo-archive-manager` |
| macOS Intel | `target/x86_64-apple-darwin/release/photo-archive-manager` |
| Windows x86_64 | `target/x86_64-pc-windows-gnu/release/photo-archive-manager.exe` |

所有分发 ZIP 和 SHA-256 清单都会写入同一个目录：`target/Releases/`。ZIP 文件名包含应用版本、平台和架构：

```text
PhotoArchiveManager-0.1.0-macOS-AppleSilicon-arm64.zip
PhotoArchiveManager-0.1.0-macOS-Intel-x86_64.zip
PhotoArchiveManager-0.1.0-Windows-x86_64.zip
SHA256SUMS.txt
```

版本号读取自 `Cargo.toml`，因此文件名会随源码版本变化。执行打包步骤会替换生成的 `target/Releases/` 目录。ZIP 条目按排序写入并使用固定时间戳，使打包元数据稳定；但这本身不保证不同工具链编译出的二进制逐字节相同。

## 包内内容

- 每个 macOS ZIP 都包含 `Photo Archive Manager.app`。应用包内含 `Contents/MacOS/` 下的可执行文件、`Info.plist`，以及放在 `Contents/Resources/Documentation/` 下的中英双语用户文档和技术文档。
- Windows ZIP 包含 x86_64 GUI 程序、中英双语文档，以及 Windows ExifTool/Perl 许可证文本。
- 每个包都包含中英双语的 `PACKAGE-INFO.txt`，说明平台和当前验收状态。根目录的 `SHA256SUMS.txt` 列出每个 ZIP 的摘要。

`.app` 尚未签名或公证。Gatekeeper 行为、干净系统启动、完整工作流程和分发验收均未完成。Windows 可执行文件由 macOS 交叉编译，尚未完成 Windows 实机验收。请将这些产物视为开发构建。

## 按平台单独构建

如需单独重新构建某个平台，可使用：

```sh
./scripts/build-macos-arm64.sh
./scripts/build-macos-x86_64.sh
./scripts/build-windows-x86_64.sh
```

三个 target 二进制文件均准备好后，运行 `python3 scripts/package-releases.py` 重新生成全部平台 ZIP。此命令需要三个二进制文件和发布文档都存在。

macOS ExifTool payload 会按固定 SHA-256 校验。编译参数会重映射仓库与 Cargo 缓存位置。Windows 构建会校验上游 ExifTool ZIP，并检查最终 PE 的 GUI 子系统和运行时 DLL 导入。程序使用进程内 Rust 7z 实现；支持的内置工作流程不需要在 `PATH` 安装 ExifTool、Perl 或 7-Zip。

## 当前验证限制

- macOS arm64：已在构建主机上运行原生窗口和文件选择器；干净 Mac 上的完整离线验收仍待完成。
- macOS Intel：应用和 ExifTool payload 已通过 Rosetta 冒烟检查，但尚无 Intel Mac 或干净设备全流程验收。
- Windows x86_64：GUI 程序和内嵌 ExifTool 包已交叉编译并静态检查；Windows 运行和离线验收仍待完成。
- 真实设备 HEIC/HEIF 样本、签名、公证、Finder 启动和 Gatekeeper 分发检查仍待完成。
- 归档 writer 会先创建完整的临时 7z 流，再切分成多个卷。临时磁盘用量可能接近最终归档的两倍；隔离还原校验还需要容纳一份还原内容的额外空间。

在宣称产物可以分发前，请按[干净设备验收清单](acceptance-checklist.zh-CN.md)逐项检查。
