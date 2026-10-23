#!/usr/bin/env python3
"""Prints the licenses of every library built into Sonora for a target.

Usage: third_party_notices.py --target x86_64-pc-windows-msvc

Follows the app's normal (shipped) dependencies, not build or dev ones, and
includes each library's own license files, with identical texts given once.
"""

import argparse
import json
import pathlib
import subprocess
import sys

LICENSE_NAMES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True)
    args = parser.parse_args()

    root = pathlib.Path(__file__).resolve().parent.parent
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", args.target],
            cwd=root,
            check=True,
            capture_output=True,
            encoding="utf-8",
        ).stdout
    )
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    app = next(package["id"] for package in metadata["packages"] if package["name"] == "app" and package["source"] is None)

    shipped, pending = set(), [app]
    while pending:
        node = nodes[pending.pop()]
        for dep in node["deps"]:
            if any(kind["kind"] is None for kind in dep["dep_kinds"]) and dep["pkg"] not in shipped:
                shipped.add(dep["pkg"])
                pending.append(dep["pkg"])

    # Sonora's own crates are under its own license.
    libraries = sorted(
        (packages[pid] for pid in shipped if packages[pid]["source"] is not None),
        key=lambda package: (package["name"], package["version"]),
    )
    by_text = {}
    unlicensed = []
    for package in libraries:
        folder = pathlib.Path(package["manifest_path"]).parent
        files = sorted(
            path for path in folder.iterdir() if path.is_file() and path.name.upper().startswith(LICENSE_NAMES)
        )
        label = f"{package['name']} {package['version']} ({package.get('license') or 'see its license'})"
        if not files:
            unlicensed.append(label)
        for path in files:
            text = path.read_text(encoding="utf-8", errors="replace").strip()
            by_text.setdefault(text, []).append(label)

    out = sys.stdout
    out.reconfigure(encoding="utf-8")
    out.write("Sonora includes the following open-source libraries.\n\n")
    for package in libraries:
        out.write(f"- {package['name']} {package['version']}: {package.get('license') or 'unknown'}")
        if package.get("repository"):
            out.write(f" ({package['repository']})")
        out.write("\n")
    for text, labels in by_text.items():
        out.write("\n" + "=" * 78 + "\n")
        out.write("Applies to: " + ", ".join(sorted(set(labels))) + "\n")
        out.write("=" * 78 + "\n\n")
        out.write(text + "\n")
    if unlicensed:
        out.write("\n" + "=" * 78 + "\n")
        out.write("These ship no license file; their license is as listed above:\n")
        for label in unlicensed:
            out.write(f"- {label}\n")


if __name__ == "__main__":
    main()
