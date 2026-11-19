# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
# Bakes tabular figures (tnum) into cmap and subsets to Latin+Cyrillic+symbols to keep size small.
import sys
from fontTools.ttLib import TTFont
from fontTools import subset
src, dst, tab = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
f = TTFont(src)
if tab:
    gsub = f["GSUB"].table
    subs = {}
    for fr in gsub.FeatureList.FeatureRecord:
        if fr.FeatureTag == "tnum":
            for li in fr.Feature.LookupListIndex:
                for st in gsub.LookupList.Lookup[li].SubTable:
                    if hasattr(st, "mapping"): subs.update(st.mapping)
    # tabular widths for figures and number signs only: a tabular hyphen, bracket or
    # asterisk reads as spaced text ("Built - in")
    cm = f.getBestCmap()
    keep = [*range(0x30, 0x3A), *map(ord, ".,+<=>~"), 0xB1, 0xD7, 0xF7, 0x2212, 0x2248, 0x2260, 0x2264, 0x2265]
    subs = {g: t for g, t in subs.items() if g in {cm[c] for c in keep if c in cm}}
    for t in f["cmap"].tables:
        for cp, g in list(t.cmap.items()):
            if g in subs: t.cmap[cp] = subs[g]
    print("tnum mapped", len(subs))
opts = subset.Options(); opts.layout_features = ["kern","liga","calt","ccmp","locl","mark","mkmk","case","ss01"] ; opts.name_IDs=["*"]; opts.notdef_outline=True
s = subset.Subsetter(opts)
uni = list(range(0x20,0x7F))+list(range(0xA0,0x250))+list(range(0x370,0x3FF))+list(range(0x400,0x530))+list(range(0x2000,0x2070))+list(range(0x2070,0x20D0))+list(range(0x2100,0x2200))+list(range(0x2190,0x2400))+list(range(0x25A0,0x2600))
s.populate(unicodes=uni); s.subset(f)
# OFL: a modified font must not keep the original family name. Output is "DDC Sans" (derived from Inter).
style = {"Regular": "Regular", "Medium": "Medium", "SemiBold": "SemiBold"}[sys.argv[4]] if len(sys.argv) > 4 else None
if style:
    fam = "DDC Sans" if style == "Regular" else f"DDC Sans {style}"
    full = f"DDC Sans {style}"
    for n in list(f["name"].names):
        if n.nameID in (16, 17, 21, 22, 25):
            f["name"].names.remove(n)
    f["name"].setName(fam, 1, 3, 1, 0x409); f["name"].setName("Regular", 2, 3, 1, 0x409)
    f["name"].setName(f"DDC Sans {style};derived from Inter", 3, 3, 1, 0x409)
    f["name"].setName(full, 4, 3, 1, 0x409); f["name"].setName(f"DDCSans-{style}", 6, 3, 1, 0x409)
    f["name"].setName("Derived from Inter (c) 2016 The Inter Project Authors, SIL OFL 1.1", 10, 3, 1, 0x409)
    if style != "Regular":
        f["name"].setName("DDC Sans", 16, 3, 1, 0x409); f["name"].setName(style, 17, 3, 1, 0x409)
    f["name"].names = [n for n in f["name"].names if n.platformID == 3]
f.save(dst)
