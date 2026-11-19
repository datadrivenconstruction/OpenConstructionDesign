# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Generate crates/ui/data/commands.json + ru command labels from docs/design/SHELL_AND_COMMANDS.md §5.
usage: python tools/gen_commands.py   (EN labels live in EN below; RU comes from the doc)"""
import json, pathlib, re

ROOT = pathlib.Path(__file__).resolve().parents[1]
DOC = (ROOT / "docs/design/SHELL_AND_COMMANDS.md").read_text(encoding="utf-8")
TABS = {"arch": "arch", "str": "struct", "mep": "systems", "ann": "annotate",
        "mod": "modify", "view": "view", "mgr": "manage", "data": "data", "ai": "data"}
ICONMAP = {"dim": "dimension", "trim": "trim-extend", "plan": "plan-view", "cube": "3d-view"}
EN = json.loads((ROOT / "tools/commands_en.json").read_text(encoding="utf-8"))

cmds, ru = [], {}
sec = re.split(r"^## 5\. ", DOC, flags=re.M)[1].split("### 5.9")[0]
for line in sec.splitlines():
    c = [x.strip() for x in line.strip().strip("|").split("|")]
    if len(c) != 7 or not re.match(r"^[a-z]+\.[a-z0-9_]+$", c[1]):
        continue
    label, cid, icon, mn, ctrl, where, stage = c
    cmds.append({"id": cid, "icon": ICONMAP.get(icon, icon), "mnemonic": None if mn == "—" else mn,
                 "shortcuts": [] if ctrl == "—" else [ctrl], "tab": TABS[cid.split(".")[0]],
                 "surfaces": where.split(), "stage": stage})
    ru[cid] = label

# §5.9 universal commands: expanded by hand from the table (multi-id rows); ru labels in tools/data/commands_ru.json.
UNIV = [
        ("app.palette", ["Ctrl+K", "Ctrl+Shift+P"]),
        ("app.where_is", ["Ctrl+Shift+K"]),
        ("file.new", ["Ctrl+N"]),
        ("file.open", ["Ctrl+O"]),
        ("file.save", ["Ctrl+S"]),
        ("file.save_as", ["Ctrl+Shift+S"]),
        ("edit.undo", ["Ctrl+Z"]),
        ("edit.redo", ["Ctrl+Y", "Ctrl+Shift+Z"]),
        ("edit.cut", ["Ctrl+X"]),
        ("edit.copy", ["Ctrl+C"]),
        ("edit.paste", ["Ctrl+V"]),
        ("edit.paste_aligned", ["Ctrl+Shift+V"]),
        ("edit.select_all", ["Ctrl+A"]),
        ("app.find", ["Ctrl+F"]),
        ("view.goto", ["Ctrl+P"]),
        ("view.close", ["Ctrl+W"]),
        ("view.next", ["Ctrl+Tab"]),
        ("view.plan_here", ["Ctrl+1"]),
        ("view.section_here", ["Ctrl+2"]),
        ("app.history", ["Ctrl+Shift+H"]),
        ("panel.browser", ["Ctrl+Alt+B"]),
        ("panel.properties", ["Ctrl+Alt+P"]),
        ("panel.issues", ["Ctrl+Alt+I"]),
        ("ui.layout.set.1", ["Ctrl+Alt+1"]),
        ("ui.layout.set.2", ["Ctrl+Alt+2"]),
        ("ui.layout.set.3", ["Ctrl+Alt+3"]),
        ("ui.layout.set.4", ["Ctrl+Alt+4"]),
        ("ui.layout.set.5", ["Ctrl+Alt+5"]),
        ("ui.layout.next", ["Ctrl+Alt+L"]),
        ("ui.layout.prev", ["Ctrl+Alt+Shift+L"]),
        ("ui.layout.settings", []),
        ("app.help", ["F1"]),
        ("app.repeat", []),
    ]
RU_UNIV = json.loads((ROOT / "tools/data/commands_ru.json").read_text(encoding="utf-8"))
ICON = {"edit.undo": "undo", "edit.redo": "redo", "edit.copy": "copy", "file.open": "open"}
for cid, sc in UNIV:
    cmds.append({"id": cid, "icon": ICON.get(cid, ""), "mnemonic": None, "shortcuts": sc, "tab": None,
                 "surfaces": ["A"], "stage": "M1"})
    ru[cid] = RU_UNIV[cid]

missing = [c["id"] for c in cmds if c["id"] not in EN]
assert not missing, f"EN labels missing: {missing}"
out = ROOT / "crates/ui/data"
(out / "commands.json").write_text(json.dumps(cmds, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
for lang, src in (("en", EN), ("ru", ru)):
    p = out / f"i18n/commands.{lang}.json"
    p.write_text(json.dumps({f"cmd.{k}": src[k] for k in sorted(src)}, ensure_ascii=False, indent=1) + "\n",
                 encoding="utf-8")
print(len(cmds), "commands")
