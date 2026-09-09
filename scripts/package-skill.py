#!/usr/bin/env python3
"""Create an architecture-independent, deterministic served skill archive."""
import gzip
import hashlib
import io
from pathlib import Path
import sys
import tarfile
import re

project = Path(__file__).resolve().parent.parent
output = Path(sys.argv[1])
version = re.search(r'^version = "([^"]+)"$', (project / "Cargo.toml").read_text(), re.MULTILINE).group(1)
output.mkdir(parents=True, exist_ok=True)
archive = output / f"served-v{version}-skill.tar.gz"
buffer = io.BytesIO()
with tarfile.open(fileobj=buffer, mode="w", format=tarfile.USTAR_FORMAT) as tar:
    root = project / "skills/served"
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        content = path.read_bytes()
        info = tarfile.TarInfo("served/" + path.relative_to(root).as_posix())
        info.mode = 0o644
        info.size = len(content)
        tar.addfile(info, io.BytesIO(content))
# Use a fixed header and filename, independent of the output directory.
with archive.open("wb") as stream:
    with gzip.GzipFile(filename="", fileobj=stream, mode="wb", mtime=0) as compressed:
        compressed.write(buffer.getvalue())
checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_name(archive.name + ".sha256").write_text(f"{checksum}  {archive.name}\n")
print(archive)
