# Photo Archive Manager

离线本地照片整理工具。原生八阶段窗口现已接通来源扫描、扩展名路由、安全复制、完整性校验、macOS arm64 内置 ExifTool 检查、重复候选审核入口，以及多目标文件备份与逐目标读回校验。

## 本机运行

需要 Rust 工具链：

```sh
cargo run
```

扫描只读取常规文件，不跟随符号链接；扫描结果和清单暂存在当前进程内存中。复制使用有界缓冲区、临时文件、SHA-256 和防覆盖提交。完整性阶段会再次读取源/目标文件；也可单独选择两个目录按相对路径核对。系统垃圾过滤可审核并可恢复包含。

阶段⑤会重新读取所选整理目录或本次复制清单中的文件，显示 GPS、原始拍摄时间、精度、时区状态、来源标签及可用相机信息。ExifTool 13.60 与 Perl runtime 已嵌入本机 macOS arm64 构建，运行时不从 `PATH` 查找工具。Windows 和 macOS Intel 尚无相应有效载荷，阶段⑤会如实禁用。第三方来源、许可和二进制校验值见 [`resources/exiftool/THIRD_PARTY_NOTICES.md`](resources/exiftool/THIRD_PARTY_NOTICES.md)。

阶段⑥每次重新扫描目录并重算文件 SHA-256；带可信时区的拍摄时间按逐对阈值生成候选，不做传递合并；JPEG/PNG 时间候选可计算离线 pHash。候选只能手动标记从本次清单排除，不会删除文件。HEIC/RAW 解码、图片预览及完整 Final Manifest 流程尚未完成。阶段⑦支持每个本地文件夹映射到多个备份目标，拒绝路径重叠，并逐文件重新读取源/目标进行 SHA-256 校验。暂停、断点恢复和跨阶段文件变化失效联动尚未完成。

`cargo test` 覆盖扫描、元数据状态和日期/GPS解析、内置 ExifTool 启动、重复 SHA 分组与时间窗口边界、来源变更拒绝、复制、同大小目标改写识别、备份多目标读回校验、路径重叠保护、系统垃圾过滤和加密 Store 7z 单卷 PoC。PoC 不代表已支持官方 7-Zip 互操作或多卷归档。

## 当前交付状态

请先读 [`docs/implementation-status.md`](docs/implementation-status.md)。`target/` 下的构建产物只是本机开发产物；尚无经过干净系统离线验收的 Windows/macOS 自包含发布包。
