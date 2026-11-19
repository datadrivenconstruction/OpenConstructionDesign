//! Parametric family from a description: a closed set of templates, so no free text reaches the
//! definition. The name carries the sizes and the variant, so loading never replaces a family.

use crate::params::{bad, Args};
use crate::{variant_of, GenError, GenOut, Generator, ParamDef, ParamKind};
use ddc_geom::component::{Family, Lod};
use serde_json::Value as J;

static KINDS: [&str; 6] = ["table", "cabinet", "shelf", "box", "door", "window"];
static MATERIALS: [&str; 6] = [
    "Oak",
    "Beech",
    "Melamine White",
    "Steel White",
    "Stainless Steel",
    "Concrete",
];

static PARAMS: [ParamDef; 5] = [
    ParamDef {
        name: "kind",
        kind: ParamKind::Choice(&KINDS),
        min: 0.0,
        max: 0.0,
        default: 0.0,
        help: "What the family is",
    },
    ParamDef {
        name: "width",
        kind: ParamKind::Length,
        min: 0.3,
        max: 6.0,
        default: 1.2,
        help: "Width, m",
    },
    ParamDef {
        name: "depth",
        kind: ParamKind::Length,
        min: 0.2,
        max: 3.0,
        default: 0.6,
        help: "Depth, m; a door or window takes it as the frame depth, at most 0.6",
    },
    ParamDef {
        name: "height",
        kind: ParamKind::Length,
        min: 0.3,
        max: 4.0,
        default: 0.75,
        help: "Height, m; a door is at least 1.8",
    },
    ParamDef {
        name: "material",
        kind: ParamKind::Choice(&MATERIALS),
        min: 0.0,
        max: 0.0,
        default: 0.0,
        help: "Main material",
    },
];

static VARIANTS: [&str; 3] = ["fewest solids", "standard detail", "most detail"];

pub(crate) struct FamilyTemplate;

/// What a variant of a family can be compared by.
pub fn family_metrics(def: &str) -> Vec<(&'static str, f64)> {
    let bodies = def.lines().filter(|l| l.starts_with("extrude ")).count();
    vec![("bodies", bodies as f64)]
}

use ddc_geom::to_mm as mm;

fn head(kind: &str, w: f64, d: f64, h: f64, v: usize, material: &str) -> String {
    let (cat, host, ifc) = match kind {
        "table" => ("furniture", "floor", "IfcFurniture TABLE"),
        "cabinet" => ("casework", "floor", "IfcFurniture NOTDEFINED"),
        "shelf" => ("furniture", "floor", "IfcFurniture SHELF"),
        "door" => ("generic", "wall", "IfcDoor DOOR"),
        "window" => ("generic", "wall", "IfcWindow WINDOW"),
        _ => ("generic", "none", "IfcBuildingElementProxy NOTDEFINED"),
    };
    let (mw, md, mh) = (mm(w), mm(d), mm(h));
    let title = format!("{}{}", kind[..1].to_uppercase(), &kind[1..]);
    let letter = ["A", "B", "C"][v];
    format!(
        "family Gen {title} {mw}x{md}x{mh} {letter}\ncategory {cat}\nhost {host}\nifc {ifc}\n\
         param Width length type {mw}mm\nparam Depth length type {md}mm\nparam Height length type {mh}mm\n\
         param Mat material type \"{material}\"\n\
         ref Left x = -Width / 2\nref Right x = Width / 2\nref Front y = 0\nref Back y = Depth\n\
         type \"{mw}x{md}x{mh}\" Width={mw}mm, Depth={md}mm, Height={mh}mm\n"
    )
}

const PLAN: &str = "symbol rect(Left, Front, Right, Back)\n";

fn table(v: usize) -> String {
    let mut s = String::from(
        "param Top length type 40mm\nparam Leg length type 60mm\nparam LegH length type = Height - Top\n\
         extrude Top rect(Left, Front, Right, Back) z(LegH, Height) mat(Mat) show(3d, elev)\n",
    );
    if v == 0 {
        s += "extrude Base rect(-Leg, Depth / 2 - Leg, Leg, Depth / 2 + Leg) z(0, LegH) mat(Mat) show(3d, elev)\n";
    } else {
        s += "extrude Leg1 rect(Left, Front, Left + Leg, Front + Leg) z(0, LegH) mat(Mat) show(3d, elev)\n\
              extrude Leg2 rect(Right - Leg, Front, Right, Front + Leg) z(0, LegH) mat(Mat) show(3d, elev)\n\
              extrude Leg3 rect(Right - Leg, Back - Leg, Right, Back) z(0, LegH) mat(Mat) show(3d, elev)\n\
              extrude Leg4 rect(Left, Back - Leg, Left + Leg, Back) z(0, LegH) mat(Mat) show(3d, elev)\n";
    }
    if v == 2 {
        s += "extrude Rail1 rect(Left + Leg, Front, Right - Leg, Front + 20mm) z(LegH - 80mm, LegH) mat(Mat) show(3d, elev)\n\
              extrude Rail2 rect(Left + Leg, Back - 20mm, Right - Leg, Back) z(LegH - 80mm, LegH) mat(Mat) show(3d, elev)\n";
    }
    s + PLAN
}

