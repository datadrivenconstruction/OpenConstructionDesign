"""Tests of tools/dwg_kind_fixtures.py. Run: python tools/test_dwg_kind_fixtures.py"""
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import dwg_kind_fixtures as fx  # noqa: E402


def dxf(entities, blocks=()):
    def sec(name, recs):
        return ["0", "SECTION", "2", name] + [x for r in recs for x in ("0", r, "8", "0")] + ["0", "ENDSEC"]
    return "\n".join(sec("BLOCKS", blocks) + sec("ENTITIES", entities) + ["0", "EOF"]) + "\n"


def make(root, name, size, entities, blocks=()):
    d = Path(root) / name
    d.mkdir()
    (d / "input.dwg").write_bytes(b"x" * size)
    (d / "reference.dxf").write_text(dxf(entities, blocks), encoding="utf-8")
    return str(d / "input.dwg")


class Kinds(unittest.TestCase):
    def test_buckets(self):
        self.assertEqual(fx.kind_of("MLEADER"), "leader")
        self.assertEqual(fx.kind_of("ACAD_PROXY_ENTITY"), "proxy")
        self.assertIsNone(fx.kind_of("LINE"))

    def test_smallest_file_per_kind(self):
        with tempfile.TemporaryDirectory() as r:
            big = make(r, "a", 500, ["LINE", "SPLINE", "MTEXT"])
            small = make(r, "b", 100, ["SPLINE", "LINE"])
            blk = make(r, "c", 50, ["LINE"], blocks=["ELLIPSE"])
            best = fx.pick([big, small, blk])
            self.assertEqual(best["spline"], (100, small))
            self.assertEqual(best["mtext"], (500, big))
            self.assertEqual(best["ellipse"], (50, blk))
            self.assertNotIn("solid", best)

    def test_header_section_ignored(self):
        with tempfile.TemporaryDirectory() as r:
            d = Path(r)
            (d / "reference.dxf").write_text(
                "0\nSECTION\n2\nTABLES\n0\nSPLINE\n0\nENDSEC\n0\nEOF\n", encoding="utf-8")
            self.assertEqual(fx.kinds_in_dxf(d / "reference.dxf"), set())


if __name__ == "__main__":
    unittest.main()
