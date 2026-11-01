#!/usr/bin/env python3
"""Stamp only the Cargo workspace package version for local Makefile builds."""

import re
import sys
from pathlib import Path


def set_build_version(manifest: Path, version: str) -> None:
    number = r"(?:0|[1-9][0-9]*)"
    identifier = r"[0-9A-Za-z-]+"
    pattern = (
        rf"{number}\.{number}\.{number}"
        rf"(?:-({identifier}(?:\.{identifier})*))?"
        rf"(?:\+{identifier}(?:\.{identifier})*)?"
    )
    match = re.fullmatch(pattern, version)
    if match is None or (
        match[1] is not None
        and any(
            part.isdigit() and len(part) > 1 and part.startswith("0")
            for part in match[1].split(".")
        )
    ):
        raise ValueError(f"invalid semantic version: {version!r}")

    original = manifest.read_bytes()
    text = original.decode("utf-8")
    section = re.search(
        r"(?ms)^\[workspace\.package\][ \t]*\r?\n(?P<body>.*?)(?=^\[|\Z)",
        text,
    )
    if section is None:
        raise ValueError(f"missing [workspace.package] in {manifest}")
    body, count = re.subn(
        r'(?m)^(version[ \t]*=[ \t]*")[^"]*(")',
        lambda match: f"{match[1]}{version}{match[2]}",
        section["body"],
    )
    if count != 1:
        raise ValueError(f"expected one [workspace.package].version in {manifest}")
    updated = (
        text[: section.start("body")] + body + text[section.end("body") :]
    ).encode("utf-8")
    if updated != original:
        manifest.write_bytes(updated)
        print(f"Set workspace version to {version}")


if __name__ == "__main__":
    try:
        set_build_version(Path(sys.argv[1]), sys.argv[2])
    except (IndexError, OSError, ValueError) as error:
        sys.exit(f"set-build-version: {error}")
