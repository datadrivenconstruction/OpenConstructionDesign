#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Generates crates/io/src/ifc/schema_table.rs from the EXPRESS schemas of buildingSMART.

    python tools/gen_ifc_schema_table.py IFC2X3_TC1.exp IFC4_ADD2_TC1.exp IFC4X3_ADD2.exp \
        > crates/io/src/ifc/schema_table.rs

The schema files are not part of the repository. The script reads, per entity, the explicit
attributes of the whole SUBTYPE OF chain (ancestors first), applies the DERIVE overrides
(a derived attribute of an ancestor is written as `*`) and redeclarations, and resolves every
attribute type to the small language below.

Attribute spec (the attributes of one entity in one schema are joined by `;`):
    [?]kind          `?` marks OPTIONAL
    kind: r entity reference | i integer | n real | u number | s string | y binary
          | e=A|B enumeration (BOOLEAN is e=F|T, LOGICAL e=F|T|U)
          | x select (any value) | l(kind) list, set, bag or array | * derived (no `?`)
"""
import re
import sys

META = [
    "IFC2X3 TC1: IFC/RELEASE/IFC2x3/TC1/EXPRESS/IFC2X3_TC1.exp",
    "IFC4 ADD2 TC1: IFC/RELEASE/IFC4/ADD2_TC1/EXPRESS/IFC4.exp",
    "IFC4X3 ADD2: IFC/DEV/IFC4_3/HTML/IFC4X3_ADD2_DEV_20260526.exp",
]
SOURCE = "https://github.com/buildingSMART/Standards-builds (commit 49101e4091ce)"
DATE = "2026-11-09"

# Classes the exporter writes (the schema_table test checks the written ones are all present).
WRITTEN = """IFCADVANCEDBREP IFCADVANCEDFACE IFCAPPLICATION IFCBOOLEANRESULT
IFCBSPLINECURVEWITHKNOTS IFCBSPLINESURFACEWITHKNOTS IFCEDGE IFCEDGECURVE IFCEDGELOOP
IFCORIENTEDEDGE IFCRATIONALBSPLINECURVEWITHKNOTS IFCRATIONALBSPLINESURFACEWITHKNOTS
IFCARBITRARYCLOSEDPROFILEDEF IFCARBITRARYPROFILEDEFWITHVOIDS
IFCAXIS1PLACEMENT IFCAXIS2PLACEMENT2D IFCAXIS2PLACEMENT3D IFCBEAM IFCBEAMTYPE
IFCBSPLINECURVEWITHKNOTS IFCFIXEDREFERENCESWEPTAREASOLID IFCRATIONALBSPLINECURVEWITHKNOTS
IFCEXTRUDEDAREASOLIDTAPERED IFCDERIVEDPROFILEDEF IFCCARTESIANTRANSFORMATIONOPERATOR2D
IFCPOLYGONALFACESET IFCINDEXEDPOLYGONALFACE IFCINDEXEDPOLYGONALFACEWITHVOIDS IFCFACEBOUND
IFCBOUNDARYNODECONDITION IFCBUILDING IFCBUILDINGELEMENTPROXY IFCBUILDINGSTOREY
IFCCABLECARRIERFITTING IFCCABLECARRIERSEGMENT IFCCARTESIANPOINT IFCCARTESIANPOINTLIST2D
IFCCARTESIANPOINTLIST3D IFCCARTESIANTRANSFORMATIONOPERATOR3D
IFCCARTESIANTRANSFORMATIONOPERATOR3DNONUNIFORM IFCCIRCLE IFCCIRCLEPROFILEDEF IFCCLOSEDSHELL
IFCCLASSIFICATION IFCCLASSIFICATIONREFERENCE IFCDISTRIBUTIONCIRCUIT IFCQUANTITYCOUNT IFCQUANTITYTIME
IFCCOLOURRGB IFCCOLUMN IFCCOLUMNTYPE IFCCONVERSIONBASEDUNIT IFCCOVERING IFCCURTAINWALL
IFCDIMENSIONALEXPONENTS IFCDIRECTION IFCDISCRETEACCESSORY IFCDISTRIBUTIONPORT IFCDISTRIBUTIONSYSTEM IFCDOOR
IFCDOORSTYLE IFCDOORTYPE IFCDUCTFITTING IFCDUCTSEGMENT IFCELEMENTASSEMBLY IFCELEMENTQUANTITY
IFCEXTRUDEDAREASOLID IFCFACE IFCFACEOUTERBOUND IFCFACESURFACE IFCFACETEDBREP IFCFLOWFITTING
IFCFLOWSEGMENT IFCFOOTING IFCGEOGRAPHICELEMENT IFCGEOMETRICCURVESET
IFCGEOMETRICREPRESENTATIONCONTEXT IFCGEOMETRICREPRESENTATIONSUBCONTEXT IFCGRID IFCGRIDAXIS
IFCINDEXEDPOLYCURVE IFCLOCALPLACEMENT IFCMAPCONVERSION IFCMAPPEDITEM IFCMATERIAL
IFCMATERIALDEFINITIONREPRESENTATION IFCMATERIALLAYER IFCMATERIALLAYERSET
IFCMATERIALLAYERSETUSAGE IFCMATERIALPROPERTIES IFCMEASUREWITHUNIT IFCMECHANICALFASTENER IFCMEMBER
IFCOPENINGELEMENT IFCOPENSHELL IFCORGANIZATION IFCOWNERHISTORY IFCPERSON
IFCPERSONANDORGANIZATION IFCPIPEFITTING IFCPIPESEGMENT IFCPLANE IFCPLATE IFCPOLYLINE
IFCPOLYLOOP IFCPOSTALADDRESS IFCPRESENTATIONSTYLEASSIGNMENT IFCPRODUCTDEFINITIONSHAPE
IFCPROJECT IFCPROJECTEDCRS IFCPROPERTYLISTVALUE IFCPROPERTYSET IFCPROPERTYSINGLEVALUE
IFCQUANTITYAREA IFCQUANTITYLENGTH IFCQUANTITYVOLUME IFCQUANTITYWEIGHT IFCRAILING IFCRAMP
IFCRAMPFLIGHT IFCRECTANGLEPROFILEDEF IFCREINFORCINGBAR IFCRELAGGREGATES
IFCRELASSIGNSTOGROUP IFCRELASSOCIATESMATERIAL IFCRELCONNECTSPORTS
IFCRELCONNECTSSTRUCTURALMEMBER IFCRELCONTAINEDINSPATIALSTRUCTURE IFCRELDEFINESBYPROPERTIES
IFCRELDEFINESBYTYPE IFCRELFILLSELEMENT IFCRELNESTS IFCRELSERVICESBUILDINGS
IFCRELSPACEBOUNDARY IFCRELVOIDSELEMENT IFCREPRESENTATIONMAP IFCREVOLVEDAREASOLID IFCROOF
IFCROOFTYPE IFCSHAPEREPRESENTATION IFCSHELLBASEDSURFACEMODEL IFCSITE IFCSIUNIT IFCSLAB
IFCSLABTYPE IFCSPACE IFCSTAIR IFCSTAIRFLIGHT IFCSTRUCTURALANALYSISMODEL
IFCSTRUCTURALCURVEMEMBER IFCSTRUCTURALPOINTCONNECTION IFCSTRUCTURALSURFACEMEMBER
IFCSTYLEDITEM IFCSTYLEDREPRESENTATION IFCSURFACESTYLE IFCSURFACESTYLERENDERING
IFCSWEPTDISKSOLID IFCSYSTEM IFCTOPOLOGYREPRESENTATION IFCTRANSPORTELEMENT
IFCTRIANGULATEDFACESET IFCTRIMMEDCURVE IFCUNITASSIGNMENT IFCVERTEXPOINT IFCVIRTUALELEMENT
IFCWALL IFCWALLTYPE
IFCWINDOW IFCWINDOWSTYLE IFCWINDOWTYPE IFCZONE""".split()
# reserve: every concrete subtype of these
FAMILIES = ["IfcBuildingElement", "IfcBuildingElementType", "IfcDistributionFlowElement",
            "IfcRelDefines", "IfcRelAssociates", "IfcRelConnects", "IfcRelDecomposes",
            "IfcRelAssigns", "IfcSpatialStructureElement", "IfcElementType", "IfcFurnishingElement"]


def strip(text):
    text = re.sub(r"\(\*.*?\*\)", " ", text, flags=re.S)
    return re.sub(r"--[^\n]*", " ", text)


def parse(path):
    text = strip(open(path, encoding="utf-8", errors="replace").read().replace(chr(13), ""))
    types, ents = {}, {}
    for m in re.finditer(r"\bTYPE\s+(\w+)\s*=\s*(.*?);\s*(?:WHERE.*?)?END_TYPE;", text, re.S):
        types[m.group(1).lower()] = m.group(2).strip()
    for m in re.finditer(r"\bENTITY\s+(\w+)(.*?)\bEND_ENTITY;", text, re.S):
        name, body = m.group(1), m.group(2)
        head, _, rest = body.partition(";")
        sub = re.search(r"SUBTYPE\s+OF\s*\(([^)]*)\)", head)
        supers = [s.strip() for s in sub.group(1).split(",")] if sub else []
        abstract = bool(re.search(r"\bABSTRACT\b", head))
        explicit, derive = rest, ""
        for kw in ("DERIVE", "INVERSE", "UNIQUE", "WHERE"):
            i = re.search(r"^[ \t]*%s[ \t]*$" % kw, explicit, re.M)
            if i:
                if kw == "DERIVE":
                    stop = r"^[ \t]*(?:INVERSE|UNIQUE|WHERE)[ \t]*$"
                    derive = re.split(stop, explicit[i.end():], flags=re.M)[0]
                explicit = explicit[: i.start()]
        attrs = []
        for stmt in explicit.split(";"):
            stmt = " ".join(stmt.split())
            if not stmt:
                continue
            names, _, typ = stmt.partition(":")
            for n in names.split(","):
                attrs.append((n.strip(), typ.strip()))
        ders = []
        for stmt in derive.split(";"):
            mm = re.match(r"\s*SELF\\(\w+)\.(\w+)\s*:", stmt)
            if mm:
                ders.append((mm.group(1), mm.group(2)))
        ents[name.lower()] = dict(name=name, supers=supers, abstract=abstract, attrs=attrs,
                                  ders=ders)
    return types, ents


def kind(typ, types, ents):
    t = typ.strip()
    m = re.match(r"(?:LIST|SET|BAG|ARRAY)\s*(?:\[[^\]]*\])?\s*OF\s+(?:UNIQUE\s+)?(.*)", t, re.S)
    if m:
        return "l(%s)" % kind(m.group(1), types, ents)
    if re.match(r"STRING\b", t):
        return "s"
    base = {"REAL": "n", "INTEGER": "i", "NUMBER": "u", "BINARY": "y", "BOOLEAN": "e=F|T",
            "LOGICAL": "e=F|T|U"}
    key = re.match(r"\w+", t).group(0)
    if key.upper() in base:
        return base[key.upper()]
    if key.lower() in ents:
        return "r"
    if key.lower() in types:
        u = types[key.lower()]
        m = re.match(r"ENUMERATION\s+OF\s*\((.*)\)", u, re.S)
        if m:
            return "e=" + "|".join(sorted(v.strip().upper() for v in m.group(1).split(",")))
        if re.match(r"SELECT\b", u):
            return "x"
        return kind(u, types, ents)
    raise SystemExit("unknown type: " + typ)


def chain(ents, name):
    e = ents[name.lower()]
    out = []
    for s in e["supers"]:
        for a in chain(ents, s):
            if a[0] not in [x[0] for x in out]:
                out.append(a)
    own = []
    for n, t in e["attrs"]:
        mm = re.match(r"SELF\\(\w+)\.(\w+)", n)
        if mm:  # redeclaration of an inherited attribute
            for i, a in enumerate(out):
                if a[0] == mm.group(2):
                    out[i] = (a[0], t, False)
        else:
            own.append((n, t, False))
    for _sup, attr in e["ders"]:
        for i, a in enumerate(out):
            if a[0] == attr:
                out[i] = (a[0], a[1], True)
    return out + own


def spec(types, ents, name):
    parts = []
    for _n, t, derived in chain(ents, name):
        if derived:
            parts.append("*")
            continue
        opt = t.startswith("OPTIONAL")
        if opt:
            t = t[len("OPTIONAL"):]
        parts.append(("?" if opt else "") + kind(t, types, ents))
    return ";".join(parts)


def descendants(ents, root):
    out = set()
    for k, e in ents.items():
        stack, seen = list(e["supers"]), set()
        while stack:
            s = stack.pop().lower()
            if s == root.lower():
                out.add(k)
            if s in ents and s not in seen:
                seen.add(s)
                stack += ents[s]["supers"]
    return out


def wrap(s):
    """A Some("...") cell as lines of at most 100 columns (string continuations)."""
    chunks = [s[i:i + 76] for i in range(0, len(s), 76)] or [""]
    body = "\\\n            ".join(chunks)
    return '        Some("%s")' % body


def main(paths):
    schemas = [parse(p) for p in paths]
    names = {n.lower() for n in WRITTEN}
    for _types, ents in schemas:
        for fam in FAMILIES:
            for d in descendants(ents, fam):
                if not ents[d]["abstract"]:
                    names.add(d)
    out = [
        "//! Attribute lists of IFC entities per schema, generated by",
        "//! `tools/gen_ifc_schema_table.py` from the EXPRESS schemas of buildingSMART:",
    ]
    out += ["//!   - " + p for p in META]
    out += [
        "//! Source: %s," % SOURCE,
        "//! fetched %s." % DATE,
        "//! The schema files are not in the repository. Note: IFC4X3 ADD2 is the only build of",
        "//! that schema published as .exp (a dated DEV build of the ADD2 text).",
        "//! Attributes of the whole SUBTYPE OF chain, ancestors first; `*` is a derived (DERIVE)",
        "//! attribute. Spec language: see the script. `None`: the schema has no such entity.",
        "//! Do not edit by hand; run the script again.",
        "",
        "/// Columns: IFC2X3, IFC4, IFC4X3_ADD2.",
        "pub(super) const CLASSES: &[(&str, [Option<&str>; 3])] = &[",
    ]
    rows = []
    for n in sorted(names):
        cells = []
        for types, ents in schemas:
            if n in ents:
                cells.append(wrap(spec(types, ents, ents[n]["name"])))
            else:
                cells.append("        None")
        if all(c.strip() == "None" for c in cells):
            continue
        body = ",\n".join(cells) + ","
        rows.append('    ("%s", [\n%s\n    ]),' % (n.upper(), body))
    out += sorted(rows)
    out.append("];")
    print("\n".join(out))


main(sys.argv[1:])
