"""Appends the licence of every crate in the shipped programs to LICENCE.

A release build runs this first, so that the installed LICENCE.txt and the About
box carry the notices that the crates' licences ask for.
"""
import json
import subprocess
from pathlib import Path

ROOTS = {"rustutty", "deck-tools", "deck-setup"}
NAMES = ("licen", "copying", "notice", "copyright", "unlicense")


def shipped(metadata):
    """The crates the programs link, leaving out build and test dependencies."""
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    packages = {p["id"]: p for p in metadata["packages"]}
    seen = set()
    todo = [p["id"] for p in metadata["packages"] if p["name"] in ROOTS and p["source"] is None]
    while todo:
        id = todo.pop()
        if id in seen:
            continue
        seen.add(id)
        for dep in nodes[id]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                todo.append(dep["pkg"])
    return sorted((packages[i] for i in seen if packages[i]["source"]), key=lambda p: (p["name"], p["version"]))


def main():
    out = subprocess.run(["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", "x86_64-pc-windows-msvc"],
                         check=True, capture_output=True, encoding="utf-8").stdout
    crates = shipped(json.loads(out))
    parts = ["\n\nThird-party notices\n===================\n\n"
             "RUSTuTTY includes the following crates, under these licences.\n"]
    for c in crates:
        parts.append(f"\n\n{'-' * 72}\n{c['name']} {c['version']}: {c['license'] or 'see below'}\n{'-' * 72}\n")
        folder = Path(c["manifest_path"]).parent
        for f in sorted(folder.iterdir()):
            if f.is_file() and f.name.lower().startswith(NAMES):
                parts.append(f"\n{f.name}:\n\n{f.read_text(encoding='utf-8', errors='replace')}\n")
    with open("LICENCE", "a", encoding="utf-8", newline="\n") as licence:
        licence.write("".join(parts))
    print(f"{len(crates)} crates")


main()
