#!/usr/bin/env python3
"""Package the versioned Linux x86-64 binary and its desktop installer."""

import argparse
import gzip
from pathlib import Path
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib


RS = Path(__file__).resolve().parents[1]
INSTALLER = RS / "installer"
PNG_HEADER = b"\x89PNG\r\n\x1a\n"


def add_icons(destination: Path) -> None:
    ico = (INSTALLER / "alm.ico").read_bytes()
    count = struct.unpack_from("<H", ico, 4)[0]
    for index in range(count):
        length, offset = struct.unpack_from("<II", ico, 14 + 16 * index)
        png = ico[offset : offset + length]
        if not png.startswith(PNG_HEADER):
            raise SystemExit("Expected PNG images in the installer icon")
        width, height = struct.unpack_from(">II", png, 16)
        path = destination / f"{width}x{height}" / "stable-alm.png"
        path.parent.mkdir(parents=True)
        path.write_bytes(png)


def archive_metadata(member: tarfile.TarInfo) -> tarfile.TarInfo:
    member.mtime = member.uid = member.gid = 0
    member.uname = member.gname = ""
    member.mode = 0o755 if member.isdir() or member.name.endswith(("/StableALM", "/install.sh")) else 0o644
    return member


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, help="the built Linux StableALM binary")
    binary = parser.parse_args().binary.resolve()
    with binary.open("rb") as source:
        header = source.read(20)
    if not header.startswith(b"\x7fELF\x02\x01") or header[18:20] != b"\x3e\x00":
        raise SystemExit("Expected an x86-64 Linux ELF binary")
    version = tomllib.loads((RS / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    if subprocess.check_output([binary, "--version"], text=True).strip() != f"StableALM {version}":
        raise SystemExit(f"The binary must report StableALM {version}")

    dist = RS / "dist" / version
    dist.mkdir(parents=True, exist_ok=True)
    name = f"StableALM-{version}-linux-x86_64"
    output = dist / f"{name}.tar.gz"
    with tempfile.TemporaryDirectory(dir=dist) as temporary:
        staging = Path(temporary)
        bundle = staging / name
        bundle.mkdir()
        shutil.copyfile(binary, bundle / "StableALM")
        shutil.copyfile(RS.parent / "LICENSE", bundle / "LICENSE")
        for filename in ("install.sh", "INSTALL.txt"):
            shutil.copyfile(INSTALLER / "linux" / filename, bundle / filename)
        add_icons(bundle / "icons")
        archive = staging / output.name
        with archive.open("wb") as raw, gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=0) as zipped:
            with tarfile.open(fileobj=zipped, mode="w") as tar:
                tar.add(bundle, arcname=name, filter=archive_metadata)
        archive.replace(output)
    print(output)


if __name__ == "__main__":
    main()
