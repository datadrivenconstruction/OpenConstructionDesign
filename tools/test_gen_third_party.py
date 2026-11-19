# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
import os, sys, unittest
sys.path.insert(0, os.path.dirname(__file__))
import gen_third_party as g


class Allowlist(unittest.TestCase):
    def test_permissive_expressions_pass(self):
        for e in ["MIT", "MIT OR Apache-2.0", "Apache-2.0 WITH LLVM-exception", "MIT/Apache-2.0",
                  "(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0", "Unicode-3.0", "Zlib OR MIT"]:
            self.assertTrue(g.ok(e)[0], e)

    def test_copyleft_fails(self):
        for e in ["GPL-3.0", "MIT AND LGPL-2.1", "AGPL-3.0-only", "SSPL-1.0", "CC-BY-NC-4.0", "BUSL-1.1"]:
            self.assertFalse(g.ok(e)[0], e)

    def test_or_needs_one_allowed_branch(self):
        self.assertTrue(g.ok("MIT OR Apache-2.0 OR LGPL-2.1-or-later")[0])
        self.assertFalse(g.ok("GPL-2.0 OR LGPL-2.1")[0])

    def test_mpl_is_allowed_but_flagged(self):
        good, fl = g.ok("MPL-2.0")
        self.assertTrue(good)
        self.assertEqual(fl, {"MPL-2.0"})

    def test_lockfile_set_is_clean_and_listing_fresh(self):
        lst, full, bad, _ = g.build()
        self.assertEqual(bad, [])
        self.assertEqual(open(g.OUT_LIST, encoding="utf-8").read().replace("\r\n", "\n"), lst)
        self.assertIn("SIL OPEN FONT LICENSE", full)


if __name__ == "__main__":
    unittest.main()
