# ExifTool runtime payloads

**English | [简体中文](THIRD_PARTY_NOTICES.zh-CN.md)**

- ExifTool 13.60, Copyright 2003–2026 Phil Harvey.
- Source archive: `Image-ExifTool-13.60.tar.gz` from the [official ExifTool SourceForge project](https://sourceforge.net/projects/exiftool/files/Image-ExifTool-13.60.tar.gz/download).
- Source archive SHA-256: `c8e3c42576f46d862e4cd9c192989776e7363931e890c95056bcd6293e83ea36`.
- macOS arm64 standalone payload: `macos-aarch64/exiftool`; SHA-256: `224c4384d1df807c14f6ebcb04972ab5b918cac076b9dc4cebc99da981ed0b95`. It includes ExifTool and Perl, was rebuilt from the listed ExifTool and PAR::Packer 1.064 sources, and contains no build-machine paths. `otool -L` shows only macOS system frameworks and libraries.
- macOS x86_64 standalone payload: `macos-x86_64/exiftool`; SHA-256: `cbd028efc28cee50fe5418dd63b14797c76039d636746425cd79f53917954ac6`. It packages the ExifTool 13.60 modules from the arm64 payload with x86_64 Perl 5.34.1 and PAR::Packer 1.064; build-machine paths were removed from its string table. `otool -L` shows only macOS system libraries. The executable was started under Rosetta and returned version `13.60`; this is not Intel machine acceptance.
- PAR-Packer 1.064 source package: `PAR-Packer-1.064.tar.gz` from CPAN; SHA-256: `0fc063e448e34f7cb5c69c6be5b2258f7150187b33140dedabd53a2c34ca3cba`.
- Windows x86_64 package: `windows-x86_64/exiftool-13.60_64.zip`; SHA-256: `548b1f4e0b713cd2a26824b34080c35db4e2d2edd166468eb38b1d55ada90c2e`, matching the upstream `checksums-13.60.txt`. The official package includes the ExifTool launcher, Strawberry Perl runtime, dependent DLLs, and its license archive. The extracted launcher hash is `44b336d7596c3ec86577a5ff6d46e8388a56eb69a15f09987d4ac6cc3abbf75e`; runtime startup verifies both hashes and the reported version `13.60`.
- Windows package source: [official ExifTool SourceForge project](https://sourceforge.net/projects/exiftool/files/). Its upstream Windows README attributes the launcher to Oliver Betz and identifies it as CC0-1.0. License texts shipped in the package are included in [`windows-x86_64/THIRD_PARTY_LICENSES.md`](windows-x86_64/THIRD_PARTY_LICENSES.md) and the app's license viewer.
- The official Windows ZIP is kept byte-for-byte. An expanded scan found only generic Unix home-directory placeholders in upstream documentation and test modules; no machine-account or build-directory paths were present.
- ExifTool may be redistributed under the same terms as Perl: the GNU GPL or the Artistic License. This distribution uses the Artistic License option; the license text and ExifTool's upstream README are included alongside this notice.
- PAR::Packer states that its generated executables carry no additional licensing restrictions; its notice is available in the upstream `pp` documentation.

The macOS arm64 payload has been exercised on this host; the macOS x86_64 payload has only had a Rosetta version smoke check, not full metadata sample or Intel machine acceptance. The Windows x86_64 package is embedded and cross-compiled from this macOS host but has not yet been executed on Windows.
