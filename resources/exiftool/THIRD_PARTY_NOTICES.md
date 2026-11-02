# ExifTool runtime payload

- ExifTool 13.60, Copyright 2003–2026 Phil Harvey.
- Source archive: `Image-ExifTool-13.60.tar.gz` from the [official ExifTool SourceForge project](https://sourceforge.net/projects/exiftool/files/Image-ExifTool-13.60.tar.gz/download).
- Source archive SHA-256: `c8e3c42576f46d862e4cd9c192989776e7363931e890c95056bcd6293e83ea36`.
- Embedded standalone payload: `macos-aarch64/exiftool`; SHA-256: `224c4384d1df807c14f6ebcb04972ab5b918cac076b9dc4cebc99da981ed0b95`.
- The payload includes ExifTool and its Perl runtime, and was built with PAR::Packer 1.064. `otool -L` shows only `/usr/lib/libSystem.B.dylib` as a non-system-framework dependency.
- ExifTool may be redistributed under the same terms as Perl: the GNU GPL or the Artistic License. This distribution uses the Artistic License option; the license text and ExifTool's upstream README are included alongside this notice.
- PAR::Packer states that its generated executables carry no additional licensing restrictions; its notice is available in the upstream `pp` documentation.

This binary has only been built and exercised on macOS arm64. It is not the Windows or Intel macOS payload.
