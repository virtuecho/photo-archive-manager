# Third-party software and licenses

- ExifTool 13.60 is included for macOS arm64. Its standalone payload and source details are listed in [`exiftool/THIRD_PARTY_NOTICES.md`](exiftool/THIRD_PARTY_NOTICES.md); the bundled copy uses the Artistic License 1.0 option. The license text is in [`exiftool/Artistic-License-1.0.pod`](exiftool/Artistic-License-1.0.pod).
- `sevenz-rust2` 0.24.2 creates and reads encrypted Store 7z archives. It is licensed under Apache-2.0; see [`licenses/Apache-2.0-sevenz-rust2.txt`](licenses/Apache-2.0-sevenz-rust2.txt). The application has not bundled the official 7-Zip executable.
- `zeroize` 1.9.1 clears application-owned password strings after archive tasks. It is licensed under MIT OR Apache-2.0; see [`licenses/MIT-zeroize.txt`](licenses/MIT-zeroize.txt) and [`licenses/Apache-2.0-zeroize.txt`](licenses/Apache-2.0-zeroize.txt).

The remaining Rust dependencies are listed in `Cargo.lock`; their license texts are not yet aggregated into this notice. The application has not completed a full dependency license audit.
