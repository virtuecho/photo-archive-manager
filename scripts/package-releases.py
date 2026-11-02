#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import plistlib
import shutil
import stat
import tomllib
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo

ROOT = Path(__file__).resolve().parents[1]
TARGET = ROOT / "target"
RELEASES = TARGET / "Releases"
BINARY_NAME = "photo-archive-manager"

with (ROOT / "Cargo.toml").open("rb") as package_file:
    version = tomllib.load(package_file)["package"]["version"]

inputs = [
    {
        "slug": "macOS-AppleSilicon-arm64",
        "target": "macOS Apple Silicon (arm64)",
        "binary": TARGET / "aarch64-apple-darwin/release/photo-archive-manager",
        "bundle": True,
        "note_en": "Development build; not signed or notarized. Clean-system installation and full-flow acceptance are incomplete.",
        "note_zh": "开发构建，尚未签名或公证；尚未完成干净系统安装和全流程验收。",
    },
    {
        "slug": "macOS-Intel-x86_64",
        "target": "macOS Intel (x86_64)",
        "binary": TARGET / "x86_64-apple-darwin/release/photo-archive-manager",
        "bundle": True,
        "note_en": "Development build; not signed or notarized. Intel Mac and clean-system full-flow acceptance are incomplete.",
        "note_zh": "开发构建，尚未签名或公证；尚未完成 Intel Mac 和干净系统全流程验收。",
    },
    {
        "slug": "Windows-x86_64",
        "target": "Windows (x86_64)",
        "binary": TARGET / "x86_64-pc-windows-gnu/release/photo-archive-manager.exe",
        "bundle": False,
        "note_en": "Development build cross-compiled on macOS; it has not yet completed full-flow acceptance on Windows.",
        "note_zh": "开发构建，由 macOS 交叉编译；尚未在 Windows 上完成全流程验收。",
    },
]

release_files = [
    (ROOT / "README.md", "README.md"),
    (ROOT / "README.zh-CN.md", "README.zh-CN.md"),
    (ROOT / "LICENSE", "LICENSE"),
    (ROOT / "resources/THIRD_PARTY_NOTICES.md", "resources/THIRD_PARTY_NOTICES.md"),
    (ROOT / "resources/THIRD_PARTY_NOTICES.zh-CN.md", "resources/THIRD_PARTY_NOTICES.zh-CN.md"),
    (ROOT / "resources/licenses/RUST_DEPENDENCIES.md", "resources/licenses/RUST_DEPENDENCIES.md"),
    (ROOT / "resources/exiftool/THIRD_PARTY_NOTICES.md", "resources/exiftool/THIRD_PARTY_NOTICES.md"),
    (ROOT / "resources/exiftool/THIRD_PARTY_NOTICES.zh-CN.md", "resources/exiftool/THIRD_PARTY_NOTICES.zh-CN.md"),
    (ROOT / "resources/exiftool/Artistic-License-1.0.pod", "resources/exiftool/Artistic-License-1.0.pod"),
    (ROOT / "resources/exiftool/ExifTool-13.60-README", "resources/exiftool/ExifTool-13.60-README"),
    (ROOT / "docs/technical-design.md", "docs/technical-design.md"),
    (ROOT / "docs/technical-design.zh-CN.md", "docs/technical-design.zh-CN.md"),
    (ROOT / "docs/implementation-status.md", "docs/implementation-status.md"),
    (ROOT / "docs/implementation-status.zh-CN.md", "docs/implementation-status.zh-CN.md"),
    (ROOT / "docs/release.md", "docs/release.md"),
    (ROOT / "docs/release.zh-CN.md", "docs/release.zh-CN.md"),
    (ROOT / "docs/acceptance-checklist.md", "docs/acceptance-checklist.md"),
    (ROOT / "docs/acceptance-checklist.zh-CN.md", "docs/acceptance-checklist.zh-CN.md"),
]

if RELEASES.exists():
    shutil.rmtree(RELEASES)
RELEASES.mkdir(parents=True)


def write_file(path: Path, data: bytes, mode: int = 0o644) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    path.chmod(mode)


