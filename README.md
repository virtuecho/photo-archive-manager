# Photo Archive Manager

离线本地照片整理工具。原生八阶段窗口现已接通来源扫描、扩展名路由、安全复制、完整性校验、macOS arm64 内置 ExifTool 检查、重复候选审核、多目标文件备份与加密 7z 分卷归档。

## 本机运行

需要 Rust 工具链：

```sh
cargo run
```

扫描只读取常规文件，不跟随符号链接；扫描结果和清单暂存在当前进程内存中。复制使用有界缓冲区、临时文件、SHA-256 和防覆盖提交。完整性阶段会再次读取源/目标文件；也可单独选择两个目录按相对路径核对。系统垃圾过滤可审核并可恢复包含。

阶段⑤会重新读取所选整理目录或本次复制清单中的文件，显示 GPS、原始拍摄时间、精度、时区状态、来源标签及可用相机信息。ExifTool 13.60 与 Perl runtime 已嵌入本机 macOS arm64 构建，运行时不从 `PATH` 查找工具。Windows 和 macOS Intel 尚无相应有效载荷，阶段⑤会如实禁用。第三方来源、许可和二进制校验值见 [`resources/exiftool/THIRD_PARTY_NOTICES.md`](resources/exiftool/THIRD_PARTY_NOTICES.md)。

阶段⑥每次重新扫描目录并重算文件 SHA-256；带可信时区的拍摄时间按逐对阈值生成候选，不做传递合并；JPEG/PNG 时间候选可计算 64-bit 离线 pHash，并可并排预览且应用 EXIF 方向。候选只能手动标记从本次清单排除，不会删除文件。携带阶段⑥结果会在当前会话生成内存 Final Manifest；阶段⑦/⑧来源与其重叠时，只处理清单内路径并核对 SHA-256。它不会跨重启保存或导出。当前进程内的扫描、复制、校验、元数据、重复检查、备份和归档支持安全边界暂停/恢复；暂停会在当前文件或批次等安全边界生效。退出后断点恢复、HEIC/RAW 预览解码和跨阶段自动失效联动尚未完成。

阶段⑧可独立选择目录，以 GUI 双次输入密码创建 Store、AES-256、加密文件名的 7z 归档；默认每卷 2,000,000,000 字节。程序先写完整临时 7z，再按字节流切成 `.7z.001/.002/...`，逐卷限额，并在提交前重组卷流、核对完整清单及逐文件 SHA-256，最后防覆盖提交。应用内超过 2 GB 单文件测试通过；官方 7-Zip 26.05 将该两卷归档识别为 `Type = Split`，完成 `t` 与提取，解出文件和同尺寸稀疏源文件的 SHA-256 相同。分卷期间临时空间可能需要接近归档体积的两倍。密码输入由程序清空，但底层 7z 库内部密码缓冲区不保证主动擦零。窗口“开源许可”含 Rust 锁定依赖的完整许可清单；ExifTool/Perl/PAR 有效载荷单独列明。

`cargo test` 最近一次为 31 passed、0 failed、3 ignored（两项需外部图片/MOV 样本；一项 2 GB 单文件压力用例需显式运行）。压力用例已用 Release 配置单独通过。覆盖扫描、暂停/恢复及取消交互、JPEG/PNG 预览解码、64-bit pHash、拍摄时间窗口边界、来源变更拒绝、同大小目标改写识别、备份读回与多余文件检查、Final Manifest 过滤与哈希基准拒绝、分卷 7z manifest/SHA-256 校验和缺少卷错误。官方 7-Zip 26.05 已通过小卷与大于 2 GB 单文件归档的 `t`，并从大归档提取后核对 SHA-256。

## 当前交付状态

请先读 [`docs/implementation-status.md`](docs/implementation-status.md) 和 [`docs/release.md`](docs/release.md)。本机 macOS arm64 可用 `scripts/build-macos-arm64.sh` 重建；`target/` 下的构建产物只是开发文件，尚无经过干净系统离线验收的 Windows/macOS 自包含发布包。
