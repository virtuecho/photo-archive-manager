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

payload="resources/exiftool/macos-aarch64/exiftool"
expected_payload_sha256="224c4384d1df807c14f6ebcb04972ab5b918cac076b9dc4cebc99da981ed0b95"
if [[ ! -f "$payload" ]]; then
    echo "Embedded ExifTool payload is missing: resources/exiftool/macos-aarch64/exiftool" >&2
    exit 1
fi
actual_payload_sha256="$(shasum -a 256 "$payload" | awk '{ print $1 }')"
if [[ "$actual_payload_sha256" != "$expected_payload_sha256" ]]; then
    echo "Embedded macOS arm64 ExifTool payload SHA-256 mismatch." >&2
    exit 1
fi

cargo_home="${CARGO_HOME:-$HOME/.cargo}"
project_remap="--remap-path-prefix=$repo_root=."
cargo_remap="--remap-path-prefix=$cargo_home=cargo"
if [[ -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]]; then
    export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS}"$'\x1f'"$project_remap"$'\x1f'"$cargo_remap"
else
    printf -v project_remap '%q' "$project_remap"
    printf -v cargo_remap '%q' "$cargo_remap"
    export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$project_remap $cargo_remap"
fi

cargo fmt --all -- --check
cargo test --locked
cargo build --locked --release --target "$host"

binary="target/$host/release/photo-archive-manager"
test -x "$binary"
file "$binary"
otool -L "$binary"
printf 'Build complete: %s\n' "$binary"
