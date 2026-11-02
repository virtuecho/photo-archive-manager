#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "The unified release build requires macOS." >&2
    exit 1
fi
host="$(rustc -vV | awk '/^host:/ { print $2 }')"
if [[ "$host" != "aarch64-apple-darwin" ]]; then
    echo "The unified release build requires an Apple Silicon Mac; detected $host." >&2
    exit 1
fi

./scripts/build-macos-arm64.sh
./scripts/build-macos-x86_64.sh
./scripts/build-windows-x86_64.sh
python3 ./scripts/package-releases.py
