#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "This build entry point requires macOS." >&2
    exit 1
fi

target="x86_64-apple-darwin"
payload="resources/exiftool/macos-x86_64/exiftool"
expected_payload_sha256="cbd028efc28cee50fe5418dd63b14797c76039d636746425cd79f53917954ac6"
actual_payload_sha256="$(shasum -a 256 "$payload" | awk '{ print $1 }')"
if [[ "$actual_payload_sha256" != "$expected_payload_sha256" ]]; then
    echo "Embedded macOS Intel ExifTool payload SHA-256 mismatch." >&2
    exit 1
fi
if ! rustup target list --installed | rg -qx "$target"; then
    echo "Install the Rust target with: rustup target add $target" >&2
    exit 1
fi

cargo fmt --all -- --check
cargo test --locked
cargo build --locked --release --target "$target"

binary="target/$target/release/photo-archive-manager"
test -x "$binary"
file "$binary"
otool -L "$binary"
printf 'Build complete: %s\n' "$binary"
