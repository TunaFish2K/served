#!/usr/bin/env python3
"""Exercise real package upgrade/rollback functions in an unprivileged prefix.

Only the systemd transport is replaced; no host installation files are touched.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time

assert sys.platform == "linux"
assert len(sys.argv) == 3, "usage: linux_variant_install.py FULL_ARCHIVE HEADLESS_ARCHIVE"

INSTALL = r'''
set -euo pipefail
source "$1/test-install-functions.sh"
script_dir="$1"
binary_target="$2/bin/served"
template_target="$2/served@.service"
legacy_system_target="$2/legacy.service"
served_docs_init "$script_dir" "$2"
had_binary=1
had_template=1
instance_active=1
instance_enabled=1
active_instances=(served@test.service)
root_cmd() { "$@"; }
systemctl_root() {
    case "$1" in
        daemon-reload|enable|start) return 0 ;;
        reload) "$binary_target" daemon --handoff ;;
        restart) return 1 ;;
        *) return 1 ;;
    esac
}
unit_active() { "$binary_target" list >/dev/null 2>&1; }
trap cleanup EXIT
served_docs_validate
render_units
backup_files
install_files || abort_install "package installation failed"
reload_active_instances
'''

with tempfile.TemporaryDirectory(prefix="served-variants-") as temporary:
    root = Path(temporary)
    packages = []
    for archive in sys.argv[1:]:
        with tarfile.open(archive) as tar:
            tar.extractall(root, filter="data")
        package = root / Path(archive).name.removesuffix(".tar.gz")
        source = (package / "install.sh").read_text().split("trap cleanup EXIT", 1)[0]
        (package / "test-install-functions.sh").write_text(source)
        packages.append(package)
    full, headless = packages
    for package, expected in [(full, "full"), (headless, "headless")]:
        result = subprocess.run([str(package / "served"), "version", "--output", "json"], capture_output=True, check=True)
        assert json.loads(result.stdout)["data"]["variant"] == expected
    prefix = root / "prefix"
    binary = prefix / "bin/served"
    binary.parent.mkdir(parents=True)
    shutil.copy2(full / "served", binary)
    shutil.copy2(full / "served@.service", prefix / "served@.service")
    shutil.copytree(full / "share", prefix / "share")
    home = root / "home"
    home.mkdir()
    environment = dict(os.environ, HOME=str(home))
    service = root / "service"
    service.mkdir()
    (service / ".served.json5").write_text(json.dumps({"name": "api", "command": "sleep 300", "tty": False}))

    def cli(*args, executable=binary):
        return subprocess.run([str(executable), *args], env=environment, cwd=service,
                              capture_output=True, timeout=15)

    def data(*args, executable=binary):
        result = cli(*args, "--output", "json", executable=executable)
        assert result.returncode == 0, result
        return json.loads(result.stdout)["data"]

    with (root / "manager.log").open("wb") as log:
        manager = subprocess.Popen([str(binary), "daemon"], env=environment, stdout=log, stderr=log)
        try:
            for _ in range(100):
                if cli("list").returncode == 0:
                    break
                time.sleep(0.05)
            assert cli("enable").returncode == 0
            for _ in range(100):
                state = data("list")["services"][0]
                if state["pid"]:
                    break
                time.sleep(0.05)
            pid = state["pid"]
            assert pid is not None
            for package, variant in [(headless, "headless"), (full, "full")]:
                result = subprocess.run(["bash", "-c", INSTALL, "install-test", str(package), str(prefix)],
                                        env=environment, capture_output=True, timeout=30)
                assert result.returncode == 0, result
                assert data("version")["variant"] == variant
                for client in [full / "served", headless / "served"]:
                    assert data("list", executable=client)["services"][0]["pid"] == pid
                assert manager.poll() is None
            original = binary.read_bytes()
            original_manual = (prefix / "share/man/man1/served.1").read_bytes()
            (headless / "served").unlink()
            (headless / "served").write_text("#!/bin/sh\nexit 1\n")
            (headless / "served").chmod(0o755)
            (headless / "share/man/man1/served.1").write_text("invalid new manual\n")
            result = subprocess.run(["bash", "-c", INSTALL, "install-test", str(headless), str(prefix)],
                                    env=environment, capture_output=True, timeout=30)
            assert result.returncode != 0, result
            assert b"previous installation restored" in result.stderr, result
            assert binary.read_bytes() == original
            assert (prefix / "share/man/man1/served.1").read_bytes() == original_manual
            assert data("list")["services"][0]["pid"] == pid
        finally:
            cli("shutdown")
            try:
                manager.wait(timeout=10)
            except subprocess.TimeoutExpired:
                manager.kill()
                manager.wait()
print("Linux variant handoff, mixed clients, and failed-upgrade rollback passed")
