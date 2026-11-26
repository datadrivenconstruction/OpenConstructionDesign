# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Bundle entry ddc-editor.js against the ERP bridge ddc-web-bridge/1: every message it posts passes
the contract schema, and it behaves as the host expects (python -m unittest tools/test_web_entry.py).
Needs node; the wasm module and the page are stand-ins (tools/web/entry_harness.mjs)."""
import json
import shutil
import subprocess
import sys
import unittest
from pathlib import Path

WEB = Path(__file__).resolve().parent / "web"
sys.path.insert(0, str(WEB))
import bundle_manifest as bm  # noqa: E402

SCHEMA = json.loads((WEB / "web-bridge.schema.json").read_text(encoding="utf-8"))


def errors(msg):
    shape = {k: ("" if isinstance(v, str) and v.endswith(" bytes>") else v) for k, v in msg.items()}
    return bm.schema_errors(shape, {"$ref": f"#/$defs/{msg['type']}"}, "", SCHEMA)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class WebEntry(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        out = subprocess.run(["node", str(WEB / "entry_harness.mjs"), str(WEB / "entry.js")],
                             capture_output=True, text=True, timeout=60, check=True).stdout
        cls.runs = {r["name"]: r for r in json.loads(out)}

    def types(self, name):
        return [m["type"] for m in self.runs[name]["posted"]]

    def calls(self, name, what):
        return [c[1:] for c in self.runs[name]["calls"] if c[0] == what]

    def test_every_posted_message_passes_the_bridge_schema(self):
        for name, r in self.runs.items():
            for m in r["posted"]:
                self.assertEqual(errors(m), [], f"{name}: {m}")

    def test_ready_first_then_the_model_only_through_read_file(self):
        for name in self.runs:
            self.assertEqual(self.types(name)[0], "ready")
        files = [m["path"] for m in self.runs["edit"]["posted"] if m["type"] == "read_file"]
        self.assertEqual(files, ["model.ddc", "elements.parquet", "draft.ddcrev"], "the tip, its rows, then the person's draft")
        self.assertEqual(self.calls("edit", "set_parent_rows"), [["9c1d2e3f-4a5b-4c6d-8e7f-0a1b2c3d4e5f", 40]])
        self.assertEqual(self.calls("view", "set_parent_rows"), [["9c1d2e3f-4a5b-4c6d-8e7f-0a1b2c3d4e5f", 40]], "classes and types in view mode too")
        self.assertEqual(self.calls("edit", "load_package"), [[30]])
        self.assertEqual(self.calls("view", "load_model"), [[10]])
        self.assertEqual(self.calls("edit", "init"), [["ddc_web_editor_bg.wasm"]], "wasm beside the entry")

    def test_an_edit_makes_dirty_then_one_autosave_then_clean(self):
        r = self.runs["edit"]
        p = r["posted"]
        self.assertEqual([m["type"] for m in p[r["dirtyAt"] - 1:r["dirtyAt"] + 2]], ["dirty", "save", "dirty"])
        self.assertEqual([m["dirty"] for m in p if m["type"] == "dirty"], [True, False])
        save = next(m for m in p if m["type"] == "save")
        self.assertEqual((save["package"], save["package_bytes"], save["package_sha256"]), ("<100 bytes>", 100, r["digest"]))
        # the draft keeps its id; the tip's rows come from its elements.parquet, so no model rows
        self.assertEqual(self.calls("edit", "save_package")[0][1], "5b0c7e21-9d3f-4a8e-b6c1-2f4d8a9e0c13")
        self.assertEqual(self.calls("edit", "set_parent_model"), [])
        # a tip without elements.parquet: its rows are computed from the model before the package
        names = [c[0] for c in self.runs["edit_no_rows"]["calls"]]
        self.assertLess(names.index("set_parent_model"), names.index("save_package"))

    def test_view_narrow_and_refused_never_save(self):
        for name in ("view", "narrow", "refused"):
            self.assertNotIn("save", self.types(name), name)
            self.assertNotIn("dirty", self.types(name), name)
            self.assertNotIn([False], self.calls(name, "set_view_only"), f"{name} stays view-only")
        # wide, then narrowed: back to view and no more saves
        r = self.runs["edit"]
        self.assertEqual(self.calls("edit", "set_view_only")[-1], [True])
        self.assertEqual(len(r["posted"]), r["afterNarrow"])
        self.assertNotIn("save", [m["type"] for m in r["posted"][r["afterNarrow"]:]])

    def test_selection_goes_out_as_element_keys_and_comes_in_highlighted(self):
        sel = next(m for m in self.runs["edit"]["posted"] if m["type"] == "select")
        # the editor's elements go out as they are: class and type when its rows know the key
        self.assertEqual(sel["elements"], [{"element_key": "b1/s01/wall", "ifc_class": "IfcWall", "type_key": "wall/brick"}, {"element_key": "x/slab/0f"}])
        self.assertEqual(self.calls("edit", "select_keys"), [['["b1/s02/wall"]']])
        self.assertEqual(self.calls("edit", "set_locale")[-1], ["ru"])
        self.assertEqual(self.calls("edit", "set_theme")[-1], ["dark"])

    def test_ai_goes_through_the_erp_only(self):
        ask = [m for m in self.runs["edit"]["posted"] if m["type"] == "ai_request"]
        self.assertEqual([(m["action"], m["text"]) for m in ask], [("brief_from_text", "Fuenf Geschosse, Mauerwerk")])
        reply = self.runs["edit"]["reply"]
        self.assertEqual(reply["bridge"], "ddc-web-bridge/1")
        self.assertIn("brief version 4", reply["text"])
        # a confirmed brief is drafted the desktop assistant's way: a tool call, then preview and Apply
        self.assertEqual(reply["finish"], "tool_calls")
        self.assertEqual([c["name"] for c in reply["tool_calls"]], ["generate_project"])
        self.assertEqual(self.calls("edit", "brief_tool_call"), [[None]], "built by the editor from the ERP brief")
        after = self.runs["edit"]["after"]
        self.assertEqual((after["finish"], after["tool_calls"]), ("stop", []))
        self.assertIn("Apply", after["text"])
        self.assertEqual(self.runs["edit"]["asks"], 1, "the tool result does not ask the ERP again")

    def test_refused_shows_the_note_and_ignores_open(self):
        r = self.runs["refused"]
        self.assertEqual(self.types("refused"), ["ready"])
        self.assertIn("alert", [c["role"] for c in r["el"]])

    def test_unmount_leaves_no_canvas_listener_or_editor(self):
        for name, r in self.runs.items():
            self.assertTrue(all(c["removed"] for c in r["el"]), name)
            self.assertEqual(r["listeners"], 0, name)
            self.assertEqual([c[0] for c in r["calls"][-2:]], ["destroy", "free"], name)

    def test_on_demand_files_are_added_only_when_the_manifest_vouches(self):
        added = [c[1:] for c in self.runs["assets"]["calls"] if c[0] == "add_asset"]
        self.assertEqual(added, [["i18n/de.json", 9]], "altered font and unlisted files are dropped")

    def test_the_vendored_bridge_schema_is_the_contract_one(self):
        contract = Path(__file__).resolve().parents[1] / "crates" / "io" / "contract" / "schemas" / "web-bridge.schema.json"
        self.assertEqual(json.loads(contract.read_text(encoding="utf-8")), SCHEMA)
        for m in json.loads((WEB / "web-bridge.example.json").read_text(encoding="utf-8")):
            self.assertEqual(errors(m), [], m["type"])


if __name__ == "__main__":
    unittest.main()
