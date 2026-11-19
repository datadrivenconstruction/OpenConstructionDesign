#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Builds the locale fallback fonts in crates/ui/assets/fonts/i18n/ from Noto sources (SIL OFL 1.1).

Usage:  python tools/build_i18n_fonts.py fetch [SRC_DIR] [NAME..]   download the pinned sources (~40 MB)
        python tools/build_i18n_fonts.py build [SRC_DIR] [NAME..]   instantiate Regular (wght 400), subset, write TTFs
Needs fonttools (MIT): pip install fonttools. SRC_DIR defaults to target/i18n_font_src (untracked).

Each output holds only the code points of its script (no layout tables: egui does no GSUB/GPOS).
CJK faces keep the frequent set of the national standard: GB 2312 (6763 hanzi, zh), JIS X 0208
(6355 kanji, ja), KS X 1001 hangul (2350 syllables, ko), plus kana, punctuation and fullwidth forms.
To change the set, edit CJK below and rerun `build`; commit the TTFs and the sizes in docs/I18N_LOCALES.md.
"""
import os
import sys
import urllib.request

REV = "a85815a42757630ce188fdad368c2dfc444d4773"  # google/fonts commit, same pin as fetch_cjk_font.ps1
RAW = f"https://github.com/google/fonts/raw/{REV}/ofl/"
ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
OUT = os.path.join(ROOT, "crates", "ui", "assets", "fonts", "i18n")

# out name, source path under ofl/, variation axes, code point ranges
R = lambda a, b: range(a, b + 1)
PUNCT = [*R(0x2000, 0x206F), 0xA0, 0xD7, 0xF7]
SPECS = {
    "NotoSansArabic": ("notosansarabic/NotoSansArabic[wdth,wght].ttf", {"wght": 400, "wdth": 100},
                       [*R(0x600, 0x6FF), *R(0x750, 0x77F), *R(0x8A0, 0x8FF), *R(0xFB50, 0xFDFF), *R(0xFE70, 0xFEFF), *PUNCT]),
    "NotoSansHebrew": ("notosanshebrew/NotoSansHebrew[wdth,wght].ttf", {"wght": 400, "wdth": 100},
                       [*R(0x590, 0x5FF), *R(0xFB1D, 0xFB4F), *PUNCT]),
    "NotoSansDevanagari": ("notosansdevanagari/NotoSansDevanagari[wdth,wght].ttf", {"wght": 400, "wdth": 100},
                           [*R(0x900, 0x97F), *R(0xA8E0, 0xA8FF), 0x25CC, *PUNCT]),
    "NotoSansBengali": ("notosansbengali/NotoSansBengali[wdth,wght].ttf", {"wght": 400, "wdth": 100},
                        [*R(0x980, 0x9FF), 0x964, 0x965, 0x25CC, *PUNCT]),
    "NotoSansThai": ("notosansthai/NotoSansThai[wdth,wght].ttf", {"wght": 400, "wdth": 100},
                     [*R(0xE00, 0xE7F), 0x25CC, *PUNCT]),
    # Vietnamese and Uzbek letters (U+1EA0-1EF9, U+02BB/02BC) that DDC Sans lacks
    "NotoSansLatinVi": ("notosans/NotoSans[wdth,wght].ttf", {"wght": 400, "wdth": 100},
                        [*R(0x100, 0x24F), *R(0x2B0, 0x2FF), *R(0x300, 0x36F), *R(0x1E00, 0x1EFF), *PUNCT]),
    # check marks, ballot boxes, geometric shapes, misc symbols and dingbats that DDC Sans lacks
    "NotoSansSymbols2": ("notosanssymbols2/NotoSansSymbols2-Regular.ttf", {},
                         [*R(0x25A0, 0x25FF), *R(0x2600, 0x26FF), *R(0x2700, 0x27BF)]),
    # arrows, math operators, technical signs (diameter, angle) and block elements
    "NotoSansMath": ("notosansmath/NotoSansMath-Regular.ttf", {},
                     [*R(0x2100, 0x214F), *R(0x2190, 0x21FF), *R(0x2200, 0x22FF), *R(0x2300, 0x23FF),
                      *R(0x27C0, 0x27FF), *R(0x2900, 0x29FF), *R(0x2B00, 0x2BFF)]),
    # box drawing and block elements (text caret U+258F)
    "NotoSansMono": ("notosansmono/NotoSansMono[wdth,wght].ttf", {"wght": 400, "wdth": 100},
                     [*R(0x2500, 0x259F)]),
    # monochrome emoji in user text (comments, names); our own fallback instead of the toolkit's
    "NotoEmoji": ("notoemoji/NotoEmoji[wght].ttf", {"wght": 400},
                  [0xA9, 0xAE, 0x200D, 0x203C, 0x2049, 0x20E3, 0x2122, 0x2139, *R(0x2194, 0x21AA),
                   *R(0x2300, 0x23FF), *R(0x2600, 0x27BF), *R(0x2B00, 0x2BFF), 0x3030, 0x303D, 0x3297,
                   0x3299, 0xFE0F, *R(0x1F000, 0x1FAFF)]),
}


def _codec_chars(codec, rows, cols=range(0xA1, 0xFF)):
    out = []
    for a in rows:
        for b in cols:
            try:
                out.append(ord(bytes([a, b]).decode(codec)))
            except UnicodeDecodeError:
                pass
    return out


CJK_COMMON = [*R(0x3000, 0x303F), *R(0x3040, 0x30FF), *R(0xFF01, 0xFF5E), *R(0xFF61, 0xFF9F), *R(0x2010, 0x2027),
              *R(0x2190, 0x2199), *R(0x25A0, 0x25CF), 0xB7, 0xD7, 0xF7]
CJK = {
    "NotoSansSC": ("notosanssc/NotoSansSC[wght].ttf", {"wght": 400},
                   [*_codec_chars("gb2312", R(0xA1, 0xF7)), *CJK_COMMON]),
    "NotoSansJP": ("notosansjp/NotoSansJP[wght].ttf", {"wght": 400},
                   [*_codec_chars("euc_jp", R(0xA1, 0xFE)), *CJK_COMMON]),
    "NotoSansKR": ("notosanskr/NotoSansKR[wght].ttf", {"wght": 400},
                   [*_codec_chars("euc_kr", R(0xB0, 0xC8)), *R(0x3130, 0x318F), *CJK_COMMON]),
}
SPECS.update(CJK)


def fetch(src, only=()):
    os.makedirs(src, exist_ok=True)
    for name, (path, _, _) in SPECS.items():
        if only and name not in only:
            continue
        dst = os.path.join(src, name + ".ttf")
        if not os.path.exists(dst):
            urllib.request.urlretrieve(RAW + path.replace("[", "%5B").replace("]", "%5D"), dst)
        print("ok", dst)


def build(src, only=()):
    from fontTools.ttLib import TTFont
    from fontTools.varLib import instancer
    from fontTools import subset

    os.makedirs(OUT, exist_ok=True)
    for name, (_, axes, cps) in SPECS.items():
        if only and name not in only:
            continue
        f = TTFont(os.path.join(src, name + ".ttf"))
        if "fvar" in f:
            instancer.instantiateVariableFont(f, axes, inplace=True)
        opt = subset.Options()
        opt.layout_features = []
        opt.hinting = False
        opt.notdef_outline = True
        opt.glyph_names = False
        opt.name_IDs = [0, 1, 2, 3, 4, 5, 6, 13, 14]
        opt.drop_tables += ["GSUB", "GPOS", "GDEF", "STAT", "MVAR", "HVAR", "avar", "fvar", "gvar", "DSIG", "kern"]
        sub = subset.Subsetter(opt)
        sub.populate(unicodes=sorted(set(cps)))
        sub.subset(f)
        dst = os.path.join(OUT, name + ".ttf")
        f.save(dst)
        kept = len(f.getBestCmap())
        print(f"{name}: {os.path.getsize(dst) / 1e6:.2f} MB, {kept} code points")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "build"
    src = sys.argv[2] if len(sys.argv) > 2 else os.path.join(ROOT, "target", "i18n_font_src")
    {"fetch": fetch, "build": build}[cmd](src, tuple(sys.argv[3:]))
