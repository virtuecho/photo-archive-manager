# Photo Archive Manager

**English | [简体中文](README.zh-CN.md)**

Photo Archive Manager is an offline desktop app for organizing, checking, backing up, and encrypting photo archives. It keeps your original files in place while you review and copy them.

## How it works

1. Choose photo folders and review the scan.
2. Organize files, check metadata or duplicates, and create verified backups.
3. Create an encrypted archive when you are ready to store a long-term copy.

The app works with files on your computer. It does not upload photos. It checks copied and archived content before reporting success, and it does not overwrite an existing destination file. Archive passwords are not saved in recovery records.

On first launch, the interface defaults to English. Choose **English** or **中文** in the app header to switch languages immediately; the choice is remembered for the next launch.

## Get a build

On an Apple Silicon Mac, run `./scripts/build-releases.sh`. It builds the available platform targets and writes versioned ZIP files to `target/Releases/`. macOS ZIPs contain a `.app` bundle. Platform and CPU architecture are part of every ZIP filename.

See [Build and release guide](docs/release.md) for prerequisites, exact filenames, output locations, and current verification limits.

## More information

- [Technical design and diagrams](docs/technical-design.md)
- [Implementation status](docs/implementation-status.md)
- [Build and release guide](docs/release.md)
- [Clean-machine acceptance checklist](docs/acceptance-checklist.md)
- [Third-party software and licenses](resources/THIRD_PARTY_NOTICES.md)
