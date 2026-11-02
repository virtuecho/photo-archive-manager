#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "This build entry point requires macOS." >&2
    exit 1
fi

host="$(rustc -vV | awk '/^host:/ { print $2 }')"
if [[ "$host" != "aarch64-apple-darwin" ]]; then
    echo "This build entry point supports macOS arm64; detected $host." >&2
    exit 1
fi

if [[ ! -f resources/exiftool/macos-aarch64/exiftool ]]; then
    echo "Embedded ExifTool payload is missing: resources/exiftool/macos-aarch64/exiftool" >&2
    exit 1
fi

cargo fmt --all -- --check
cargo test --locked
cargo build --locked --release --target "$host"

binary="target/$host/release/photo-archive-manager"
test -x "$binary"
file "$binary"
otool -L "$binary"
printf 'Build complete: %s\n' "$binary"
