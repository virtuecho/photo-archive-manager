# 本机构建与发布状态

## 当前可复现构建

目前只提供 macOS arm64 构建脚本；它要求本机已安装 Rust 工具链，并用锁定的 Cargo 依赖运行格式检查、默认测试及 Release 构建：

```sh
./scripts/build-macos-arm64.sh
```

输出文件为 `target/aarch64-apple-darwin/release/photo-archive-manager`。它是一个 Mach-O 可执行文件，包含本机 macOS arm64 的 ExifTool 有效载荷；归档功能使用进程内 Rust 7z 引擎。运行程序不需要从 `PATH` 安装或查找 ExifTool、7-Zip、Perl 或 Python。

超过默认单卷上限的大文件验收需显式运行，因为它会完整流式处理至少 2 GB：

```sh
cargo test --release archive::tests::archives_a_single_file_larger_than_the_default_volume_limit -- --ignored --nocapture
```

## 发布判定

当前产物仍是开发构建，不是已批准的最终发布包。没有在断网干净系统上完成安装依赖为零的启动和全流程验收，也没有签名/公证；本机 GUI 文件选择器、Dock 和 Gatekeeper 的裸 Mach-O 分发行为尚未确认。Windows 与 macOS Intel 没有对应的 ExifTool 有效载荷或 Release 构建。

因此，本仓库目前不能宣称达到交接方案要求的 Windows/macOS 单文件、自包含、离线最终交付定义。请在获得目标平台有效载荷、干净系统验收和分发验证前，把 `target/` 产物视为开发文件。

## 已知限制

- ExifTool 仅有 macOS arm64 嵌入构建；Windows、macOS Intel 需补齐真正自包含的对应运行时。
- 重复检查支持 JPEG/PNG pHash 与并排预览；当前会话携带阶段⑥结果后会生成内存 Final Manifest，备份/归档仅处理其确认路径并复核哈希。没有 HEIC/RAW 解码或持久/导出的 Final Manifest，排除决定只保存在当前会话。
- 文件在阶段间被外部修改时，尚未实现完整的跨阶段自动失效与依赖重检联动。
- 备份没有暂停/断点恢复。归档先写临时单文件再切卷，峰值临时空间可能接近最终归档体积的两倍；多卷最终提交遇到一般错误会回滚，但系统崩溃时多文件名无法作为一个原子事务提交。
- 归档已在本机用官方 7-Zip 26.05 验证小卷互操作及 2,000,000,001 字节单文件的测试、提取和 SHA-256；未在 Windows、macOS Intel 或干净系统上做同等验收。程序内没有“恢复到隔离目录”的单独严格校验模式。
- 程序擦除自己持有的密码字符串；`sevenz-rust2` 内部密码缓冲区不保证主动擦零。
- 第三方声明和部分直接依赖许可证已嵌入窗口；完整传递依赖许可证审计未完成。

完整阶段现状及证据见 [`implementation-status.md`](implementation-status.md)。
