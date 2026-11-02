# Third-party software and licenses

**English | [简体中文](THIRD_PARTY_NOTICES.zh-CN.md)**

- ExifTool 13.60 is included for macOS arm64, macOS x86_64, and Windows x86_64. The macOS standalone payloads are documented in [`exiftool/THIRD_PARTY_NOTICES.md`](exiftool/THIRD_PARTY_NOTICES.md) and use the Artistic License 1.0 option. The official Windows package includes ExifTool, Strawberry Perl, its DLL dependencies, and package licenses; its SHA-256 and upstream source are documented in the same notice. The application license viewer includes the Windows package's license texts in [`exiftool/windows-x86_64/THIRD_PARTY_LICENSES.md`](exiftool/windows-x86_64/THIRD_PARTY_LICENSES.md).
- `sevenz-rust2` 0.24.2 creates and reads encrypted Store 7z archives. It is licensed under Apache-2.0. The application has not bundled the official 7-Zip executable.
- `zeroize` 1.9.1 clears application-owned password strings after archive tasks. It is licensed under MIT OR Apache-2.0.

[`licenses/RUST_DEPENDENCIES.md`](licenses/RUST_DEPENDENCIES.md) contains full license texts and package attribution for the locked Rust dependency graph, including target-specific packages, bundled NOTICE/copyright files, and the application's AGPL-3.0-only license. It is embedded in the application's license viewer. Regenerate it with `python3 scripts/generate-license-report.py` using cargo-about 0.9.3 and the locked crate sources. The generator configuration selects one permitted license for `OR` expressions and retains every required term for `AND` expressions. The report includes Rust packages; ExifTool's embedded runtime is documented separately above. Upstream license texts are included in their original wording; this overview does not replace them.
