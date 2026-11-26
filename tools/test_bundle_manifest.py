# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Tests for web/bundle_manifest.py: python -m unittest tools/test_bundle_manifest.py (or pytest -q)."""
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "web"))
import bundle_manifest as bm  # noqa: E402

WEB = Path(__file__).resolve().parent / "web"


class BundleManifest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dist = Path(self.tmp.name)
        (self.dist / "ddc-editor.js").write_text("export async function mount() {}\n")
        (self.dist / "ddc_web_editor.js").write_text("// glue\n")
        (self.dist / "ddc_web_editor_bg.wasm").write_bytes(b"\0asm\1\0\0\0" + bytes(100))
        (self.dist / "snippets" / "ddc-app-0").mkdir(parents=True)
        (self.dist / "snippets" / "ddc-app-0" / "inline0.js").write_text("export {}\n")
        self.schema = json.loads(bm.SCHEMA.read_text(encoding="utf-8"))

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, **kw):
        self.assertEqual(bm.main(["write", str(self.dist), "--version", "0.0.1",
                                  "--entry", kw.get("entry", "ddc-editor.js")]), kw.get("code", 0))
        return json.loads((self.dist / bm.MANIFEST).read_text(encoding="utf-8"))

    def test_written_manifest_passes_the_schema_and_lists_every_file(self):
        doc = self.write()
        self.assertEqual(bm.schema_errors(doc, self.schema), [])
        self.assertEqual(bm.check(self.dist), [])
        self.assertEqual({f["path"] for f in doc["files"]},
                         {"ddc-editor.js", "ddc_web_editor.js", "ddc_web_editor_bg.wasm",
                          "snippets/ddc-app-0/inline0.js"})
        wasm = next(f for f in doc["files"] if f["path"].endswith(".wasm"))
        self.assertEqual(wasm["bytes"], 108)
        self.assertEqual(doc["bridge"], [1])

    def test_assets_merge_locale_fragments_and_are_marked_on_demand(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        ui = Path(tmp.name)
        for code, frags in {"en": {"a.json": {"k": "en"}}, "ru": {"a.json": {"k": "ru"}},
                            "de": {"a.json": {"k": "a", "x": "1"}, "b.json": {"k": "b"}}, "fr": {}}.items():
            (ui / "data/i18n" / code).mkdir(parents=True)
            for name, body in frags.items():
                (ui / "data/i18n" / code / name).write_text(json.dumps(body), encoding="utf-8")
        (ui / "assets/fonts/i18n").mkdir(parents=True)
        for f in ["NotoSansSC.ttf", "NotoEmoji.ttf"]:
            (ui / "assets/fonts/i18n" / f).write_bytes(b"ttf")
        self.assertEqual(bm.assets(self.dist, ui), ["i18n/de.json", "fonts/NotoSansSC.ttf"])
        self.assertEqual(json.loads((self.dist / "i18n/de.json").read_text(encoding="utf-8")), {"k": "b", "x": "1"})
        doc = self.write()
        load = {f["path"]: f.get("load") for f in doc["files"]}
        self.assertEqual((load["i18n/de.json"], load["fonts/NotoSansSC.ttf"], load["ddc_web_editor_bg.wasm"]),
                         ("on_demand", "on_demand", None))

    def test_built_in_fonts_are_the_ones_the_browser_build_embeds(self):
        src = (WEB.parents[1] / "crates/ui/src/i18n_fonts.rs").read_text(encoding="utf-8")
        cfg = src.split('#[cfg(feature = "external-assets")]\n    fn embedded')[1].split("_ =>")[0]
        self.assertEqual(set(re.findall(r'fonts/i18n/([^"]+)"', cfg)), bm.BUILT_IN_FONTS)

    def test_the_contract_example_passes_and_broken_ones_do_not(self):
        ex = json.loads((WEB / "web-editor.example.json").read_text(encoding="utf-8"))
        self.assertEqual(bm.schema_errors(ex, self.schema), [])
        for bad in (
            dict(ex, schema="ddc-web-editor/2"),
            dict(ex, entry="../evil.js"),
            dict(ex, entry="ddc-editor.css"),
            dict(ex, bridge=[]),
            dict(ex, extra=1),
            dict(ex, files=[dict(ex["files"][0], sha256="ABC")]),
            dict(ex, files=[dict(ex["files"][0], path="a/../b.js")]),
            dict(ex, files=[dict(ex["files"][0], bytes=-1)]),
            {k: v for k, v in ex.items() if k != "version"},
        ):
            self.assertNotEqual(bm.schema_errors(bad, self.schema), [], bad)

    def test_a_changed_missing_or_unlisted_file_fails_the_check(self):
        self.write()
        wasm = self.dist / "ddc_web_editor_bg.wasm"
        data = wasm.read_bytes()
        wasm.write_bytes(data[:-1] + b"\1")
        self.assertTrue(any("SHA-256" in p for p in bm.check(self.dist)))
        wasm.write_bytes(data + b"\0")
        self.assertTrue(any("bytes" in p for p in bm.check(self.dist)))
        wasm.write_bytes(data)
        self.assertEqual(bm.check(self.dist), [])
        (self.dist / "stray.js").write_text("x")
        self.assertTrue(any("stray.js" in p for p in bm.check(self.dist)))
        (self.dist / "stray.js").unlink()
        wasm.unlink()
        self.assertTrue(any("missing" in p for p in bm.check(self.dist)))

    def test_an_entry_that_is_not_a_bundle_file_is_refused(self):
        self.write(entry="nope.js", code=1)
        self.assertTrue(any("/entry" in p for p in bm.check(self.dist)))

    def test_the_vendored_schemas_are_the_contract_ones(self):
        schemas = Path(__file__).resolve().parents[1] / "crates" / "io" / "contract" / "schemas"
        for name in ("web-editor.schema.json", "web-release.schema.json"):
            self.assertEqual(json.loads((schemas / name).read_text(encoding="utf-8")),
                             json.loads((WEB / name).read_text(encoding="utf-8")), name)

    def test_release_zip_holds_every_file_precompressed_and_the_feed_matches_it(self):
        try:
            import brotli
        except ImportError:
            self.skipTest("pip install brotli")
        import datetime, gzip, hashlib, zipfile
        self.write()
        out = self.dist / "rel"
        now = datetime.datetime(2026, 11, 20, 12, 0, tzinfo=datetime.timezone.utc)
        feed = bm.release(self.dist, "v0.0.1", out, now=now)
        z = out / "OpenConstructionDesign-v0.0.1-web.zip"
        self.assertEqual(feed["zip"], {"url": "https://github.com/datadrivenconstruction/OpenConstructionDesign"
                                       "/releases/download/v0.0.1/OpenConstructionDesign-v0.0.1-web.zip",
                                       "bytes": z.stat().st_size, "sha256": hashlib.sha256(z.read_bytes()).hexdigest()})
        self.assertEqual(feed["published_at"], "2026-11-20T12:00:00Z")
        self.assertTrue(feed["signature_url"].endswith("-web.zip.minisig"))
        self.assertEqual(json.loads((out / "latest-web.json").read_text(encoding="utf-8")), feed)
        with zipfile.ZipFile(z) as zf:
            names = set(zf.namelist())
            wasm = zf.read("ddc_web_editor_bg.wasm")
            self.assertEqual(brotli.decompress(zf.read("ddc_web_editor_bg.wasm.br")), wasm)
            self.assertEqual(gzip.decompress(zf.read("ddc_web_editor_bg.wasm.gz")), wasm)
        self.assertIn("manifest.json", names)
        self.assertIn("snippets/ddc-app-0/inline0.js.br", names)

    def test_release_refuses_a_tag_other_than_the_manifest_version(self):
        self.write()
        with self.assertRaises(ValueError):
            bm.release(self.dist, "v9.9.9", self.dist / "rel")


if __name__ == "__main__":
    unittest.main()
