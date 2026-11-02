# Clean-machine acceptance checklist

**English | [简体中文](acceptance-checklist.zh-CN.md)**

Use this checklist for each release candidate. Record actual results and evidence; a cross-build, development-machine run, or unchecked box is not clean-machine acceptance. Do not place real GPS data or passwords in shared evidence.

## Acceptance record

| Field | Record |
| --- | --- |
| Date / operator |  |
| Git revision |  |
| OS version / architecture |  |
| ZIP filename / SHA-256 |  |
| Clean machine or VM |  |
| Network disconnected |  |
| Result / evidence location |  |

## Package and launch

- [ ] Build the packages from the intended revision; confirm the three ZIP filenames identify version, platform, and architecture and that `SHA256SUMS.txt` matches them.
- [ ] On a clean, offline system without Rust, Python, Perl, ExifTool, 7-Zip, HEIF extensions, or extra runtimes installed, extract and launch the matching package.
- [ ] For macOS, launch `Photo Archive Manager.app` from Finder; check the file picker, Dock, permissions, signing, notarization, and Gatekeeper behavior.
- [ ] For Windows, launch the GUI executable and verify startup and embedded ExifTool extraction without installing additional runtimes.
- [ ] Confirm the first-run interface is English; switch to Chinese and back while the app is open, then restart and verify the selected language is remembered.
- [ ] Confirm the bilingual quick-start, technical pages, and third-party notices are included and their language links work.

## Scan, organization, and verification

- [ ] Scan parent and child folders together; confirm files are not counted twice and symbolic-link loops are not followed.
- [ ] Route HEIC, MOV, and JPEG to one folder and RAW to another; check unmapped types are reported and same-name destination files are not overwritten.
- [ ] Confirm default system-file exclusions and the option to include them; regular hidden files and sidecars remain available.
- [ ] Compare source file lists and hashes before and after organization to confirm originals were not changed.
- [ ] Change one copied destination byte without changing its size; verification must report a SHA-256 mismatch. Restore it and rerun verification.
- [ ] Pause, resume, cancel, and switch stages at safe boundaries; force-close during an organization copy and backup, then confirm only supported checkpoints are offered and existing outputs are revalidated.

## Metadata and duplicate review

- [ ] Check GPS and capture-time states for valid, missing, partial, invalid, unsupported, and read-error samples; valid `0,0` coordinates must not be rejected solely for being zero.
- [ ] Modify a missing metadata field outside the app and run a full recheck; confirm the changed field is marked `RESOLVED` and dependent earlier results need rechecking.
- [ ] Acknowledge a readable warning; confirm it is marked `ACCEPTED` without changing the underlying field state, then change the file and confirm the acknowledgement clears.
- [ ] Compare trusted capture times, timezone handling, and known different cameras; filesystem timestamps must not be presented as capture times.
- [ ] Confirm byte-identical files are reported without deletion, while visual candidates remain reviewable and can be excluded.
- [ ] Test real phone and camera HEIC/HEIF samples, orientation, previews, perceptual hashes, and the 50-megapixel decode limit.
- [ ] Exclude a duplicate candidate, restart the app, and confirm the review decision is restored and changed decisions update the manifest.

## Backups and encrypted archives

- [ ] Back up one source to multiple targets; verify each target independently by rereading it and comparing SHA-256.
- [ ] Test an unwritable or disconnected target; other target results remain visible and existing files are not overwritten.
- [ ] Create an encrypted 7z archive with a confirmed password; verify that passwords are absent from logs and persistent checkpoints.
- [ ] Test archive listing and extraction with an independent 7-Zip installation and confirm a wrong password cannot reveal encrypted names or contents.
- [ ] Test small split volumes, a missing volume, and a file larger than 2,000,000,000 bytes; compare extracted and source SHA-256 values.
- [ ] Exercise optional isolated restore verification; confirm restored files are reread and checked before commit and temporary data is removed.
- [ ] Interrupt archive creation and restart; confirm recovery requires the password again and rebuilds from the beginning without overwriting existing output.

## Platform results

### macOS Apple Silicon and Intel

- [ ] Verify the correct architecture-specific `.app` opens on matching hardware with networking disabled.
- [ ] Complete scan, metadata, duplicate review, backup, encrypted archive, and restore checks.
- [ ] Run the Intel package on Intel hardware; a Rosetta check on Apple Silicon does not substitute for this step.
- [ ] Complete signing, notarization, Finder, Dock, permissions, and Gatekeeper checks.

### Windows x86_64

- [ ] Verify the package launches on clean Windows without Perl, ExifTool, 7-Zip, HEIF extensions, or extra runtimes installed.
- [ ] Check embedded ExifTool extraction, hash/version verification, Unicode and spaced paths, plus metadata reads for common photo and video formats.
- [ ] Complete preview, backup, encrypted-volume creation, independent archive test, and extraction checks.
- [ ] Record Windows trust prompts and signing status.
