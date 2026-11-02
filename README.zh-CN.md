# Photo Archive Manager

**[English](README.md) | 简体中文**

Photo Archive Manager 是一款离线桌面应用，可整理、检查、备份照片并制作加密归档。整理和复制时，原始文件会保留在原处，供你检查处理结果。

## 使用流程

1. 选择照片文件夹并查看扫描结果。
2. 整理文件、检查元数据或重复项，并制作经过校验的备份。
3. 准备长期保存时，创建加密归档。

程序只在本机处理文件，不会上传照片。报告成功前会检查复制和归档内容，也不会覆盖已有目标文件。恢复记录不会保存归档密码。

首次启动时，界面默认使用英语。在程序顶部选择 **English** 或 **中文** 即可立即切换；语言选择会保存，并在下次启动时继续使用。

## 获取构建产物

在 Apple Silicon Mac 上运行 `./scripts/build-releases.sh`，即可构建可用的平台目标，并将带版本号的 ZIP 写入 `target/Releases/`。macOS ZIP 内含 `.app` 应用包。每个 ZIP 文件名都会标明平台和 CPU 架构。

请查看[构建与发布说明](docs/release.zh-CN.md)，了解前置条件、完整文件名、产物位置和当前验证范围。

## 更多信息

- [技术设计与流程图](docs/technical-design.zh-CN.md)
- [实施状态](docs/implementation-status.zh-CN.md)
- [构建与发布说明](docs/release.zh-CN.md)
- [干净设备验收清单](docs/acceptance-checklist.zh-CN.md)
- [第三方组件与许可](resources/THIRD_PARTY_NOTICES.zh-CN.md)
