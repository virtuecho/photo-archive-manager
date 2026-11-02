# Build and release guide

**English | [简体中文](release.zh-CN.md)**

This guide explains how to rebuild the current platform packages and where to find them. Builds are development packages until each platform passes the clean-machine checks below.

## One-command build

Run this command on an Apple Silicon Mac with Rust targets for macOS arm64, macOS Intel, and Windows x86_64 installed, plus MinGW-w64 and Python 3.11 or later:

```sh
./scripts/build-releases.sh
```

The script runs the platform build entries in sequence. They check formatting, run the locked Rust test suite, and build optimized binaries. It then assembles the app bundle and ZIP files. The macOS arm64 entry requires an Apple Silicon macOS host; Windows is cross-compiled and must still be checked on Windows.

## Stable output locations

Build binaries are written to these Cargo target paths:

| Platform | Binary path |
| --- | --- |
| macOS Apple Silicon | `target/aarch64-apple-darwin/release/photo-archive-manager` |
| macOS Intel | `target/x86_64-apple-darwin/release/photo-archive-manager` |
| Windows x86_64 | `target/x86_64-pc-windows-gnu/release/photo-archive-manager.exe` |

All distributable ZIP files and their SHA-256 list are written to the same directory: `target/Releases/`. The ZIP filenames include the app version, platform, and architecture:

```text
PhotoArchiveManager-0.1.0-macOS-AppleSilicon-arm64.zip
PhotoArchiveManager-0.1.0-macOS-Intel-x86_64.zip
PhotoArchiveManager-0.1.0-Windows-x86_64.zip
SHA256SUMS.txt
```

The version is read from `Cargo.toml`, so filenames follow the version in the source. Running the packaging step replaces the generated `target/Releases/` directory. ZIP entries are sorted and use fixed timestamps for stable packaging metadata; this does not by itself promise byte-for-byte identical compiled binaries across toolchains.

## What's inside

- Each macOS ZIP contains `Photo Archive Manager.app`, with its executable under `Contents/MacOS/`, an `Info.plist`, and the bilingual user and technical documents under `Contents/Resources/Documentation/`.
- The Windows ZIP contains the x86_64 GUI executable, the bilingual documents, and the Windows ExifTool/Perl license texts.
- Each package contains a bilingual `PACKAGE-INFO.txt` with its platform and current acceptance status. The root `SHA256SUMS.txt` lists each ZIP digest.

The `.app` bundle is not signed or notarized. Gatekeeper behavior, clean-system startup, full workflows, and distribution acceptance have not been completed. The Windows executable is cross-compiled on macOS and has not yet completed Windows-machine acceptance. Treat these as development builds.

## Platform build commands

To rebuild one target separately, use:

```sh
./scripts/build-macos-arm64.sh
./scripts/build-macos-x86_64.sh
./scripts/build-windows-x86_64.sh
```

Run `python3 scripts/package-releases.py` after the target binaries exist to regenerate all platform ZIPs. This command expects all three binaries and the release documents.

The macOS ExifTool payloads are checked against pinned SHA-256 values. Build flags remap the repository and Cargo cache locations before compiling. Windows checks the bundled upstream ExifTool ZIP and inspects the resulting PE GUI subsystem and runtime DLL imports. The app uses its in-process Rust 7z implementation; ExifTool, Perl, and 7-Zip do not need to be installed on `PATH` for the supported bundled workflows.

## Current validation limits

- macOS arm64: the native window and file picker have been exercised on the build host. Full offline acceptance on a clean Mac is still outstanding.
- macOS Intel: the app and ExifTool payload have a Rosetta smoke check, but there is no Intel Mac or clean-machine full-flow acceptance yet.
- Windows x86_64: the GUI executable and embedded ExifTool package are cross-built and statically checked; Windows runtime and offline acceptance are outstanding.
- Real-device HEIC/HEIF samples, signing, notarization, Finder launch, and Gatekeeper distribution checks remain open.
- The archive writer first creates a complete temporary 7z stream and then splits it into volumes. Temporary disk use can approach twice the final archive size; isolated restore verification needs additional space for a restored copy.

See the [clean-machine checklist](acceptance-checklist.md) before calling a package ready to distribute.
