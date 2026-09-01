#!/usr/bin/env python3
"""Ask whether the DJ crate's filenames agree with its tags.

The crate names files ``BPM - KEY - Artist - Title (Mix)``, from an era when
the software could not sort on anything else. The tags carry ``tbpm``/``tkey``
too — and **41% of them disagree with the name about the key**, which is why
the prefix cannot simply be stripped.

Run it against a local copy; over SMB it takes minutes and contends with
everything else.
"""

from __future__ import annotations

import argparse
import re
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Final

from genre.common import tags_of, walk

#: `BPM - KEY - rest`, where KEY is classical notation.
PREFIX: Final = re.compile(r"^(\d+)\s*-\s*([A-G][#b]?m?)\s*-\s*(.*)$")
#: A BPM gap this small is rounding, not disagreement.
BPM_TOLERANCE: Final = 0.6


@dataclass
class Findings:
    """What one pass over a crate turned up."""

    files: int = 0
    no_prefix: list[str] = field(default_factory=list)
    bpm_disagrees: list[str] = field(default_factory=list)
    key_disagrees: list[str] = field(default_factory=list)
    key_spelling: Counter[str] = field(default_factory=Counter)
    bpm_format: Counter[str] = field(default_factory=Counter)
    second_key: Counter[str] = field(default_factory=Counter)


def spelling_of(key: str) -> str:
    """Classify a key's accidental, which is what decides the normalisation target."""
    if "#" in key:
        return "sharp"
    return "flat" if key[1:2] == "b" else "natural"


def compare(path: Path, out: Findings) -> None:
    """Compare one file's name against its tags."""
    out.files += 1
    tags = tags_of(path)
    tag_bpm = (tags.get("tbpm") or tags.get("bpm") or "").strip()
    tag_key = (tags.get("tkey") or tags.get("initialkey") or "").strip()

    if tag_key:
        out.key_spelling[spelling_of(tag_key)] += 1
    if tag_bpm:
        out.bpm_format["float" if "." in tag_bpm else "int"] += 1
    second = (tags.get("tky2") or "").strip()
    if second and tag_key and second != tag_key:
        out.second_key[f"{tag_key} vs {second}"] += 1

    match = PREFIX.match(path.name)
    if not match:
        out.no_prefix.append(path.name)
        return
    name_bpm, name_key = match.group(1), match.group(2)
    if tag_bpm and abs(float(tag_bpm) - float(name_bpm)) > BPM_TOLERANCE:
        out.bpm_disagrees.append(f"{path.name[:60]}  name={name_bpm} tag={tag_bpm}")
    if tag_key and name_key != tag_key:
        out.key_disagrees.append(f"{path.name[:52]}  name={name_key} tag={tag_key}")


def report(root: Path, out: Findings) -> None:
    """Print the findings."""
    print(f"=== {out.files} files under {root} ===")
    print(f"no BPM-key prefix in the name : {len(out.no_prefix)}")
    print(f"name BPM disagrees with tag   : {len(out.bpm_disagrees)}")
    print(f"name key disagrees with tag   : {len(out.key_disagrees)}")
    print(f"\ntag key spelling: {dict(out.key_spelling)}")
    print(f"tag BPM format  : {dict(out.bpm_format)}")
    print(f"\ntkey vs tky2 disagreements: {sum(out.second_key.values())}")
    for pair, count in out.second_key.most_common(6):
        print(f"    {count:4}  {pair}")
    for title, items in (
        ("key disagreements", out.key_disagrees),
        ("BPM disagreements", out.bpm_disagrees),
        ("names with no prefix", out.no_prefix),
    ):
        print(f"\n--- sample {title} ---")
        for item in items[:8]:
            print(f"    {item}")


def main() -> None:
    """Audit the crate named on the command line."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", help="crate directory to audit")
    args = parser.parse_args()

    root = Path(args.root).expanduser()
    out = Findings()
    for path in walk(root):
        compare(path, out)
    report(root, out)


if __name__ == "__main__":
    main()
