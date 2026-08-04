#!/usr/bin/env python3

import io
import os
from pathlib import Path, PurePosixPath
import stat
import struct
import sys
import tarfile
import zlib


def fail(message: str) -> None:
    raise SystemExit(f"error: {message}")


def archive_paths(project: Path, inputs: list[str]) -> list[Path]:
    paths: dict[str, Path] = {}
    for raw in inputs:
        relative = PurePosixPath(raw)
        if relative.is_absolute() or ".." in relative.parts:
            fail(f"invalid source input: {raw}")
        path = project.joinpath(*relative.parts)
        if not path.exists() and not path.is_symlink():
            fail(f"source input does not exist: {raw}")
        paths[relative.as_posix()] = path
        if path.is_dir():
            for child in path.rglob("*"):
                child_relative = child.relative_to(project).as_posix()
                paths[child_relative] = child
    return [paths[name] for name in sorted(paths)]


def tar_bytes(project: Path, version: str, inputs: list[str]) -> bytes:
    output = io.BytesIO()
    prefix = PurePosixPath(f"served-{version}")
    with tarfile.open(fileobj=output, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        for path in archive_paths(project, inputs):
            relative = PurePosixPath(path.relative_to(project).as_posix())
            info = tarfile.TarInfo((prefix / relative).as_posix())
            metadata = path.lstat()
            info.uid = 0
            info.gid = 0
            info.uname = ""
            info.gname = ""
            info.mtime = 0

            if stat.S_ISDIR(metadata.st_mode):
                info.type = tarfile.DIRTYPE
                info.mode = 0o755
                archive.addfile(info)
            elif stat.S_ISREG(metadata.st_mode):
                info.mode = 0o755 if metadata.st_mode & 0o111 else 0o644
                info.size = metadata.st_size
                with path.open("rb") as source:
                    archive.addfile(info, source)
            elif stat.S_ISLNK(metadata.st_mode):
                info.type = tarfile.SYMTYPE
                info.mode = 0o777
                info.linkname = os.readlink(path)
                archive.addfile(info)
            else:
                fail(f"unsupported source input type: {relative}")
    return output.getvalue()


def gzip_stored(data: bytes) -> bytes:
    result = bytearray(b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03")
    chunks = [data[index : index + 65535] for index in range(0, len(data), 65535)]
    if not chunks:
        chunks = [b""]
    for index, chunk in enumerate(chunks):
        result.append(1 if index == len(chunks) - 1 else 0)
        length = len(chunk)
        result.extend(struct.pack("<HH", length, length ^ 0xFFFF))
        result.extend(chunk)
    result.extend(struct.pack("<II", zlib.crc32(data) & 0xFFFFFFFF, len(data) & 0xFFFFFFFF))
    return bytes(result)


def main() -> None:
    if len(sys.argv) < 5:
        fail("usage: package-source.py PROJECT OUTPUT VERSION INPUT...")
    project = Path(sys.argv[1]).resolve()
    output = Path(sys.argv[2])
    version = sys.argv[3]
    output.write_bytes(gzip_stored(tar_bytes(project, version, sys.argv[4:])))


if __name__ == "__main__":
    main()
