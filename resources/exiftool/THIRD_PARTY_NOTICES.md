# ExifTool runtime payloads

- ExifTool 13.60, Copyright 2003–2026 Phil Harvey.
- Source archive: `Image-ExifTool-13.60.tar.gz` from the [official ExifTool SourceForge project](https://sourceforge.net/projects/exiftool/files/Image-ExifTool-13.60.tar.gz/download).
- Source archive SHA-256: `c8e3c42576f46d862e4cd9c192989776e7363931e890c95056bcd6293e83ea36`.
- macOS arm64 standalone payload: `macos-aarch64/exiftool`; SHA-256: `224c4384d1df807c14f6ebcb04972ab5b918cac076b9dc4cebc99da981ed0b95`. It includes ExifTool and Perl, and was built with PAR::Packer 1.064. `otool -L` shows only macOS system frameworks and libraries.
- Windows x86_64 package: `windows-x86_64/exiftool-13.60_64.zip`; SHA-256: `548b1f4e0b713cd2a26824b34080c35db4e2d2edd166468eb38b1d55ada90c2e`, matching the upstream `checksums-13.60.txt`. The official package includes the ExifTool launcher, Strawberry Perl runtime, dependent DLLs, and its license archive. The extracted launcher hash is `44b336d7596c3ec86577a5ff6d46e8388a56eb69a15f09987d4ac6cc3abbf75e`; runtime startup verifies both hashes and the reported version `13.60`.
- Windows package source: [official ExifTool SourceForge project](https://sourceforge.net/projects/exiftool/files/). Its upstream Windows README attributes the launcher to Oliver Betz and identifies it as CC0-1.0. License texts shipped in the package are included in [`windows-x86_64/THIRD_PARTY_LICENSES.md`](windows-x86_64/THIRD_PARTY_LICENSES.md) and the app's license viewer.
- ExifTool may be redistributed under the same terms as Perl: the GNU GPL or the Artistic License. This distribution uses the Artistic License option; the license text and ExifTool's upstream README are included alongside this notice.
- PAR::Packer states that its generated executables carry no additional licensing restrictions; its notice is available in the upstream `pp` documentation.

The macOS payload has only been built and exercised on macOS arm64. The Windows x86_64 package is embedded and cross-compiled from this macOS host but has not yet been executed on Windows. No macOS Intel payload is available.
