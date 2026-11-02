#!/usr/bin/env python3
"""Regenerate the embedded Cargo license report and package notices."""

import json
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
REPORT = ROOT / "resources/licenses/RUST_DEPENDENCIES.md"
TEMPLATE = ROOT / "about.hbs"


def cargo_about(*args: str) -> str:
    result = subprocess.run(
        ["cargo", "about", "generate", *args],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout


def main() -> None:
    version = subprocess.run(
        ["cargo", "about", "--version"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if version != "cargo-about 0.9.3":
        raise SystemExit(f"Expected cargo-about 0.9.3, found {version}.")

    audit = json.loads(
        cargo_about("--format", "json", "--fail", "--locked", "--offline")
    )
    cargo_about("--fail", "--locked", "--offline", "-o", str(REPORT), str(TEMPLATE))

    notices = []
    for entry in audit["crates"]:
        package = entry["package"]
        package_root = Path(package["manifest_path"]).parent
        for path in sorted(package_root.iterdir()):
            name = path.name.lower()
            if path.is_file() and (
                name.startswith("notice")
                or name.startswith("copyright")
                or name.startswith("copying")
            ):
                notices.append((package["name"], package["version"], path))

    with REPORT.open("a", encoding="utf-8") as report:
        if notices:
            report.write("\n\nPackage NOTICE and copyright files\n")
        for name, version, path in notices:
            report.write(f"\n================================================================\n")
            report.write(f"{name} {version} · {path.name}\n\n")
            report.write(path.read_text(encoding="utf-8", errors="replace"))
            report.write("\n")

    normalized = "\n".join(line.rstrip() for line in REPORT.read_text(encoding="utf-8").splitlines())
    REPORT.write_text(normalized + "\n", encoding="utf-8")

    print(f"Wrote {REPORT.relative_to(ROOT)} with {len(notices)} package notice files.")


if __name__ == "__main__":
    main()
