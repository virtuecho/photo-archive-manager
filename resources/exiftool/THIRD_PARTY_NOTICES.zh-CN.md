# ExifTool runtime payloads

**[English](THIRD_PARTY_NOTICES.md) | 简体中文**

本文概述随程序分发的 ExifTool runtime。完整哈希、来源和平台说明见[英文技术记录](THIRD_PARTY_NOTICES.md)；许可全文以其原始文本为准。

| 平台 | 内嵌内容 | 验证状态 |
| --- | --- | --- |
| macOS arm64 | ExifTool 13.60、Perl runtime；使用列出的源码和 PAR::Packer 1.064 重建 | 已在构建主机运行；动态依赖仅为 macOS 系统框架和库 |
| macOS x86_64 | ExifTool 13.60、x86_64 Perl 5.34.1、PAR::Packer 1.064 | 已完成 SHA-256 检查，并在 Apple Silicon 的 Rosetta 下启动；尚未在 Intel 实机验收 |
| Windows x86_64 | 官方 ExifTool ZIP，含 Strawberry Perl、依赖 DLL 和许可证 | 构建时校验上游 ZIP、释放后的 EXE 哈希和版本；尚未在 Windows 实机验收 |

ExifTool 13.60 由 Phil Harvey 版权所有，可按 Perl 同等条款以 GNU GPL 或 Artistic License 重新分发。本项目选择 Artistic License 选项。原始许可文本、ExifTool 上游 README 和 Windows 包许可证均随相关发行内容提供。Windows 启动器由 Oliver Betz 编写，上游说明采用 CC0-1.0。

PAR::Packer 说明其生成的可执行文件不附加额外许可限制。macOS arm64 与 x86_64 payload 所用打包模块的完整归属信息仍需单独核验。Windows 官方 ZIP 保持原样；对其内容的检查只发现上游文档和测试模块中的通用 Unix home 占位路径，没有发现机器账户或构建目录路径。
