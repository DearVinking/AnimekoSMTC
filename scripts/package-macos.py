#!/usr/bin/env python3
"""Package already-built macOS binaries; apply a development ad-hoc signature."""
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {"aarch64-apple-darwin": "arm64", "x86_64-apple-darwin": "x86_64"}


def run(*args):
    subprocess.run(args, cwd=ROOT, check=True)


def package(target, profile):
    arch = TARGETS[target]
    build = ROOT / "target" / target / profile
    binaries = [build / "AnimekoSMTC", build / "libanimeko_probe.dylib"]
    for binary in binaries:
        if not binary.is_file():
            raise ValueError(f"Missing {binary}; build the selected target/profile first")
        run("lipo", str(binary), "-verify_arch", arch)
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], cwd=ROOT))
    version = next(p["version"] for p in metadata["packages"] if p["name"] == "animeko-smtc")
    match = re.fullmatch(r"(\d+\.\d+\.\d+)(?:[-+].*)?", version)
    if not match:
        raise ValueError(f"Version cannot be represented in Info.plist: {version}")
    info = {
        "CFBundleIdentifier": "io.github.dearvinking.AnimekoSMTC",
        "CFBundleName": "Animeko SMTC",
        "CFBundleDisplayName": "Animeko SMTC",
        "CFBundleExecutable": "AnimekoSMTC",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": match[1],
        "CFBundleVersion": match[1],
        "CFBundleGetInfoString": "Animeko SMTC " + version,
        "CFBundleIconFile": "ani.icns",
        "LSMinimumSystemVersion": "11.0",
        "LSUIElement": True,
    }
    dist = ROOT / "dist"
    dist.mkdir(exist_ok=True)
    name = f"AnimekoSMTC-{version}-macos-{arch}.zip"
    with tempfile.TemporaryDirectory(prefix=".macos-package-", dir=dist) as temporary:
        staging = Path(temporary)
        app = staging / "AnimekoSMTC.app"
        contents = app / "Contents"
        for directory in ["MacOS", "Frameworks", "Resources"]:
            (contents / directory).mkdir(parents=True)
        executable = contents / "MacOS" / "AnimekoSMTC"
        probe = contents / "Frameworks" / "libanimeko_probe.dylib"
        shutil.copy2(binaries[0], executable)
        shutil.copy2(binaries[1], probe)
        run("install_name_tool", "-id", "@rpath/libanimeko_probe.dylib", str(probe))
        resources = contents / "Resources"
        shutil.copy2(ROOT / "crates/companion/assets/ani.icns", resources / "ani.icns")
        for resource in ["LICENSE", "THIRD-PARTY-NOTICES.txt", "README.md"]:
            shutil.copy2(ROOT / resource, resources / resource)
        shutil.copytree(ROOT / "licenses", resources / "licenses")
        (contents / "Info.plist").write_bytes(plistlib.dumps(info))
        for code in [probe, executable, app]:
            run("codesign", "--force", "--sign", "-", str(code))
        for code in [probe, executable, app]:
            run("codesign", "--verify", "--strict", "--verbose=2", str(code))
        archive = staging / name
        run("ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(app), str(archive))
        checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
        checksum_file = staging / (name + ".sha256")
        checksum_file.write_text(f"{checksum}  {name}\n", encoding="utf-8")
        output_app = dist / app.name
        if output_app.exists():
            shutil.rmtree(output_app)
        app.rename(output_app)
        archive.replace(dist / name)
        checksum_file.replace(dist / checksum_file.name)
    print(f"Created {dist / name} (ad-hoc signed development build; not notarized)")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--profile", choices=["debug", "release"], default="release")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("Packaging requires macOS codesign and ditto")
    try:
        package(args.target, args.profile)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Packaging failed: {error}\n")


if __name__ == "__main__":
    main()
