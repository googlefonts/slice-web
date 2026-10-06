#!/usr/bin/env python3
"""Count the fonts a variable-positioning bug can reach.

Question this answers
---------------------
    Of the variable fonts in a google/fonts checkout, how many keep kerning or anchors in
    a GDEF item variation store, and how many address that store from inside an extension
    lookup?

The first number is how many fonts a static instance that ignores the store gets wrong,
anywhere off the default location. The second is how many the residual walk got wrong
while it skipped extension lookups (type 9): compilers move the biggest lookup into one,
and the biggest is usually the kerning.

Usage
-----
    .shaping-venv/bin/python tools/gdef-store-survey.py ~/google/fonts survey.tsv

Any Python with fontTools will do; `.shaping-venv/` is the one `kerning-compare.py` sets
up. It reads every `*/*/*.ttf` under the checkout (`ofl/`, `apache/`, `ufl/`), writes one
row per font -- path, axis count (0 when static), whether GDEF has a store, and how many
VariationIndex devices GPOS holds outside and inside extension lookups -- and prints the
totals. A few minutes for the whole of Google Fonts.
"""

from __future__ import annotations

import sys
from pathlib import Path

from fontTools.ttLib import TTFont


def variation_devices(node, seen: set) -> int:
    """How many VariationIndex device tables (DeltaFormat 0x8000) sit under `node`."""
    if id(node) in seen:
        return 0
    seen.add(id(node))
    if getattr(node, "DeltaFormat", None) == 0x8000:
        return 1
    if isinstance(node, (list, tuple)):
        children = node
    elif hasattr(node, "__dict__"):
        children = vars(node).values()
    else:
        return 0
    return sum(variation_devices(child, seen) for child in children
               if isinstance(child, (list, tuple)) or hasattr(child, "__dict__"))


def survey(path: Path) -> tuple:
    font = TTFont(path, lazy=True)
    axes = len(font["fvar"].axes) if "fvar" in font else 0
    store = "GDEF" in font and getattr(font["GDEF"].table, "VarStore", None) is not None
    outside = inside = 0
    if axes and store and "GPOS" in font:
        for lookup in font["GPOS"].table.LookupList.Lookup:
            found = variation_devices(lookup, set())
            if lookup.LookupType == 9:
                inside += found
            else:
                outside += found
    return axes, int(store), outside, inside


def main() -> int:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    root, out = Path(sys.argv[1]), Path(sys.argv[2])
    rows, unreadable = [], 0
    for path in sorted(root.glob("*/*/*.ttf")):
        try:
            rows.append((path, *survey(path)))
        except Exception as e:  # a malformed font is a data point, not a reason to stop
            print(f"unreadable: {path}: {e}", file=sys.stderr)
            unreadable += 1
    with out.open("w") as handle:
        for row in rows:
            handle.write("\t".join(map(str, row)) + "\n")

    variable = [r for r in rows if r[1] > 0]
    with_store = [r for r in variable if r[2]]
    in_extensions = [r for r in variable if r[4] > 0]
    print(f"{len(variable)} variable fonts; {len(with_store)} with a GDEF item variation "
          f"store; {len(in_extensions)} address it from inside an extension lookup; "
          f"{unreadable} unreadable")
    return 0


if __name__ == "__main__":
    sys.exit(main())
