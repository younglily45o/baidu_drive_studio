"""Package an already-built Windows x64 executable; no credentials or network."""
from pathlib import Path
import hashlib
import shutil
import zipfile

root = Path(__file__).resolve().parents[1]
binary = root / "target/x86_64-pc-windows-gnu/release/Baidu-Drive-Studio.exe"
if not binary.is_file():
    raise SystemExit("Build first: cargo build --locked --release --target x86_64-pc-windows-gnu")
dist = root / "dist"
output = dist / "Baidu-Drive-Studio-Windows-x64"
output.mkdir(parents=True, exist_ok=True)
names = ["Baidu-Drive-Studio.exe", "README.md", "LICENSE", "THIRD-PARTY-NOTICES.md", "DEPENDENCY-LICENSES.txt", "FONT-LICENSE.txt", "docs/verification.md", "docs/direct-link-feasibility.md"]
for name in names:
    source = binary if name.endswith(".exe") else root / name
    if name == "FONT-LICENSE.txt": source = root / "assets" / name
    (output / name).parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, output / name)
checksums = [hashlib.sha256((output / name).read_bytes()).hexdigest() + "  " + name for name in sorted(names)]
(output / "SHA256SUMS.txt").write_text("\n".join(checksums) + "\n", encoding="utf-8")
archive = dist / "Baidu-Drive-Studio-Windows-x64.zip"
with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as package:
    for name in sorted(names + ["SHA256SUMS.txt"]):
        package.write(output / name, output.name + "/" + name)
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_suffix(".zip.sha256").write_text(digest + "  " + archive.name + "\n", encoding="ascii")
print(archive)
print("SHA256", digest)
