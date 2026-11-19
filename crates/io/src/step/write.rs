//! STEP AP214 / AP242 writer of exact bodies and assemblies (docs/IO4.md): advanced B-rep
//! faces on planes, cylinders, cones, spheres, tori and B-spline surfaces; one product per part
//! and per assembly node, placements by item defined transformations; surface colours.

use std::fmt::Write as _;

use ddc_geom::math::V3;
use ddc_geom::nurbs;

use crate::exact::{Ax3, Body, Curve, Doc, Node, Surface};
use crate::meshx::Units;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Schema {
    /// AUTOMOTIVE_DESIGN (AP214 IS).
    #[default]
    Ap214,
    /// AP242 managed model based 3D engineering.
    Ap242,
}

impl Schema {
    pub fn parse(s: &str) -> Option<Schema> {
        match s.to_ascii_lowercase().as_str() {
            "ap214" | "214" => Some(Schema::Ap214),
            "ap242" | "242" => Some(Schema::Ap242),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct WriteOptions {
    pub schema: Schema,
    pub units: Units,
    /// FILE_NAME time stamp (ISO 8601); empty gives the epoch, so output can be compared.
    pub stamp: String,
}

/// STEP text of the document.
pub fn write(doc: &Doc, opt: &WriteOptions) -> Result<String, String> {
    if doc.parts.is_empty() {
        return Err("nothing to write: the document has no part".into());
    }
    for (i, p) in doc.parts.iter().enumerate() {
        p.body.check().map_err(|e| format!("part {i} ({}): {e}", p.name))?;
    }
    let mut w = W {
        out: String::new(),
        n: 0,
        k: opt.units.per_metre(),
        ctx: 0,
        styled: vec![],
    };
    let (sch, ac_name, apd) = match opt.schema {
        Schema::Ap214 => (
            "AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }",
            "automotive design",
            "'international standard','automotive_design',2000",
        ),
        Schema::Ap242 => (
            "AP242_MANAGED_MODEL_BASED_3D_ENGINEERING_MIM_LF { 1 0 10303 442 1 1 4 }",
            "managed model based 3d engineering",
            "'international standard','ap242_managed_model_based_3d_engineering',2014",
        ),
    };
    let stamp = if opt.stamp.is_empty() { "1970-01-01T00:00:00" } else { opt.stamp.as_str() };
    let mut head = String::from("ISO-10303-21;\nHEADER;\n");
    let _ = writeln!(head, "FILE_DESCRIPTION(('exact boundary representation'),'2;1');");
    let _ = writeln!(
        head,
        "FILE_NAME({},{},(''),(''),'','OpenConstructionDesign','');",
        text(&doc.name),
        text(stamp)
    );
    let _ = writeln!(head, "FILE_SCHEMA(('{sch}'));\nENDSEC;\nDATA;");

    let ac = w.add(format!("APPLICATION_CONTEXT('{ac_name}')"));
    w.add(format!("APPLICATION_PROTOCOL_DEFINITION({apd},#{ac})"));
    let pc = w.add(format!("PRODUCT_CONTEXT('',#{ac},'mechanical')"));
    let pdc = w.add(format!("PRODUCT_DEFINITION_CONTEXT('part definition',#{ac},'design')"));
    let len = if opt.units == Units::Millimetre {
        w.add("(LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.))")
    } else {
        w.add("(LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT($,.METRE.))")
    };
    let ang = w.add("(NAMED_UNIT(*) PLANE_ANGLE_UNIT() SI_UNIT($,.RADIAN.))");
    let sol = w.add("(NAMED_UNIT(*) SI_UNIT($,.STERADIAN.) SOLID_ANGLE_UNIT())");
    let unc = w.add(format!(
        "UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE({}),#{len},'distance_accuracy_value','confusion accuracy')",
        num(1e-6 * w.k)
    ));
    w.ctx = w.add(format!(
        "(GEOMETRIC_REPRESENTATION_CONTEXT(3) GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{unc})) \
         GLOBAL_UNIT_ASSIGNED_CONTEXT((#{len},#{ang},#{sol})) REPRESENTATION_CONTEXT('3D',''))"
    ));
    let ids = Ids { pc, pdc };

    let mut parts = Vec::with_capacity(doc.parts.len());
    for p in &doc.parts {
        let (rep, origin, solid) = w.body(&p.name, &p.body)?;
        let pd = w.product(&ids, &p.name, rep);
        if let Some(c) = p.rgb {
            w.style(solid, c);
        }
        parts.push(Prod { pd, rep, origin });
    }
    let single = doc.roots.len() == 1 && doc.roots[0].children.is_empty() && doc.roots[0].place == Ax3::WORLD;
    if !single {
        let top = Node {
            name: if doc.name.is_empty() { "assembly".into() } else { doc.name.clone() },
            place: Ax3::WORLD,
            part: None,
            children: doc.roots.clone(),
        };
        w.assembly(&ids, &top, &parts)?;
    }
    if !w.styled.is_empty() {
        let items: Vec<String> = w.styled.iter().map(|i| format!("#{i}")).collect();
        let ctx = w.ctx;
        w.add(format!(
            "MECHANICAL_DESIGN_GEOMETRIC_PRESENTATION_REPRESENTATION('',({}),#{ctx})",
            items.join(",")
        ));
    }
    head.push_str(&w.out);
    head.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
    Ok(head)
}

struct Ids {
    pc: u64,
    pdc: u64,
}

#[derive(Clone, Copy)]
struct Prod {
    pd: u64,
    rep: u64,
    origin: u64,
}

struct W {
    out: String,
    n: u64,
    /// File units per metre.
    k: f64,
    ctx: u64,
    styled: Vec<u64>,
}

/// STEP real: always with a decimal point, no negative zero.
fn num(x: f64) -> String {
    let x = if x == 0.0 { 0.0 } else { x };
    let mut s = format!("{x}");
    if !s.contains('.') && !s.contains('e') && !s.contains("inf") && !s.contains("NaN") {
        s.push('.');
    }
    s
}

/// STEP string: quotes and backslashes doubled, non-ASCII as `\X2\` UTF-16 runs.
fn text(s: &str) -> String {
    let mut o = String::from("'");
    let mut wide = String::new();
    let flush = |o: &mut String, wide: &mut String| {
        if !wide.is_empty() {
            o.push_str("\\X2\\");
            o.push_str(wide);
            o.push_str("\\X0\\");
            wide.clear();
        }
    };
    for c in s.chars() {
        if c.is_ascii() && !c.is_ascii_control() {
            flush(&mut o, &mut wide);
            match c {
                '\'' => o.push_str("''"),
                '\\' => o.push_str("\\\\"),
                _ => o.push(c),
            }
        } else if c.is_ascii_control() {
            flush(&mut o, &mut wide);
            o.push(' ');
        } else {
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                let _ = write!(wide, "{u:04X}");
            }
        }
    }
    flush(&mut o, &mut wide);
    o.push('\'');
    o
}

fn flag(b: bool) -> &'static str {
    if b {
        ".T."
    } else {
        ".F."
    }
}

fn refs(ids: &[u64]) -> String {
    ids.iter().map(|i| format!("#{i}")).collect::<Vec<_>>().join(",")
}

fn nums(xs: impl IntoIterator<Item = f64>) -> String {
    xs.into_iter().map(num).collect::<Vec<_>>().join(",")
}

/// Distinct knots and their multiplicities.
fn knots(k: &[f64]) -> (Vec<f64>, Vec<usize>) {
    let mut v: Vec<f64> = vec![];
    let mut m: Vec<usize> = vec![];
    for &x in k {
        if let (Some(&l), Some(c)) = (v.last(), m.last_mut()) {
            if l == x {
                *c += 1;
                continue;
            }
        }
        v.push(x);
        m.push(1);
    }
    (v, m)
}

impl W {
    fn add(&mut self, s: impl AsRef<str>) -> u64 {
        self.n += 1;
        let _ = writeln!(self.out, "#{}={};", self.n, s.as_ref());
        self.n
    }
    fn pt(&mut self, p: V3) -> u64 {
        let k = self.k;
        self.add(format!("CARTESIAN_POINT('',({},{},{}))", num(p.x * k), num(p.y * k), num(p.z * k)))
    }
    fn dir(&mut self, d: V3) -> u64 {
        self.add(format!("DIRECTION('',({},{},{}))", num(d.x), num(d.y), num(d.z)))
    }
    fn ax(&mut self, a: &Ax3) -> u64 {
        let (p, z, x) = (self.pt(a.o), self.dir(a.z), self.dir(a.x));
        self.add(format!("AXIS2_PLACEMENT_3D('',#{p},#{z},#{x})"))
    }
    fn len(&self, x: f64) -> String {
        num(x * self.k)
    }

