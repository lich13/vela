"""Allow only the two installers and the Linux server archive into a release."""
import pathlib
import shutil
import sys
source, destination = map(pathlib.Path, sys.argv[1:3])
version = sys.argv[3]
expected = {
    f"Vela_{version}_aarch64.dmg",
    f"Vela_{version}_x64-setup.exe",
    f"vela-server_{version}_x86_64-linux.tar.gz",
}
files = [p for p in source.rglob("*") if p.is_file()]
if len(files) != len(expected) or {p.name for p in files} != expected:
    raise SystemExit(f"Unexpected release assets: {[p.name for p in files]}")
for path in files:
    shutil.copy2(path, destination / path.name)
