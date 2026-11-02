# Photo Archive Manager

离线本地照片整理工具。当前原生八阶段窗口已接通来源扫描、扩展名路由、安全复制、完整性校验，以及 macOS arm64 的内置 ExifTool 元数据检查。

## 本机运行

需要 Rust 工具链：

```sh
cargo run
```

扫描只读取常规文件，不跟随符号链接；扫描结果和清单暂存在当前进程内存中。复制使用有界缓冲区、临时文件、SHA-256 和防覆盖提交。完整性阶段会再次读取源/目标文件；也可单独选择两个目录按相对路径核对。系统垃圾过滤可审核并可恢复包含。

阶段⑤会重新读取所选整理目录或本次复制清单中的文件，显示 GPS、原始拍摄时间、精度、时区状态、来源标签及可用相机信息。ExifTool 13.60 与 Perl runtime 已嵌入本机 macOS arm64 构建，运行时不从 `PATH` 查找工具。Windows 和 macOS Intel 尚无相应有效载荷，阶段⑤会如实禁用。第三方来源、许可和二进制校验值见 [`resources/exiftool/THIRD_PARTY_NOTICES.md`](resources/exiftool/THIRD_PARTY_NOTICES.md)。

`cargo test` 覆盖扫描、元数据状态和日期/GPS解析、内置 ExifTool 启动、来源变更拒绝、复制、同大小目标改写识别、系统垃圾过滤和加密 Store 7z 单卷 PoC。PoC 不代表已支持官方 7-Zip 互操作或多卷归档。

## 当前交付状态

请先读 [`docs/implementation-status.md`](docs/implementation-status.md)。`target/` 下的构建产物只是本机开发产物；尚无经过干净系统离线验收的 Windows/macOS 自包含发布包。