def copy_file(source: Path, destination: Path) -> None:
    if not source.is_file():
        raise SystemExit(f"Required release input is missing: {source.name}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)


def package_info(item: dict[str, object]) -> bytes:
    return (
        f"Photo Archive Manager {version}\n"
        f"Platform: {item['target']}\n\n"
        f"Status: {item['note_en']}\n\n"
        f"Photo Archive Manager {version}\n"
        f"平台：{item['target']}\n\n"
        f"状态：{item['note_zh']}\n"
    ).encode("utf-8")


def build_app(bundle: Path, binary: Path, item: dict[str, object]) -> None:
    contents = bundle / "Contents"
    executable = contents / "MacOS" / BINARY_NAME
    copy_file(binary, executable)
    executable.chmod(0o755)
    plist = {
        "CFBundleDevelopmentRegion": "en",
        "CFBundleExecutable": BINARY_NAME,
        "CFBundleIdentifier": "com.photoarchivemanager.desktop",
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleName": "Photo Archive Manager",
        "CFBundleDisplayName": "Photo Archive Manager",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": version,
        "CFBundleLocalizations": ["en", "zh-Hans"],
        "LSApplicationCategoryType": "public.app-category.photography",
        "NSHighResolutionCapable": True,
        "NSPrincipalClass": "NSApplication",
    }
    write_file(contents / "Info.plist", plistlib.dumps(plist, fmt=plistlib.FMT_XML))
    write_file(contents / "PkgInfo", b"APPL????")
    resources = contents / "Resources" / "Documentation"
    for source, name in release_files:
        copy_file(source, resources / name)
    write_file(resources / "PACKAGE-INFO.txt", package_info(item))


def make_zip(folder: Path, destination: Path) -> None:
    with ZipFile(destination, "w", compression=ZIP_DEFLATED, compresslevel=9) as archive:
        for path in sorted(folder.rglob("*"), key=lambda item: item.as_posix()):
            relative = path.relative_to(folder.parent).as_posix()
            info = ZipInfo(relative + ("/" if path.is_dir() else ""), date_time=(2020, 1, 1, 0, 0, 0))
            info.create_system = 3
            mode = path.stat().st_mode & 0o777
            file_type = stat.S_IFDIR if path.is_dir() else stat.S_IFREG
            info.external_attr = (file_type | mode) << 16
            if path.is_dir():
                archive.writestr(info, b"")
            else:
                archive.writestr(info, path.read_bytes())


checksums: list[str] = []
for item in inputs:
    binary = item["binary"]
    if not binary.is_file():
        raise SystemExit(f"Build this target first; missing {binary.relative_to(ROOT)}")
    if any(not source.is_file() for source, _ in release_files):
        missing = [source.name for source, _ in release_files if not source.is_file()]
        raise SystemExit("Required release documentation is missing: " + ", ".join(missing))
    package_name = f"PhotoArchiveManager-{version}-{item['slug']}"
    stage = RELEASES / package_name
    stage.mkdir()
    if item["bundle"]:
        bundle = stage / "Photo Archive Manager.app"
        build_app(bundle, binary, item)
        # Keep a quick-start and notices beside the app in the archive as well.
        for source, name in release_files:
            copy_file(source, stage / name)
    else:
        executable_name = f"{BINARY_NAME}.exe"
        copy_file(binary, stage / executable_name)
        for source, name in release_files:
            copy_file(source, stage / name)
        windows_licenses = ROOT / "resources/exiftool/windows-x86_64/THIRD_PARTY_LICENSES.md"
        copy_file(windows_licenses, stage / "THIRD_PARTY_LICENSES.md")
        copy_file(windows_licenses, stage / "resources/exiftool/windows-x86_64/THIRD_PARTY_LICENSES.md")
    write_file(stage / "PACKAGE-INFO.txt", package_info(item))
    archive_path = RELEASES / f"{package_name}.zip"
    make_zip(stage, archive_path)
    digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
    checksums.append(f"{digest}  {archive_path.name}")

(RELEASES / "SHA256SUMS.txt").write_text("\n".join(checksums) + "\n", encoding="ascii")
print(f"Release bundles written to {RELEASES}")
for checksum in checksums:
    print(checksum)
