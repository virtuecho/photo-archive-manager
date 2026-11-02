# Third-party software and licenses

- ExifTool 13.60 is included for macOS arm64. Its standalone payload and source details are listed in [`exiftool/THIRD_PARTY_NOTICES.md`](exiftool/THIRD_PARTY_NOTICES.md); the bundled copy uses the Artistic License 1.0 option. The license text is in [`exiftool/Artistic-License-1.0.pod`](exiftool/Artistic-License-1.0.pod).
- `sevenz-rust2` 0.24.2 creates and reads encrypted Store 7z archives. It is licensed under Apache-2.0. The application has not bundled the official 7-Zip executable.
- `zeroize` 1.9.1 clears application-owned password strings after archive tasks. It is licensed under MIT OR Apache-2.0.

[`licenses/RUST_DEPENDENCIES.md`](licenses/RUST_DEPENDENCIES.md) contains full license texts and package attribution for the locked Rust dependency graph, including target-specific packages, bundled NOTICE/copyright files, and the application's AGPL-3.0-only license. It is embedded in the application's license viewer. Regenerate it with `python3 scripts/generate-license-report.py` using cargo-about 0.9.3 and the locked crate sources. The generator configuration selects one permitted license for `OR` expressions and retains every required term for `AND` expressions. The report includes Rust packages; ExifTool's embedded runtime is documented separately above.
