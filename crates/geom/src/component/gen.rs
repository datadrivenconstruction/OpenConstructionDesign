//! Parametric family generators: doors, windows, stairs, steel sections, pipe and duct fittings.
//! Each returns family text (see the module header of `component`); sizes are parameters, the
//! counts (panes, risers, bend segments) are fixed when the family is generated.

use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoorKind {
    Single,
    Double,
    Sliding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowKind {
    Fixed,
    Casement,
    /// Columns × rows of panes.
    MultiPane(u32, u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StairKind {
    Straight,
    /// Quarter landing: the second flight turns right.
    L,
    /// Half landing: the second flight runs back beside the first.
    U,
}

/// Steel section; dimensions in metres as the user entered them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Steel {
    /// Height, flange width, web, flange.
    I { h: f64, b: f64, tw: f64, tf: f64 },
    H { h: f64, b: f64, tw: f64, tf: f64 },
    /// Channel: height, flange width, web, flange.
    C { h: f64, b: f64, tw: f64, tf: f64 },
    /// Angle: leg heights and thickness.
    L { h: f64, b: f64, t: f64 },
    /// Rectangular hollow section.
    Rhs { h: f64, b: f64, t: f64 },
    /// Circular hollow section: outside diameter, wall.
    Chs { d: f64, t: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fitting {
    PipeElbow,
    PipeTee,
    PipeReducer,
    DuctElbow,
    DuctTee,
    DuctTransition,
}

fn mm(v: f64) -> String {
    super::doc::mm(v)
}

fn head(s: &mut String, name: &str, cat: &str, host: &str, ifc: &str) {
    let _ = writeln!(s, "family {name}\ncategory {cat}\nhost {host}\nifc {ifc}");
}

/// Wall-hosted frame shared by doors and windows: jambs, head, the opening void.
fn frame(s: &mut String, bottom: bool) {
    s.push_str(
        "extrude JambL rect(Left, -FrameD / 2, Left + FrameW, FrameD / 2) z(0, Height) mat(FrameMat)\n\
         extrude JambR rect(Right - FrameW, -FrameD / 2, Right, FrameD / 2) z(0, Height) mat(FrameMat)\n\
         extrude Head rect(Left + FrameW, -FrameD / 2, Right - FrameW, FrameD / 2) z(Height - FrameW, Height) mat(FrameMat)\n",
    );
    if bottom {
        s.push_str(
            "extrude Bottom rect(Left + FrameW, -FrameD / 2, Right - FrameW, FrameD / 2) z(0, FrameW) mat(FrameMat)\n",
        );
    }
    s.push_str("extrude Opening rect(Left, -1m, Right, 1m) z(0, Height) void\n");
}

pub fn door(kind: DoorKind) -> String {
    let (name, w) = match kind {
        DoorKind::Single => ("Door Single", 0.9),
        DoorKind::Double => ("Door Double", 1.6),
        DoorKind::Sliding => ("Door Sliding", 1.8),
    };
    let mut s = String::new();
    head(&mut s, name, "door", "wall", match kind {
        DoorKind::Sliding => "IfcDoor SLIDING_TO_LEFT",
        _ => "IfcDoor DOOR",
    });
    let _ = writeln!(s, "param Width length type {}", mm(w));
    s.push_str(
        "param Height length type 2100mm\n\
         param Sill length type 0mm\n\
         param FrameW length type 50mm\n\
         param FrameD length type 100mm\n\
         param LeafT length type 40mm\n\
         param FrameMat material type \"Wood\"\n\
         param LeafMat material type \"Wood\"\n\
         ref Left x = -Width / 2\n\
         ref Right x = Width / 2\n\
         limit Width 400mm 4000mm\n\
         limit Height 1500mm 4000mm\n",
    );
    let leaves = if kind == DoorKind::Single { 1 } else { 2 };
    let _ = writeln!(s, "param LeafW length type = (Width - 2 * FrameW) / {leaves}");
    match kind {
        DoorKind::Single => s.push_str(
            "type \"800 x 2100\" Width=800mm\ntype \"900 x 2100\" Width=900mm\n\
             type \"1000 x 2100\" Width=1000mm\n",
        ),
        DoorKind::Double => s.push_str(
            "type \"1400 x 2100\" Width=1400mm\ntype \"1600 x 2100\" Width=1600mm\n",
        ),
        DoorKind::Sliding => s.push_str(
            "type \"1600 x 2100\" Width=1600mm\ntype \"2400 x 2200\" Width=2400mm, Height=2200mm\n",
        ),
    }
    frame(&mut s, false);
    let leaf = "mat(LeafMat) show(coarse, medium, fine, 3d, elev)";
    match kind {
        DoorKind::Single => {
            let _ = writeln!(s, "extrude Leaf rect(Left + FrameW, -LeafT / 2, Right - FrameW, LeafT / 2) z(0, Height - FrameW) {leaf}");
            s.push_str(
                "symbol line(Left + FrameW, 0, Left + FrameW, LeafW)\n\
                 symbol arc(Left + FrameW, 0, LeafW, 0deg, 90deg)\n",
            );
        }
        DoorKind::Double => {
            let _ = writeln!(s, "extrude LeafA rect(Left + FrameW, -LeafT / 2, Left + FrameW + LeafW, LeafT / 2) z(0, Height - FrameW) {leaf}");
            let _ = writeln!(s, "extrude LeafB rect(Right - FrameW - LeafW, -LeafT / 2, Right - FrameW, LeafT / 2) z(0, Height - FrameW) {leaf}");
            s.push_str(
                "symbol line(Left + FrameW, 0, Left + FrameW, LeafW)\n\
                 symbol arc(Left + FrameW, 0, LeafW, 0deg, 90deg)\n\
                 symbol line(Right - FrameW, 0, Right - FrameW, LeafW)\n\
                 symbol arc(Right - FrameW, 0, LeafW, 90deg, 180deg)\n",
            );
        }
        DoorKind::Sliding => {
            s.push_str("param Overlap length type 50mm\n");
            let all = "mat(LeafMat)";
            let _ = writeln!(s, "extrude PanelA rect(Left + FrameW, -LeafT, Left + FrameW + LeafW + Overlap, 0) z(0, Height - FrameW) {all}");
            let _ = writeln!(s, "extrude PanelB rect(Right - FrameW - LeafW - Overlap, 0, Right - FrameW, LeafT) z(0, Height - FrameW) {all}");
            s.push_str("symbol line(Left + FrameW + LeafW / 2, -LeafT * 3, Left + FrameW + LeafW * 1.5, -LeafT * 3)\n");
        }
    }
    s
}

pub fn window(kind: WindowKind) -> String {
    let (name, cols, rows, sash) = match kind {
        WindowKind::Fixed => ("Window Fixed".to_string(), 1, 1, false),
        WindowKind::Casement => ("Window Casement".to_string(), 1, 1, true),
        WindowKind::MultiPane(c, r) => (format!("Window {c}x{r}"), c.max(1), r.max(1), false),
    };
    let mut s = String::new();
    head(&mut s, &name, "window", "wall", "IfcWindow WINDOW");
    s.push_str(
        "param Width length type 1200mm\n\
         param Height length type 1400mm\n\
         param Sill length type 900mm\n\
         param FrameW length type 60mm\n\
         param FrameD length type 80mm\n\
         param GlassT length type 24mm\n\
         param BarW length type 40mm\n\
         param FrameMat material type \"PVC\"\n\
         param Glass material type \"Glass\"\n\
         ref Left x = -Width / 2\n\
         ref Right x = Width / 2\n\
         limit Width 300mm 6000mm\n\
         limit Height 300mm 4000mm\n\
         type \"600 x 600\" Width=600mm, Height=600mm\n\
         type \"1200 x 1400\" Width=1200mm, Height=1400mm\n\
         type \"1800 x 1500\" Width=1800mm, Height=1500mm\n",
    );
    frame(&mut s, true);
    // inner field: x from Left + FrameW (+ sash), z from FrameW
    let inset = if sash {
        s.push_str("param SashW length type 50mm\n");
        let sm = "mat(FrameMat)";
        let _ = writeln!(s, "extrude SashL rect(Left + FrameW, -FrameD / 4, Left + FrameW + SashW, FrameD / 4) z(FrameW, Height - FrameW) {sm}");
        let _ = writeln!(s, "extrude SashR rect(Right - FrameW - SashW, -FrameD / 4, Right - FrameW, FrameD / 4) z(FrameW, Height - FrameW) {sm}");
        let _ = writeln!(s, "extrude SashT rect(Left + FrameW + SashW, -FrameD / 4, Right - FrameW - SashW, FrameD / 4) z(Height - FrameW - SashW, Height - FrameW) {sm}");
        let _ = writeln!(s, "extrude SashB rect(Left + FrameW + SashW, -FrameD / 4, Right - FrameW - SashW, FrameD / 4) z(FrameW, FrameW + SashW) {sm}");
        s.push_str(
            "symbol line(Left + FrameW, FrameD / 2, Left + FrameW + (Width - 2 * FrameW) * 0.7071, FrameD / 2 + (Width - 2 * FrameW) * 0.7071)\n\
             symbol arc(Left + FrameW, FrameD / 2, Width - 2 * FrameW, 0deg, 45deg)\n",
        );
        "(FrameW + SashW)"
    } else {
        "FrameW"
    };
    let _ = writeln!(s, "param CellW length type = (Width - 2 * {inset} - {} * BarW) / {cols}", cols - 1);
    let _ = writeln!(s, "param CellH length type = (Height - 2 * {inset} - {} * BarW) / {rows}", rows - 1);
    for i in 0..cols {
        let x0 = format!("Left + {inset} + {i} * (CellW + BarW)");
        if i > 0 {
            let _ = writeln!(s, "extrude Mullion{i} rect({x0} - BarW, -FrameD / 4, {x0}, FrameD / 4) z({inset}, Height - {inset}) mat(FrameMat)");
        }
        for j in 0..rows {
            let z0 = format!("{inset} + {j} * (CellH + BarW)");
            if j > 0 {
                let _ = writeln!(s, "extrude Bar{i}_{j} rect({x0}, -FrameD / 4, {x0} + CellW, FrameD / 4) z({z0} - BarW, {z0}) mat(FrameMat)");
            }
            let _ = writeln!(s, "extrude Pane{i}_{j} rect({x0}, -GlassT / 2, {x0} + CellW, GlassT / 2) z({z0}, {z0} + CellH) mat(Glass)");
        }
    }
    s.push_str("symbol line(Left + FrameW, 0, Right - FrameW, 0)\n");
    s
}

/// Stair as one component; `risers` per flight is fixed, its height follows `Height`.
pub fn stair(kind: StairKind, risers: u32) -> String {
    let n = risers.max(3);
    let (name, flights) = match kind {
        StairKind::Straight => ("Stair Straight", vec![n]),
        StairKind::L => ("Stair L", vec![n / 2, n - n / 2]),
        StairKind::U => ("Stair U", vec![n / 2, n - n / 2]),
    };
    let mut s = String::new();
    head(&mut s, name, "stairs", "floor", "IfcStair NOTDEFINED");
    let _ = writeln!(s, "param Height length type {}", mm(0.175 * n as f64));
    let _ = writeln!(s, "param Riser length type = Height / {n}");
    s.push_str(
        "param Going length type 280mm\n\
         param Width length type 1000mm\n\
         param Waist length type 150mm\n\
         param Gap length type 100mm\n\
         param Concrete material type \"Concrete\"\n\
         limit Width 600mm 3000mm\n\
         limit Going 200mm 400mm\n",
    );
    let _ = writeln!(s, "type \"{n} risers\" Width=1000mm");
    let m = "mat(Concrete)";
    let step = |s: &mut String, name: String, rect: String, k: u32| {
        let lo = if k == 0 {
            "0".to_string()
        } else {
            format!("max(0mm, {k} * Riser - Waist)")
        };
        let _ = writeln!(s, "extrude {name} rect({rect}) z({lo}, {} * Riser) {m}", k + 1);
    };
    let f1 = flights[0];
    // flight 1 runs along +y from y = 0, x across [-Width/2, Width/2]
    let last1 = if flights.len() == 1 { f1 } else { f1 - 1 };
    for k in 0..last1 {
        step(
            &mut s,
            format!("Step{k}"),
            format!("-Width / 2, {k} * Going, Width / 2, {} * Going", k + 1),
            k,
        );
    }
    if flights.len() == 1 {
        let _ = writeln!(s, "symbol line(0, 0, 0, {f1} * Going)");
        return s;
    }
    let f2 = flights[1];
    let lz = format!("{f1} * Riser");
    let ly = format!("{last1} * Going");
    match kind {
        StairKind::L => {
            let _ = writeln!(s, "extrude Landing rect(-Width / 2, {ly}, Width / 2, {ly} + Width) z({lz} - Waist, {lz}) {m}");
            // flight 2 runs along +x from the landing's right edge
            for k in 0..f2 {
                let j = f1 + k;
                step(
                    &mut s,
                    format!("Step{j}"),
                    format!("Width / 2 + {k} * Going, {ly}, Width / 2 + {} * Going, {ly} + Width", k + 1),
                    j,
                );
            }
            let _ = writeln!(s, "symbol poly(0, 0, 0, {ly} + Width / 2, Width / 2 + {f2} * Going, {ly} + Width / 2)");
        }
        _ => {
            let _ = writeln!(s, "extrude Landing rect(-Width / 2, {ly}, 1.5 * Width + Gap, {ly} + Width) z({lz} - Waist, {lz}) {m}");
            // flight 2 runs back along -y beside flight 1
            for k in 0..f2 {
                let j = f1 + k;
                step(
                    &mut s,
                    format!("Step{j}"),
                    format!("Width / 2 + Gap, {ly} - {} * Going, 1.5 * Width + Gap, {ly} - {k} * Going", k + 1),
                    j,
                );
            }
            let _ = writeln!(s, "symbol poly(0, 0, 0, {ly} + Width / 2, Width + Gap, {ly} + Width / 2, Width + Gap, {ly} - {f2} * Going)");
        }
    }
    s
}

/// Steel member along x from the insertion point; the section is centred on y, z from 0.
pub fn steel(sec: Steel) -> String {
    let mut s = String::new();
    let (name, dims): (&str, Vec<(&str, f64)>) = match sec {
        Steel::I { h, b, tw, tf } => ("Steel I", vec![("H", h), ("B", b), ("Tw", tw), ("Tf", tf)]),
        Steel::H { h, b, tw, tf } => ("Steel H", vec![("H", h), ("B", b), ("Tw", tw), ("Tf", tf)]),
        Steel::C { h, b, tw, tf } => ("Steel C", vec![("H", h), ("B", b), ("Tw", tw), ("Tf", tf)]),
        Steel::L { h, b, t } => ("Steel L", vec![("H", h), ("B", b), ("T", t)]),
        Steel::Rhs { h, b, t } => ("Steel RHS", vec![("H", h), ("B", b), ("T", t)]),
        Steel::Chs { d, t } => ("Steel CHS", vec![("D", d), ("T", t)]),
    };
    head(&mut s, name, "structural", "none", "IfcMember NOTDEFINED");
    for (k, v) in &dims {
        let _ = writeln!(s, "param {k} length type {}", mm(*v));
    }
    s.push_str("param Length length instance 3000mm\nparam Steel material type \"Steel\"\n");
    let tname = dims
        .iter()
        .map(|(_, v)| format!("{}", (v * 1000.0 * 10.0).round() / 10.0))
        .collect::<Vec<_>>()
        .join("x");
    let vals = dims
        .iter()
        .map(|(k, v)| format!("{k}={}", mm(*v)))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(s, "type \"{tname}\" {vals}");
    let ex = "dir(x) z(0, Length) mat(Steel)";
    match sec {
        Steel::I { .. } | Steel::H { .. } => {
            let _ = writeln!(s, "extrude Section poly(-B / 2, 0, B / 2, 0, B / 2, Tf, Tw / 2, Tf, Tw / 2, H - Tf, B / 2, H - Tf, B / 2, H, -B / 2, H, -B / 2, H - Tf, -Tw / 2, H - Tf, -Tw / 2, Tf, -B / 2, Tf) {ex}");
        }
        Steel::C { .. } => {
            let _ = writeln!(s, "extrude Section poly(0, 0, B, 0, B, Tf, Tw, Tf, Tw, H - Tf, B, H - Tf, B, H, 0, H) {ex}");
        }
        Steel::L { .. } => {
            let _ = writeln!(s, "extrude Section poly(0, 0, B, 0, B, T, T, T, T, H, 0, H) {ex}");
        }
        Steel::Rhs { .. } => {
            let _ = writeln!(s, "extrude Section rect(-B / 2, 0, B / 2, H) {ex}");
            let _ = writeln!(s, "extrude Hollow rect(-B / 2 + T, T, B / 2 - T, H - T) dir(x) z(0, Length) void");
        }
        Steel::Chs { .. } => {
            let _ = writeln!(s, "extrude Section circle(0, D / 2, D / 2) {ex}");
            let _ = writeln!(s, "extrude Hollow circle(0, D / 2, D / 2 - T) dir(x) z(0, Length) void");
        }
    }
    s
}

/// Points of a 90° bend of radius `R` from (0, 0) along +x turning to +y, as path text.
fn bend_path(segs: u32) -> String {
    (0..=segs)
        .map(|k| {
            let t = std::f64::consts::FRAC_PI_2 * k as f64 / segs as f64;
            format!("R * {:.6}, R * {:.6}, 0", t.sin(), 1.0 - t.cos())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn fitting(kind: Fitting) -> String {
    let mut s = String::new();
    let pipe = matches!(kind, Fitting::PipeElbow | Fitting::PipeTee | Fitting::PipeReducer);
    let name = match kind {
        Fitting::PipeElbow => "Pipe Elbow",
        Fitting::PipeTee => "Pipe Tee",
        Fitting::PipeReducer => "Pipe Reducer",
        Fitting::DuctElbow => "Duct Elbow",
        Fitting::DuctTee => "Duct Tee",
        Fitting::DuctTransition => "Duct Transition",
    };
    let pre = match kind {
        Fitting::PipeElbow | Fitting::DuctElbow => "BEND",
        Fitting::PipeTee | Fitting::DuctTee => "JUNCTION",
        _ => "TRANSITION",
    };
    if pipe {
        head(&mut s, name, "pipe_fitting", "none", &format!("IfcPipeFitting {pre}"));
        s.push_str("param Mat material type \"Steel\"\n");
    } else {
        head(&mut s, name, "duct_fitting", "none", &format!("IfcDuctFitting {pre}"));
        s.push_str("param Mat material type \"Galvanised Steel\"\n");
    }
    match kind {
        Fitting::PipeElbow => {
            s.push_str(
                "param D length type 50mm\nparam R length type = 1.5 * D\n\
                 type \"DN50\" D=60.3mm\ntype \"DN100\" D=114.3mm\n",
            );
            let _ = writeln!(s, "sweep Body circle(0, 0, D / 2) path({}) mat(Mat)", bend_path(8));
            s.push_str(
                "connector A pipe at(0, 0, 0) dir(-1, 0, 0) size(D)\n\
                 connector B pipe at(R, R, 0) dir(0, 1, 0) size(D)\n",
            );
        }
        Fitting::PipeTee => {
            s.push_str(
                "param D length type 60.3mm\nparam Db length type 60.3mm\nparam L length type = 3 * D\n\
                 type \"DN50\" D=60.3mm, Db=60.3mm\ntype \"DN100 x DN50\" D=114.3mm, Db=60.3mm\n\
                 limit Db 1mm _\n",
            );
            s.push_str(
                "extrude Run circle(0, 0, D / 2) dir(x) z(-L / 2, L / 2) mat(Mat)\n\
                 extrude Branch circle(0, 0, Db / 2) dir(y) z(sqrt(max(D * D / 4 - Db * Db / 4, 0mm * 0mm)), L / 2) mat(Mat)\n\
                 connector A pipe at(-L / 2, 0, 0) dir(-1, 0, 0) size(D)\n\
                 connector B pipe at(L / 2, 0, 0) dir(1, 0, 0) size(D)\n\
                 connector C pipe at(0, L / 2, 0) dir(0, 1, 0) size(Db)\n",
            );
        }
        Fitting::PipeReducer => {
            s.push_str(
                "param D1 length type 114.3mm\nparam D2 length type 60.3mm\nparam L length type 120mm\n\
                 type \"DN100 x DN50\" D1=114.3mm, D2=60.3mm\n\
                 blend Body circle(0, 0, D1 / 2) circle(0, 0, D2 / 2) dir(x) z(0, L) mat(Mat)\n\
                 connector A pipe at(0, 0, 0) dir(-1, 0, 0) size(D1)\n\
                 connector B pipe at(L, 0, 0) dir(1, 0, 0) size(D2)\n",
            );
        }
        Fitting::DuctElbow => {
            s.push_str(
                "param W length type 400mm\nparam H length type 300mm\nparam R length type = W\n\
                 type \"400 x 300\" W=400mm, H=300mm\n",
            );
            let _ = writeln!(s, "sweep Body rect(-W / 2, -H / 2, W / 2, H / 2) path({}) mat(Mat)", bend_path(8));
            s.push_str(
                "connector A duct at(0, 0, 0) dir(-1, 0, 0) size(W)\n\
                 connector B duct at(R, R, 0) dir(0, 1, 0) size(W)\n",
            );
        }
        Fitting::DuctTee => {
            s.push_str(
                "param W length type 400mm\nparam H length type 300mm\nparam Wb length type 250mm\n\
                 param L length type = W + Wb + 200mm\n\
                 type \"400 x 300 / 250\" W=400mm, H=300mm, Wb=250mm\n\
                 extrude Run rect(-W / 2, -H / 2, W / 2, H / 2) dir(x) z(-L / 2, L / 2) mat(Mat)\n\
                 extrude Branch rect(-Wb / 2, -H / 2, Wb / 2, H / 2) dir(y) z(W / 2, W / 2 + 200mm) mat(Mat)\n\
                 connector A duct at(-L / 2, 0, 0) dir(-1, 0, 0) size(W)\n\
                 connector B duct at(L / 2, 0, 0) dir(1, 0, 0) size(W)\n\
                 connector C duct at(0, W / 2 + 200mm, 0) dir(0, 1, 0) size(Wb)\n",
            );
        }
        Fitting::DuctTransition => {
            s.push_str(
                "param W1 length type 600mm\nparam H1 length type 400mm\n\
                 param W2 length type 400mm\nparam H2 length type 300mm\nparam L length type 400mm\n\
                 type \"600x400 - 400x300\" W1=600mm, H1=400mm, W2=400mm, H2=300mm\n\
                 blend Body rect(-W1 / 2, -H1 / 2, W1 / 2, H1 / 2) rect(-W2 / 2, -H2 / 2, W2 / 2, H2 / 2) dir(x) z(0, L) mat(Mat)\n\
                 connector A duct at(0, 0, 0) dir(-1, 0, 0) size(W1)\n\
                 connector B duct at(L, 0, 0) dir(1, 0, 0) size(W2)\n",
            );
        }
    }
    s
}

/// Family texts of every generator at its default sizes.
pub fn catalog() -> Vec<String> {
    let mut v = vec![
        door(DoorKind::Single),
        door(DoorKind::Double),
        door(DoorKind::Sliding),
        window(WindowKind::Fixed),
        window(WindowKind::Casement),
        window(WindowKind::MultiPane(2, 2)),
        window(WindowKind::MultiPane(3, 1)),
        stair(StairKind::Straight, 17),
        stair(StairKind::L, 18),
        stair(StairKind::U, 18),
        steel(Steel::I { h: 0.3, b: 0.15, tw: 0.0071, tf: 0.0107 }),
        steel(Steel::H { h: 0.2, b: 0.2, tw: 0.009, tf: 0.015 }),
        steel(Steel::C { h: 0.2, b: 0.075, tw: 0.0085, tf: 0.0115 }),
        steel(Steel::L { h: 0.1, b: 0.1, t: 0.01 }),
        steel(Steel::Rhs { h: 0.2, b: 0.1, t: 0.008 }),
        steel(Steel::Chs { d: 0.1683, t: 0.008 }),
    ];
    for f in [
        Fitting::PipeElbow,
        Fitting::PipeTee,
        Fitting::PipeReducer,
        Fitting::DuctElbow,
        Fitting::DuctTee,
        Fitting::DuctTransition,
    ] {
        v.push(fitting(f));
    }
    v
}
