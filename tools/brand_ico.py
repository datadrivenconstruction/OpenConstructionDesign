# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Pack data/brand/png/design-appicon-*.png into design-appicon.ico (PNG-compressed entries)."""
import struct, pathlib
d = pathlib.Path(__file__).resolve().parent.parent / "crates/app/data/brand"
sizes = [16, 24, 32, 48, 64, 128, 256]
pngs = [(s, (d / f"png/design-appicon-{s}.png").read_bytes()) for s in sizes]
out = struct.pack("<HHH", 0, 1, len(pngs))
off = 6 + 16 * len(pngs)
for s, b in pngs:
    out += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(b), off)
    off += len(b)
(d / "design-appicon.ico").write_bytes(out + b"".join(b for _, b in pngs))
