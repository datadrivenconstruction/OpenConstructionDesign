"""Tests of tools/validate_ifc.py: python -m unittest discover -s tools -p "test_*.py"."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import validate_ifc as v  # noqa: E402

G = ["0" * 21 + c for c in "ABCDEFGHIJ"]


def sample(stair_args="$,#12,$,$,.STRAIGHT_RUN_STAIR.", tris="(1,2,3),(1,3,4)", extra=""):
    return f"""ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [x; y]'),'2;1');
FILE_NAME('a.ifc','2026-01-01T00:00:00',(''),(''),'x','x','');
FILE_SCHEMA(('IFC4X3_ADD2'));
ENDSEC;
DATA;
/* a comment; with a semicolon */
#1=IFCCARTESIANPOINT((0.,0.,0.));
#2=IFCPROJECT('{G[5]}',$,'P',$,$,$,$,$,$);
#3=IFCBUILDINGSTOREY('{G[6]}',$,'L1 ''q''',$,$,#12,$,$,.ELEMENT.,0.);
#10=IFCAXIS2PLACEMENT3D(#1,$,$);
#12=IFCLOCALPLACEMENT($,#10);
#13=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1.,0.,0.),(1.,1.,0.),(0.,1.,0.)),$);
#14=IFCTRIANGULATEDFACESET(#13,$,.T.,({tris}),$);
#15=IFCSHAPEREPRESENTATION($,'Body','Tessellation',(#14));
#20=IFCPRODUCTDEFINITIONSHAPE($,$,(#15));
#30=IFCSTAIR('{G[0]}',$,'Stair 1',$,{stair_args});
#31=IFCSTAIRFLIGHT('{G[1]}',$,'Flight',$,$,#12,#20,$,5,4,0.17,0.28,.STRAIGHT.);
#32=IFCRELAGGREGATES('{G[2]}',$,'',$,#30,(#31));
#33=IFCRELCONTAINEDINSPATIALSTRUCTURE('{G[7]}',$,$,$,(#30),#3);
{extra}ENDSEC;
END-ISO-10303-21;
"""


def run(text, **kw):
    with tempfile.NamedTemporaryFile("w", suffix=".ifc", delete=False, encoding="utf-8") as f:
        f.write(text)
    try:
        return v.run(f.name, kw.get("expected"), kw.get("strict", False), False)
    finally:
        os.unlink(f.name)


class Validate(unittest.TestCase):
    def test_a_good_file_is_clean_and_counted(self):
        r = run(sample())
        self.assertEqual(r["errors"], [])
        self.assertEqual(r["coverage"]["IFCSTAIRFLIGHT"]["shape_ok"], 1)
        self.assertEqual(r["coverage"]["IFCSTAIR"]["by_parts"], 1)

    def test_a_dangling_reference_is_found(self):
        r = run(sample(extra="#40=IFCRELAGGREGATES('%s',$,'',$,#30,(#99));\n" % G[3]))
        self.assertTrue(any(e.startswith("E002") and "#99" in e for e in r["errors"]))

    def test_duplicate_ids_and_global_ids_are_found(self):
        r = run(sample(extra="#31=IFCCARTESIANPOINT((0.,0.,0.));\n"))
        self.assertTrue(any(e.startswith("E001") for e in r["errors"]))
        r = run(sample(extra="#41=IFCRELAGGREGATES('%s',$,'',$,#30,(#31));\n" % G[2]))
        self.assertTrue(any(e.startswith("E004") for e in r["errors"]))
        r = run(sample().replace(G[0], "short"))
        self.assertTrue(any(e.startswith("E003") for e in r["errors"]))

    def test_wrong_attribute_count_of_a_pinned_class_is_found(self):
        r = run(sample().replace(",5,4,0.17,0.28,.STRAIGHT.", ",.STRAIGHT."))
        self.assertTrue(any(e.startswith("E005") and "IFCSTAIRFLIGHT" in e for e in r["errors"]))

    def test_a_bad_triangle_is_a_shape_error(self):
        for tris, why in [("(1,2,9)", "out of range"), ("(1,1,2)", "collapsed")]:
            r = run(sample(tris=tris))
            self.assertTrue(any(e.startswith("E006") and why in e for e in r["errors"]), tris)

    def test_a_product_without_shape_or_parts_is_a_warning_and_strict_makes_it_an_error(self):
        extra = (
            "#50=IFCRAILING('%s',$,'R',$,$,#12,$,$,.BALUSTRADE.);\n"
            "#51=IFCRELCONTAINEDINSPATIALSTRUCTURE('%s',$,$,$,(#50),#3);\n" % (G[4], G[8])
        )
        r = run(sample(extra=extra))
        self.assertEqual(r["errors"], [])
        self.assertTrue(any(w.startswith("W001") and "IFCRAILING" in w for w in r["warnings"]))
        self.assertTrue(run(sample(extra=extra), strict=True)["errors"])

    def test_expected_counts_name_every_difference(self):
        r = run(sample(), expected={"IFCSTAIR": 1, "IFCSTAIRFLIGHT": 2, "IFCRAMP": 1})
        got = [e for e in r["errors"] if e.startswith("C001")]
        self.assertEqual(len(got), 2)
        self.assertTrue(any("IFCSTAIRFLIGHT" in e for e in got) and any("IFCRAMP" in e for e in got))

    def test_a_cut_off_file_and_garbage_are_reported(self):
        r = run(sample().replace("END-ISO-10303-21;", ""))
        self.assertTrue(any(e.startswith("S003") for e in r["errors"]))
        r = run(sample(extra="#60=IFCWALL('x',;\n"))
        self.assertTrue(any(e.startswith("S005") or e.startswith("S004") for e in r["errors"]))

    def test_strings_with_quotes_and_semicolons_parse(self):
        r = run(sample(extra="#70=IFCPROPERTYSINGLEVALUE('a;b',$,IFCTEXT('it''s'),$);\n"))
        self.assertEqual(r["errors"], [])

    def test_the_external_layer_is_skipped_without_a_toolkit(self):
        os.environ.pop("DDC_IFC_TOOLKIT", None)
        self.assertEqual(run(sample())["external"], "skipped")
        self.assertEqual(v.external("x.ifc", True)[0], "failed")


    def test_storey_has_ten_attributes(self):
        r = run(sample().replace("#12,$,$,.ELEMENT.", "#12,$,$,$,.ELEMENT."))
        self.assertTrue(any(e.startswith("E005") and "IFCBUILDINGSTOREY: 11" in e for e in r["errors"]))

    def test_flight_risers_are_integers_and_heights_reals(self):
        def errs(a, b):
            return [e for e in run(sample().replace(a, b))["errors"] if e.startswith("E012")]
        self.assertTrue(any("NumberOfRisers must be an integer" in e for e in errs(",5,4,0.17", ",5.,4,0.17")))
        self.assertTrue(any("RiserHeight must be a real" in e for e in errs("0.17,0.28", "17,0.28")))
        self.assertTrue(any("NumberOfTreads must be an integer" in e for e in errs(",5,4,0.17", ",5,IFCINTEGER(4),0.17")))
        self.assertEqual(errs("x", "x"), [])

    def test_older_schema_counts(self):
        r = run(sample().replace("IFC4X3_ADD2", "IFC2X3"))
        self.assertTrue(any(e.startswith("E005") and "expected 12" in e for e in r["errors"]))
        self.assertTrue(any(e.startswith("E011") and "IFCTRIANGULATEDFACESET" in e for e in r["errors"]))

    def test_exactly_one_project(self):
        two = sample(extra="#80=IFCPROJECT('%s',$,'Q',$,$,$,$,$,$);\n" % G[9])
        self.assertTrue(any(e.startswith("E008") for e in run(two)["errors"]))
        none = sample().replace("IFCPROJECT(", "IFCPROJECTLIBRARY(")
        self.assertTrue(any(e.startswith("E008") for e in run(none)["errors"]))

    def test_an_uncontained_element_or_orphan_part_is_found(self):
        r = run(sample().replace("(#30),#3", "(#1),#3"))
        self.assertTrue(any(e.startswith("E009") and "#30 IFCSTAIR" in e for e in r["errors"]))
        r = run(sample().replace("#30,(#31)", "#30,(#1)"))
        self.assertTrue(any(e.startswith("E009") and "#31 IFCSTAIRFLIGHT" in e for e in r["errors"]))

    def test_the_container_must_be_spatial(self):
        r = run(sample().replace("(#30),#3", "(#30),#12"))
        self.assertTrue(any(e.startswith("E010") for e in r["errors"]))

    def test_header_problems(self):
        r = run(sample().replace("FILE_NAME(", "FILE_NOME("))
        self.assertTrue(any(e.startswith("S007") for e in r["errors"]))
        r = run(sample().replace("IFC4X3_ADD2", "AP214"))
        self.assertTrue(any(e.startswith("S006") for e in r["errors"]))
        self.assertTrue(run("DATA;\nENDSEC;\n")["errors"])

    def test_entity_syntax(self):
        for bad in [
            sample().replace("#1=IFCCARTESIANPOINT((0.,0.,0.));", "#1 IFCCARTESIANPOINT;"),
            sample().replace("#1=IFCCARTESIANPOINT((0.,0.,0.))", "#1=IFCCARTESIANPOINT((0.,0.,0.)"),
            sample().replace("'Tessellation'", "'Tessellation"),
        ]:
            self.assertTrue(run(bad)["errors"])

    def test_command_line(self):
        with tempfile.TemporaryDirectory() as d:
            ok, ko = os.path.join(d, "ok.ifc"), os.path.join(d, "ko.ifc")
            with open(ok, "w", encoding="utf-8") as f:
                f.write(sample())
            with open(ko, "w", encoding="utf-8") as f:
                f.write(sample().replace("(#31))", "(#99))"))
            self.assertEqual(v.main([ok]), 0)
            self.assertEqual(v.main([ko]), 1)
            self.assertEqual(v.main([os.path.join(d, "none.ifc")]), 2)


if __name__ == "__main__":
    unittest.main()
