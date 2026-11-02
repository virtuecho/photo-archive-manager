#!/usr/bin/env python3
"""Extract the license texts shipped in the official Windows ExifTool package."""

from __future__ import annotations

from hashlib import sha256
from io import BytesIO
from pathlib import Path
from zipfile import ZipFile


ROOT = Path(__file__).resolve().parent.parent
PAYLOAD = ROOT / "resources/exiftool/windows-x86_64/exiftool-13.60_64.zip"
REPORT = ROOT / "resources/exiftool/windows-x86_64/THIRD_PARTY_LICENSES.md"
PAYLOAD_SHA256 = "548b1f4e0b713cd2a26824b34080c35db4e2d2edd166468eb38b1d55ada90c2e"


def normalize_license(text: str) -> str:
    return "\n".join(line.rstrip() for line in text.splitlines()).strip()


def main() -> None:
    payload = PAYLOAD.read_bytes()
    actual_hash = sha256(payload).hexdigest()
    if actual_hash != PAYLOAD_SHA256:
        raise SystemExit(f"Unexpected ExifTool package SHA-256: {actual_hash}")

    with ZipFile(BytesIO(payload)) as package:
        prefix = "exiftool-13.60_64/exiftool_files/"
        main_license = normalize_license(
            package.read(prefix + "LICENSE").decode("utf-8", errors="replace")
        )
        nested_bytes = package.read(prefix + "Licenses_Strawberry_Perl.zip")

    sections = [
        "# Windows ExifTool and Perl runtime licenses",
        "",
        "The following notices and license texts were extracted from the SHA-256-verified official ExifTool 13.60 Windows x86_64 package.",
        "The complete original license archive remains embedded in the application payload.",
        "",
        "## ExifTool Windows package license (exiftool_files/LICENSE)",
        "",
        main_license,
    ]

    with ZipFile(BytesIO(nested_bytes)) as licenses:
        for name in sorted(licenses.namelist(), key=str.casefold):
            if name.endswith("/") or name.endswith(".diff"):
                continue
            leaf = name.rsplit("/", 1)[-1].lower()
            if not any(token in leaf for token in ("license", "copying", "copyright", "notice", "authors", "_info_")):
                continue
            data = licenses.read(name)
            text = normalize_license(data.decode("utf-8", errors="replace"))
            sections.extend(["", f"## Strawberry Perl bundle: `{name}`", "", text])

    REPORT.write_text("\n".join(sections) + "\n", encoding="utf-8")
    print(f"Wrote {REPORT.relative_to(ROOT)} ({REPORT.stat().st_size:,} bytes).")


if __name__ == "__main__":
    main()
