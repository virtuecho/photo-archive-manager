# 第三方组件与许可

**[English](THIRD_PARTY_NOTICES.md) | 简体中文**

本文是第三方组件的中文概述；完整许可文本按原始语言提供，并以各许可原文为准。

- macOS arm64、macOS x86_64 和 Windows x86_64 构建包含 ExifTool 13.60。macOS 独立 payload 使用 Artistic License 1.0 选项。Windows 官方包包含 ExifTool、Strawberry Perl、依赖 DLL 和包内许可证。来源、摘要及各平台详情见 [ExifTool 组件说明](exiftool/THIRD_PARTY_NOTICES.md)。Windows 包内许可证也显示在程序许可查看器中，文本见 [Windows 第三方许可](exiftool/windows-x86_64/THIRD_PARTY_LICENSES.md)。
- `sevenz-rust2` 0.24.2 用于创建和读取加密 Store 7z 归档，采用 Apache-2.0 许可。程序不包含官方 7-Zip 可执行文件。
- `zeroize` 1.9.1 用于归档任务后清理程序自身持有的密码字符串，采用 MIT 或 Apache-2.0 许可。

[Rust 依赖完整许可报告](licenses/RUST_DEPENDENCIES.md)包含锁定依赖图的许可全文和组件归属信息，包括目标平台依赖、NOTICE/版权文件以及本程序的 AGPL-3.0-only 许可。报告会嵌入程序的许可查看器。可使用锁定的 crate 源码和 cargo-about 0.9.3，通过 `python3 scripts/generate-license-report.py` 重新生成。`OR` 表达式会选择一个允许的许可，`AND` 表达式会保留所有必要许可条款。该报告涵盖 Rust 包；内嵌 ExifTool runtime 在上文另行说明。