    fn nurbs_curve(&mut self, c: &nurbs::Curve) -> u64 {
        let pts: Vec<u64> = c.ctrl().iter().map(|&p| self.pt(p)).collect();
        let (kv, km) = knots(c.knots());
        let mults = km.iter().map(|m| m.to_string()).collect::<Vec<_>>().join(",");
        let (deg, pl, kl) = (c.degree(), refs(&pts), nums(kv));
        if c.weights().iter().all(|&w| w == 1.0) {
            self.add(format!(
                "B_SPLINE_CURVE_WITH_KNOTS('',{deg},({pl}),.UNSPECIFIED.,.F.,.F.,({mults}),({kl}),.UNSPECIFIED.)"
            ))
        } else {
            let wl = nums(c.weights().iter().copied());
            self.add(format!(
                "(BOUNDED_CURVE() B_SPLINE_CURVE({deg},({pl}),.UNSPECIFIED.,.F.,.F.) \
                 B_SPLINE_CURVE_WITH_KNOTS(({mults}),({kl}),.UNSPECIFIED.) CURVE() GEOMETRIC_REPRESENTATION_ITEM() \
                 RATIONAL_B_SPLINE_CURVE(({wl})) REPRESENTATION_ITEM(''))"
            ))
        }
    }

    fn nurbs_surface(&mut self, s: &nurbs::Surface) -> u64 {
        let (nu, nv) = (s.nu(), s.nv());
        let mut rows = vec![];
        for i in 0..nu {
            let r: Vec<u64> = (0..nv).map(|j| self.pt(s.ctrl()[i * nv + j])).collect();
            rows.push(format!("({})", refs(&r)));
        }
        let (ku, mu) = knots(s.knots_u());
        let (kv, mv) = knots(s.knots_v());
        let ms = |m: &[usize]| m.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
        let (du, dv, pl) = (s.deg_u(), s.deg_v(), rows.join(","));
        let (mu, mv, ku, kv) = (ms(&mu), ms(&mv), nums(ku), nums(kv));
        if s.weights().iter().all(|&w| w == 1.0) {
            self.add(format!(
                "B_SPLINE_SURFACE_WITH_KNOTS('',{du},{dv},({pl}),.UNSPECIFIED.,.F.,.F.,.F.,({mu}),({mv}),({ku}),({kv}),.UNSPECIFIED.)"
            ))
        } else {
            let wl: Vec<String> = (0..nu)
                .map(|i| format!("({})", nums((0..nv).map(|j| s.weights()[i * nv + j]))))
                .collect();
            let wl = wl.join(",");
            self.add(format!(
                "(BOUNDED_SURFACE() B_SPLINE_SURFACE({du},{dv},({pl}),.UNSPECIFIED.,.F.,.F.,.F.) \
                 B_SPLINE_SURFACE_WITH_KNOTS(({mu}),({mv}),({ku}),({kv}),.UNSPECIFIED.) GEOMETRIC_REPRESENTATION_ITEM() \
                 RATIONAL_B_SPLINE_SURFACE(({wl})) REPRESENTATION_ITEM('') SURFACE())"
            ))
        }
    }

