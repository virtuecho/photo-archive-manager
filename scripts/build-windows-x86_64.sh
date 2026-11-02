#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

target="x86_64-pc-windows-gnu"
payload="resources/exiftool/windows-x86_64/exiftool-13.60_64.zip"
expected_payload_sha256="548b1f4e0b713cd2a26824b34080c35db4e2d2edd166468eb38b1d55ada90c2e"
actual_payload_sha256="$(shasum -a 256 "$payload" | awk '{ print $1 }')"
if [[ "$actual_payload_sha256" != "$expected_payload_sha256" ]]; then
    echo "Embedded Windows ExifTool payload SHA-256 mismatch." >&2
    exit 1
fi

if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
    echo "Install a MinGW-w64 x86_64 linker before building this target." >&2
    exit 1
fi
if ! rustup target list --installed | rg -qx "$target"; then
    echo "Install the Rust target with: rustup target add $target" >&2
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
cargo build --locked --release --target "$target"

binary="target/$target/release/photo-archive-manager.exe"
test -f "$binary"
file "$binary"

objdump="x86_64-w64-mingw32-objdump"
headers="$("$objdump" -p "$binary")"
if ! printf '%s\n' "$headers" | rg 'Subsystem.*Windows GUI' >/dev/null; then
    echo "The executable is not marked as a Windows GUI application." >&2
    exit 1
fi
dependencies="$(printf '%s\n' "$headers" | awk '/DLL Name/ { print tolower($3) }')"
if [[ "$dependencies" =~ (^|$'\n')(libgcc_s_seh-1\.dll|libstdc\+\+-6\.dll|libwinpthread-1\.dll)($|$'\n') ]]; then
    echo "The executable depends on a MinGW runtime DLL that is not bundled." >&2
    exit 1
fi

printf '%s\n' "$dependencies"
printf 'Build complete: %s\n' "$binary"