fn cabinet(v: usize) -> String {
    let mut s = match v {
        0 => String::from("extrude Body rect(Left, Front, Right, Back) z(0, Height) mat(Mat) show(3d, elev)\n"),
        _ => String::from(
            "param Plinth length type 100mm\n\
             extrude Plinth rect(Left + 20mm, Front + 20mm, Right - 20mm, Back) z(0, Plinth) mat(Mat) show(3d, elev)\n\
             extrude Body rect(Left, Front, Right, Back) z(Plinth, Height) mat(Mat) show(3d, elev)\n",
        ),
    };
    if v == 2 {
        s += "extrude LeafL rect(Left, Front - 20mm, -2mm, Front) z(Plinth, Height) mat(Mat) show(3d, elev)\n\
              extrude LeafR rect(2mm, Front - 20mm, Right, Front) z(Plinth, Height) mat(Mat) show(3d, elev)\n";
    }
    s + PLAN
}

fn shelf(v: usize) -> String {
    let mut s = String::from(
        "param Board length type 20mm\n\
         extrude SideL rect(Left, Front, Left + Board, Back) z(0, Height) mat(Mat) show(3d, elev)\n\
         extrude SideR rect(Right - Board, Front, Right, Back) z(0, Height) mat(Mat) show(3d, elev)\n\
         extrude Bottom rect(Left + Board, Front, Right - Board, Back) z(0, Board) mat(Mat) show(3d, elev)\n\
         extrude Cover rect(Left + Board, Front, Right - Board, Back) z(Height - Board, Height) mat(Mat) show(3d, elev)\n",
    );
    if v >= 1 {
        s += "extrude Mid rect(Left + Board, Front, Right - Board, Back) z(Height / 2 - Board / 2, Height / 2 + Board / 2) mat(Mat) show(3d, elev)\n";
    }
    if v == 2 {
        s += "extrude Rear rect(Left + Board, Back - Board, Right - Board, Back) z(Board, Height - Board) mat(Mat) show(3d, elev)\n";
    }
    s + PLAN
}

fn block(v: usize) -> String {
    let mut s = String::new();
    if v == 0 {
        s += "extrude Body rect(Left, Front, Right, Back) z(0, Height) mat(Mat) show(3d, elev)\n";
    } else {
        s += "extrude Body rect(Left + 20mm, Front + 20mm, Right - 20mm, Back - 20mm) z(0, Height - 40mm) mat(Mat) show(3d, elev)\n\
              extrude Cap rect(Left, Front, Right, Back) z(Height - 40mm, Height) mat(Mat) show(3d, elev)\n";
    }
    if v == 2 {
        s += "extrude Foot rect(Left, Front, Right, Back) z(0, 40mm) mat(Mat) show(3d, elev)\n";
    }
    s + PLAN
}

/// Door and window: the void cuts the host wall, whatever its thickness.
fn filling(v: usize, kind: &str) -> String {
    let mut s = String::from("param Frame length type 50mm\nextrude Hole void rect(Left, -600mm, Right, 600mm) z(0, Height)\n");
    let leaf = if kind == "door" { "Leaf" } else { "Pane" };
    if v == 0 {
        s += &format!("extrude {leaf} rect(Left, Front, Right, Front + 40mm) z(0, Height) mat(Mat) show(3d, elev)\n");
        return s + PLAN;
    }
    s += &format!("extrude {leaf} rect(Left + Frame, Front, Right - Frame, Front + 40mm) z(Frame, Height - Frame) mat(Mat) show(3d, elev)\n");
    s += "extrude FrameL rect(Left, Front, Left + Frame, Back) z(0, Height) mat(Mat) show(3d, elev)\n\
          extrude FrameR rect(Right - Frame, Front, Right, Back) z(0, Height) mat(Mat) show(3d, elev)\n\
          extrude FrameT rect(Left + Frame, Front, Right - Frame, Back) z(Height - Frame, Height) mat(Mat) show(3d, elev)\n";
    if v == 2 {
        s += "extrude FrameB rect(Left + Frame, Front, Right - Frame, Back) z(0, Frame) mat(Mat) show(3d, elev)\n";
    }
    s + PLAN
}

fn verify(def: &str) -> Result<(), GenError> {
    let fail = |e: String| GenError::Infeasible(format!("family: {e}"));
    let f = Family::parse(def).map_err(|e| fail(format!("{e:?}")))?;
    let t = f.types.first().ok_or_else(|| fail("no type".to_string()))?;
    let r = f
        .resolve(&t.values, &[])
        .map_err(|e| fail(format!("{e:?}")))?;
    f.build(&r, Lod::Medium)
        .map_err(|e| fail(format!("{e:?}")))?;
    Ok(())
}

impl Generator for FamilyTemplate {
    fn id(&self) -> &'static str {
        "family"
    }
    fn params(&self) -> &'static [ParamDef] {
        &PARAMS
    }
    fn variants(&self) -> &'static [&'static str] {
        &VARIANTS
    }
    fn summary(&self) -> &'static str {
        "Parametric family from a kind and sizes. Only the listed kinds and materials are possible."
    }
    fn build(&self, params: &J, seed: u64) -> Result<GenOut, GenError> {
        let a = Args::new(&PARAMS, params)?;
        let kind = a.choice("kind");
        let (w, d, h) = (a.num("width"), a.num("depth"), a.num("height"));
        if matches!(kind, "door" | "window") && d > 0.6 {
            return Err(bad("depth", "a door or window frame is at most 0.6 m deep"));
        }
        if kind == "door" && h < 1.8 {
            return Err(bad("height", "a door is at least 1.8 m high"));
        }
        let v = variant_of(seed);
        let body = match kind {
            "table" => table(v),
            "cabinet" => cabinet(v),
            "shelf" => shelf(v),
            "door" | "window" => filling(v, kind),
            _ => block(v),
        };
        let def = head(kind, w, d, h, v, a.choice("material")) + &body;
        verify(&def)?;
        Ok(GenOut::Family { def })
    }
}