    fn surface(&mut self, s: &Surface) -> u64 {
        match s {
            Surface::Plane(a) => {
                let a = self.ax(a);
                self.add(format!("PLANE('',#{a})"))
            }
            Surface::Cylinder { ax, r } => {
                let a = self.ax(ax);
                let r = self.len(*r);
                self.add(format!("CYLINDRICAL_SURFACE('',#{a},{r})"))
            }
            Surface::Cone { ax, r, semi } => {
                let a = self.ax(ax);
                let r = self.len(*r);
                self.add(format!("CONICAL_SURFACE('',#{a},{r},{})", num(*semi)))
            }
            Surface::Sphere { ax, r } => {
                let a = self.ax(ax);
                let r = self.len(*r);
                self.add(format!("SPHERICAL_SURFACE('',#{a},{r})"))
            }
            Surface::Torus { ax, major, minor } => {
                let a = self.ax(ax);
                let (r1, r2) = (self.len(*major), self.len(*minor));
                self.add(format!("TOROIDAL_SURFACE('',#{a},{r1},{r2})"))
            }
            Surface::Nurbs(n) => self.nurbs_surface(n),
        }
    }

    /// Representation of a body: (representation, its origin placement, solid or surface model).
    fn body(&mut self, name: &str, b: &Body) -> Result<(u64, u64, u64), String> {
        let vp: Vec<u64> = b
            .verts
            .iter()
            .map(|&p| {
                let q = self.pt(p);
                self.add(format!("VERTEX_POINT('',#{q})"))
            })
            .collect();
        let mut ec = Vec::with_capacity(b.edges.len());
        for e in &b.edges {
            let (a, z) = (b.verts[e.v[0] as usize], b.verts[e.v[1] as usize]);
            let c = match &e.curve {
                Curve::Line => {
                    let d = z - a;
                    if d.len() <= 0.0 {
                        return Err("a straight edge of zero length".into());
                    }
                    let p = self.pt(a);
                    let dd = self.dir(d.normalized());
                    let l = self.len(d.len());
                    let v = self.add(format!("VECTOR('',#{dd},{l})"));
                    self.add(format!("LINE('',#{p},#{v})"))
                }
                Curve::Circle { ax, r } => {
                    let x = self.ax(ax);
                    let r = self.len(*r);
                    self.add(format!("CIRCLE('',#{x},{r})"))
                }
                Curve::Ellipse { ax, a, b } => {
                    let x = self.ax(ax);
                    let (a, b) = (self.len(*a), self.len(*b));
                    self.add(format!("ELLIPSE('',#{x},{a},{b})"))
                }
                Curve::Nurbs(n) => self.nurbs_curve(n),
            };
            let (v0, v1) = (vp[e.v[0] as usize], vp[e.v[1] as usize]);
            ec.push(self.add(format!("EDGE_CURVE('',#{v0},#{v1},#{c},{})", flag(e.sense))));
        }
        // void shells are written reversed and placed with orientation .F.
        let mut shells = vec![];
        for (si, sh) in b.shells.iter().enumerate() {
            let rev = b.closed && si > 0;
            let mut fs = vec![];
            for &fi in sh {
                let f = &b.faces[fi as usize];
                let mut bounds = vec![];
                for (li, l) in f.loops.iter().enumerate() {
                    let oes: Vec<u64> = l
                        .iter()
                        .map(|&(e, fwd)| {
                            let id = ec[e as usize];
                            self.add(format!("ORIENTED_EDGE('',*,*,#{id},{})", flag(fwd)))
                        })
                        .collect();
                    let lp = self.add(format!("EDGE_LOOP('',({}))", refs(&oes)));
                    let kind = if li == 0 { "FACE_OUTER_BOUND" } else { "FACE_BOUND" };
                    bounds.push(self.add(format!("{kind}('',#{lp},{})", flag(!rev))));
                }
                let s = self.surface(&f.surface);
                let face = self.add(format!("ADVANCED_FACE('',({}),#{s},{})", refs(&bounds), flag(f.sense != rev)));
                if let Some(c) = f.rgb {
                    self.style(face, c);
                }
                fs.push(face);
            }
            let kind = if b.closed { "CLOSED_SHELL" } else { "OPEN_SHELL" };
            shells.push(self.add(format!("{kind}('',({}))", refs(&fs))));
        }
        if shells.is_empty() {
            return Err("a body without shells".into());
        }
        let origin = self.ax(&Ax3::WORLD);
        let ctx = self.ctx;
        let nm = text(name);
        if b.closed {
            let solid = if shells.len() == 1 {
                self.add(format!("MANIFOLD_SOLID_BREP({nm},#{})", shells[0]))
            } else {
                let voids: Vec<u64> = shells[1..]
                    .iter()
                    .map(|s| self.add(format!("ORIENTED_CLOSED_SHELL('',*,#{s},.F.)")))
                    .collect();
                self.add(format!("BREP_WITH_VOIDS({nm},#{},({}))", shells[0], refs(&voids)))
            };
            let rep = self.add(format!("ADVANCED_BREP_SHAPE_REPRESENTATION('',(#{solid},#{origin}),#{ctx})"));
            Ok((rep, origin, solid))
        } else {
            let model = self.add(format!("SHELL_BASED_SURFACE_MODEL({nm},({}))", refs(&shells)));
            let rep = self.add(format!("MANIFOLD_SURFACE_SHAPE_REPRESENTATION('',(#{model},#{origin}),#{ctx})"));
            Ok((rep, origin, model))
        }
    }

