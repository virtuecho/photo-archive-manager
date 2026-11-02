# Implementation status

**English | [简体中文](implementation-status.zh-CN.md)**

This page summarizes the implemented workflows and what has actually been verified. Cross-compiling a target does not count as acceptance on that platform.

## Implemented workflows

The app has eight directly accessible stages: scan folders; set extension-based organization rules; copy files; verify copies; inspect metadata; review exact and visual duplicate candidates; back up to multiple targets; and create encrypted 7z archives.

- Scanning runs in the background, can be cancelled, and skips symbolic links and special files. Parent and child folder selections are normalized to avoid scanning the same files twice.
- Organization and backup copies retain relative paths, stream file data, compare SHA-256, and avoid overwriting existing destinations. Checkpoints can restore selected copy plans and backup mappings after restart; resumed tasks reread and verify files.
- Metadata inspection reads GPS and capture-time fields through the bundled ExifTool runtime. It distinguishes missing, partial, invalid, unsupported, and read-error states. A user may acknowledge a warning without changing the field state.
- Duplicate review finds byte-identical files and image candidates for JPEG, PNG, HEIC, and HEIF. Candidate groups can be previewed side by side and excluded. Video does not participate in perceptual-hash comparison.
- Archive creation uses encrypted 7z Store mode, supports split volumes, and checks paths and SHA-256 before committing. Optional isolated restore verification rereads restored files. A resumed archive task requires the password again and rebuilds from the beginning.
- The interface supports US English and Simplified Chinese. English is the first-run default; the visible **English** and **中文** controls switch immediately without restarting and remember the selection.

## Build and package outputs

The platform binaries have stable target-specific paths, and the release packager writes versioned, architecture-labelled ZIPs under `target/Releases/`. macOS ZIPs contain a `Photo Archive Manager.app` bundle. The [release guide](release.md) lists exact names and rebuild commands.

The ZIP packager sorts entries, fixes their timestamps, includes checksums, and records the platform acceptance status. This makes archive metadata predictable; compiled binaries are not promised to be bit-for-bit identical across different toolchains.

## Verification evidence

- On the Apple Silicon build host, native window and folder-picker use and temporary-folder scanning have been exercised. The arm64 app includes the ExifTool/Perl runtime; its build checks the payload digest and remaps repository and Cargo cache paths.
- The macOS Intel target has a Rosetta smoke check for the app and its embedded ExifTool. This does not replace testing on Intel hardware.
- The Windows x86_64 app is cross-compiled and checked for the GUI subsystem and unexpected MinGW runtime DLL imports. It has not yet been run through full workflows on Windows.
- The locked Rust test suite previously passed 54 tests, with 3 ignored cases (two requiring external ExifTool fixtures and one explicit 2 GB archive stress case). A 2,000,000,001-byte archive case and official 7-Zip interoperability were also recorded as passing on the build host.
- HEIC/HEIF preview decoding has been exercised with generated HEIC samples. Real camera and phone sample coverage is still needed.

## Remaining acceptance work

- Run offline, clean-machine startup and full workflows on macOS arm64, macOS Intel, and Windows x86_64.
- Sign and notarize macOS packages; verify Finder launch, file pickers, Dock behavior, and Gatekeeper distribution.
- Test real-device HEIC/HEIF samples and platform-specific path, permission, and removable-drive behavior.
- Validate large archive temporary-space requirements. The implementation writes a complete temporary archive before splitting; peak temporary use can approach twice the final archive size. Isolated restore checking also needs space for a restored copy.
- The archive dependency's internal password buffer is not guaranteed to be zeroized. The app clears its own password strings.

See the [acceptance checklist](acceptance-checklist.md) for the remaining hands-on checks and the [technical design](technical-design.md) for safety flows.
