"""Collect published license texts from the exact locked Cargo packages."""
from pathlib import Path
import json
import subprocess

root = Path(__file__).resolve().parents[1]
metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--filter-platform", "x86_64-pc-windows-gnu", "--format-version", "1"], cwd=root
))
parts = ["Baidu Drive Studio - locked Windows dependency license texts\n"]
missing = []
for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
    if package["name"] == "baidu-drive-studio":
        continue
    directory = Path(package["manifest_path"]).parent
    candidates = set()
    for pattern in ["LICENSE*", "LICENCE*", "COPYING*", "UNLICENSE*", "NOTICE*", "license/*", "licenses/*"]:
        candidates.update(p for p in directory.glob(pattern) if p.is_file())
    if package.get("license_file"):
        candidates.add(directory / package["license_file"])
    parts.append("\n" + "=" * 78 + "\n" + package["name"] + " " + package["version"])
    parts.append("License: " + (package.get("license") or "see license file"))
    parts.append("Repository: " + (package.get("repository") or ""))
    if not candidates:
        missing.append(package["name"])
    for candidate in sorted(candidates):
        parts.append("\n--- " + candidate.relative_to(directory).as_posix() + " ---\n")
        parts.append(candidate.read_text(encoding="utf-8", errors="replace"))
for supplemental in sorted((root / "assets/licenses").glob("*.txt")):
    parts.append("\n" + "=" * 78 + "\nSupplemental upstream license: " + supplemental.name)
    parts.append(supplemental.read_text(encoding="utf-8"))
output = root / "DEPENDENCY-LICENSES.txt"
output.write_text("\n".join(parts) + "\n", encoding="utf-8")
print(output)
if missing:
    print("Packages declaring licenses without a standalone license file:", ", ".join(missing))