    /// Product, its definition and shape, bound to `rep`; returns the product definition.
    fn product(&mut self, ids: &Ids, name: &str, rep: u64) -> u64 {
        let nm = text(name);
        let pc = ids.pc;
        let pdc = ids.pdc;
        let p = self.add(format!("PRODUCT({nm},{nm},'',(#{pc}))"));
        let f = self.add(format!("PRODUCT_DEFINITION_FORMATION('','',#{p})"));
        let pd = self.add(format!("PRODUCT_DEFINITION('design','',#{f},#{pdc})"));
        let pds = self.add(format!("PRODUCT_DEFINITION_SHAPE('','',#{pd})"));
        self.add(format!("SHAPE_DEFINITION_REPRESENTATION(#{pds},#{rep})"));
        pd
    }

    fn style(&mut self, item: u64, c: [u8; 3]) {
        let [r, g, b] = c.map(|x| num(f64::from(x) / 255.0));
        let col = self.add(format!("COLOUR_RGB('',{r},{g},{b})"));
        let fc = self.add(format!("FILL_AREA_STYLE_COLOUR('',#{col})"));
        let fs = self.add(format!("FILL_AREA_STYLE('',(#{fc}))"));
        let sf = self.add(format!("SURFACE_STYLE_FILL_AREA(#{fs})"));
        let ss = self.add(format!("SURFACE_SIDE_STYLE('',(#{sf}))"));
        let su = self.add(format!("SURFACE_STYLE_USAGE(.BOTH.,#{ss})"));
        let psa = self.add(format!("PRESENTATION_STYLE_ASSIGNMENT((#{su}))"));
        let si = self.add(format!("STYLED_ITEM('color',(#{psa}),#{item})"));
        self.styled.push(si);
    }

    /// Assembly product of `n`, its children written first.
    fn assembly(&mut self, ids: &Ids, n: &Node, parts: &[Prod]) -> Result<Prod, String> {
        let own_part = Node {
            name: n.name.clone(),
            place: Ax3::WORLD,
            part: n.part,
            children: vec![],
        };
        let mut kids: Vec<(Prod, &Node)> = vec![];
        if let Some(p) = n.part {
            kids.push((*parts.get(p).ok_or_else(|| format!("node '{}': no part {p}", n.name))?, &own_part));
        }
        for c in &n.children {
            let prod = match (c.part, c.children.is_empty()) {
                (Some(p), true) => *parts.get(p).ok_or_else(|| format!("node '{}': no part {p}", c.name))?,
                _ => self.assembly(ids, c, parts)?,
            };
            kids.push((prod, c));
        }
        let origin = self.ax(&Ax3::WORLD);
        let places: Vec<u64> = kids.iter().map(|(_, c)| self.ax(&c.place)).collect();
        let mut items = vec![origin];
        items.extend(&places);
        let ctx = self.ctx;
        let rep = self.add(format!("SHAPE_REPRESENTATION('',({}),#{ctx})", refs(&items)));
        let pd = self.product(ids, &n.name, rep);
        for (k, (child, c)) in kids.iter().enumerate() {
            let nm = text(&c.name);
            let id = text(&format!("{}", k + 1));
            let nauo = self.add(format!("NEXT_ASSEMBLY_USAGE_OCCURRENCE({id},{nm},'',#{pd},#{},$)", child.pd));
            let pds = self.add(format!("PRODUCT_DEFINITION_SHAPE('','',#{nauo})"));
            let idt = self.add(format!("ITEM_DEFINED_TRANSFORMATION('','',#{},#{})", child.origin, places[k]));
            let rr = self.add(format!(
                "(REPRESENTATION_RELATIONSHIP('','',#{},#{rep}) REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION(#{idt}) \
                 SHAPE_REPRESENTATION_RELATIONSHIP())",
                child.rep
            ));
            self.add(format!("CONTEXT_DEPENDENT_SHAPE_REPRESENTATION(#{rr},#{pds})"));
        }
        Ok(Prod { pd, rep, origin })
    }
}

#[cfg(test)]
pub(crate) fn num_for_tests(x: f64) -> String {
    num(x)
}

#[cfg(test)]
pub(crate) fn text_for_tests(s: &str) -> String {
    text(s)
}
