// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Steel connection catalogue: bolts, property classes, stock lengths, spacing rules, fillet
//! welds, plates and connection defaults, read once from `data/steel/*.csv` (our own data).
//! The files hold millimetres; everything here is metres, pascals and radians. The realism
//! checks read the same rules, so a detail and its check never disagree.

use std::collections::HashMap;
use std::sync::OnceLock;

const BOLTS: &str = include_str!("../../data/steel/bolts.csv");
const SPACING: &str = include_str!("../../data/steel/spacing.csv");
const WELDS: &str = include_str!("../../data/steel/welds.csv");
const PLATES: &str = include_str!("../../data/steel/plates.csv");
const CONNECTIONS: &str = include_str!("../../data/steel/connections.csv");

const MM: f64 = 1e-3;

/// One bolt set (bolt, nut, two washers) of the series, metres.
#[derive(Clone, Debug, PartialEq)]
pub struct BoltSize {
    pub d: f64,
    pub pitch: f64,
    /// Tensile stress area, m2.
    pub area: f64,
    pub s: f64,
    pub k: f64,
    pub m: f64,
    pub w_od: f64,
    pub w_id: f64,
    pub w_t: f64,
    pub hole: f64,
    pub oversize: f64,
    pub anchor_hole: f64,
    /// Square plate washer of an anchor rod: side and thickness.
    pub plate_washer: f64,
    pub plate_washer_t: f64,
}

impl BoltSize {
    /// `M20`.
    pub fn name(&self) -> String {
        format!("M{}", fmt_mm(self.d))
    }
    /// Radius of the hexagon corners.
    pub fn hex_r(&self) -> f64 {
        self.s / 3f64.sqrt()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoltClass {
    pub name: String,
    /// Ultimate and yield strength, Pa.
    pub fub: f64,
    pub fyb: f64,
    pub shear_alpha: f64,
}

/// Spacing rules: `*_min`, `*_pref` are multiples of the hole diameter.
#[derive(Clone, Debug, PartialEq)]
pub struct Spacing {
    pub e1_min: f64,
    pub e2_min: f64,
    pub p1_min: f64,
    pub p2_min: f64,
    pub e_pref: f64,
    pub p_pref: f64,
    pub round: f64,
    pub e_max_t: f64,
    pub e_max_add: f64,
    pub p_max_t: f64,
    pub p_max_abs: f64,
    pub face_clear: f64,
    /// Thread beyond the nut, in pitches.
    pub protrusion: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WeldRules {
    /// (thicker part up to, minimum leg).
    pub min_leg: Vec<(f64, f64)>,
    pub legs: Vec<f64>,
    pub leg_max_t: f64,
    pub len_min: f64,
    pub len_min_leg: f64,
    /// Weld metal ultimate strength, Pa.
    pub fu: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlateRules {
    pub thickness: Vec<f64>,
    pub size_round: f64,
    pub snip: f64,
    pub min_t: f64,
    pub stiff_ratio: f64,
    pub ratio_max: f64,
    pub grade: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SphereBolt {
    pub tube_max: f64,
    pub d: f64,
    pub sleeve_s: f64,
    pub sleeve_len: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Defaults {
    pub bolt_by_depth: Vec<(f64, f64)>,
    pub end_plate_t: Vec<(f64, f64)>,
    /// (largest section side up to, rod diameter, rod count).
    pub anchors: Vec<(f64, f64, usize)>,
    pub balls: Vec<f64>,
    pub sphere_bolt: Vec<SphereBolt>,
    pub grout: f64,
    /// Rod length below the grout, in rod diameters.
    pub anchor_embed: f64,
    /// Rod thread above the nut, in pitches.
    pub anchor_above: f64,
    pub gap: f64,
    pub bolt_class: String,
    pub anchor_class: String,
    pub fin_depth_share: f64,
    /// Cone length in tube diameters.
    pub cone_len: f64,
    /// Free angle between two sleeves on a ball, rad.
    pub ball_gap: f64,
    /// Thread depth in the ball, in bolt diameters.
    pub ball_thread: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Catalog {
    pub bolts: Vec<BoltSize>,
    pub classes: Vec<BoltClass>,
    pub lengths: Vec<f64>,
    pub spacing: Spacing,
    pub weld: WeldRules,
    pub plate: PlateRules,
    pub conn: Defaults,
}

/// A table of one `[section]`: header names and rows of cells.
struct Table<'a> {
    head: Vec<&'a str>,
    rows: Vec<Vec<&'a str>>,
}

impl<'a> Table<'a> {
    fn col(&self, name: &str) -> Result<usize, String> {
        self.head
            .iter()
            .position(|h| *h == name)
            .ok_or_else(|| format!("no column {name}"))
    }
    fn nums(&self, name: &str) -> Result<Vec<f64>, String> {
        let c = self.col(name)?;
        self.rows
            .iter()
            .map(|r| num(r.get(c).copied().unwrap_or("")))
            .collect()
    }
    /// `key,value,unit` rows.
    fn rules(&self) -> Result<HashMap<&'a str, &'a str>, String> {
        let (k, v) = (self.col("key")?, self.col("value")?);
        Ok(self
            .rows
            .iter()
            .map(|r| (r[k], r.get(v).copied().unwrap_or("")))
            .collect())
    }
}

fn num(s: &str) -> Result<f64, String> {
    s.trim()
        // num-lint: bundled catalog file, invariant form
        .parse::<f64>()
        .ok()
        .filter(|x| x.is_finite())
        .ok_or_else(|| format!("not a number: {s:?}"))
}

fn sections(src: &str) -> Result<HashMap<&str, Table<'_>>, String> {
    let mut out: HashMap<&str, Table> = HashMap::new();
    let mut cur: Option<&str> = None;
    for line in src.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some(name) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            cur = Some(name);
            continue;
        }
        let name = cur.ok_or_else(|| format!("row outside a section: {l}"))?;
        let cells: Vec<&str> = l.split(',').map(str::trim).collect();
        match out.get_mut(name) {
            None => {
                out.insert(
                    name,
                    Table {
                        head: cells,
                        rows: vec![],
                    },
                );
            }
            Some(t) => {
                if cells.len() != t.head.len() {
                    return Err(format!("[{name}] row has {} cells: {l}", cells.len()));
                }
                t.rows.push(cells);
            }
        }
    }
    Ok(out)
}

fn table<'a, 'b>(s: &'b HashMap<&str, Table<'a>>, name: &str) -> Result<&'b Table<'a>, String> {
    s.get(name).ok_or_else(|| format!("no section [{name}]"))
}

fn rule(r: &HashMap<&str, &str>, key: &str) -> Result<f64, String> {
    num(r.get(key).ok_or_else(|| format!("no rule {key}"))?)
}

fn text_rule(r: &HashMap<&str, &str>, key: &str) -> Result<String, String> {
    Ok(r.get(key)
        .ok_or_else(|| format!("no rule {key}"))?
        .to_string())
}

fn pairs(t: &Table, a: &str, b: &str, ka: f64, kb: f64) -> Result<Vec<(f64, f64)>, String> {
    let (x, y) = (t.nums(a)?, t.nums(b)?);
    Ok(x.into_iter()
        .zip(y)
        .map(|(p, q)| (p * ka, q * kb))
        .collect())
}

fn ascending(v: &[f64], what: &str) -> Result<(), String> {
    if v.windows(2).all(|w| w[0] < w[1]) && !v.is_empty() {
        Ok(())
    } else {
        Err(format!("{what} must ascend"))
    }
}

/// Parses the five files; an error names the file problem.
pub fn parse() -> Result<Catalog, String> {
    let b = sections(BOLTS)?;
    let bt = table(&b, "bolts")?;
    let mut bolts = vec![];
    for r in &bt.rows {
        let g = |n: &str| -> Result<f64, String> { num(r[bt.col(n)?]) };
        bolts.push(BoltSize {
            d: g("d")? * MM,
            pitch: g("pitch")? * MM,
            area: g("as")? * MM * MM,
            s: g("s")? * MM,
            k: g("k")? * MM,
            m: g("m")? * MM,
            w_od: g("w_od")? * MM,
            w_id: g("w_id")? * MM,
            w_t: g("w_t")? * MM,
            hole: g("hole")? * MM,
            oversize: g("oversize")? * MM,
            anchor_hole: g("anchor_hole")? * MM,
            plate_washer: g("plate_washer")? * MM,
            plate_washer_t: g("plate_washer_t")? * MM,
        });
    }
    ascending(
        &bolts.iter().map(|b| b.d).collect::<Vec<_>>(),
        "bolt diameters",
    )?;
    let ct = table(&b, "classes")?;
    let (cn, fub, fyb, sa) = (
        ct.col("class")?,
        ct.nums("fub")?,
        ct.nums("fyb")?,
        ct.nums("shear_alpha")?,
    );
    let classes = ct
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| BoltClass {
            name: r[cn].to_string(),
            fub: fub[i] * 1e6,
            fyb: fyb[i] * 1e6,
            shear_alpha: sa[i],
        })
        .collect();
    let lengths: Vec<f64> = table(&b, "lengths")?
        .nums("len")?
        .iter()
        .map(|x| x * MM)
        .collect();
    ascending(&lengths, "bolt lengths")?;

    let s = sections(SPACING)?;
    let r = table(&s, "rules")?.rules()?;
    let spacing = Spacing {
        e1_min: rule(&r, "e1_min")?,
        e2_min: rule(&r, "e2_min")?,
        p1_min: rule(&r, "p1_min")?,
        p2_min: rule(&r, "p2_min")?,
        e_pref: rule(&r, "e_pref")?,
        p_pref: rule(&r, "p_pref")?,
        round: rule(&r, "round")? * MM,
        e_max_t: rule(&r, "e_max_t")?,
        e_max_add: rule(&r, "e_max_add")? * MM,
        p_max_t: rule(&r, "p_max_t")?,
        p_max_abs: rule(&r, "p_max_abs")? * MM,
        face_clear: rule(&r, "face_clear")? * MM,
        protrusion: rule(&r, "protrusion")?,
    };

    let w = sections(WELDS)?;
    let wr = table(&w, "rules")?.rules()?;
    let legs: Vec<f64> = table(&w, "legs")?
        .nums("leg")?
        .iter()
        .map(|x| x * MM)
        .collect();
    ascending(&legs, "weld legs")?;
    let weld = WeldRules {
        min_leg: pairs(table(&w, "min_leg")?, "t_max", "leg", MM, MM)?,
        legs,
        leg_max_t: rule(&wr, "leg_max_t")?,
        len_min: rule(&wr, "len_min_mm")? * MM,
        len_min_leg: rule(&wr, "len_min_leg")?,
        fu: rule(&wr, "fu")? * 1e6,
    };

    let p = sections(PLATES)?;
    let pr = table(&p, "rules")?.rules()?;
    let thickness: Vec<f64> = table(&p, "thickness")?
        .nums("t")?
        .iter()
        .map(|x| x * MM)
        .collect();
    ascending(&thickness, "plate thicknesses")?;
    let plate = PlateRules {
        thickness,
        size_round: rule(&pr, "size_round")? * MM,
        snip: rule(&pr, "snip")? * MM,
        min_t: rule(&pr, "min_t")? * MM,
        stiff_ratio: rule(&pr, "stiff_ratio")?,
        ratio_max: rule(&pr, "ratio_max")?,
        grade: text_rule(&pr, "grade")?,
    };

    let c = sections(CONNECTIONS)?;
    let cr = table(&c, "rules")?.rules()?;
    let at = table(&c, "anchors")?;
    let (asz, ad, acount) = (at.nums("size_max")?, at.nums("d")?, at.nums("count")?);
    let st = table(&c, "sphere_bolt")?;
    let (tm, sd, ss, sl) = (
        st.nums("tube_max")?,
        st.nums("d")?,
        st.nums("sleeve_s")?,
        st.nums("sleeve_len")?,
    );
    let balls: Vec<f64> = table(&c, "balls")?
        .nums("d")?
        .iter()
        .map(|x| x * MM)
        .collect();
    ascending(&balls, "ball diameters")?;
    let conn = Defaults {
        bolt_by_depth: pairs(table(&c, "bolt_by_depth")?, "depth_max", "d", MM, MM)?,
        end_plate_t: pairs(table(&c, "end_plate_t")?, "d", "t", MM, MM)?,
        anchors: (0..asz.len())
            .map(|i| (asz[i] * MM, ad[i] * MM, acount[i] as usize))
            .collect(),
        balls,
        sphere_bolt: (0..tm.len())
            .map(|i| SphereBolt {
                tube_max: tm[i] * MM,
                d: sd[i] * MM,
                sleeve_s: ss[i] * MM,
                sleeve_len: sl[i] * MM,
            })
            .collect(),
        grout: rule(&cr, "grout")? * MM,
        anchor_embed: rule(&cr, "anchor_embed")?,
        anchor_above: rule(&cr, "anchor_above")?,
        gap: rule(&cr, "gap")? * MM,
        bolt_class: text_rule(&cr, "bolt_class")?,
        anchor_class: text_rule(&cr, "anchor_class")?,
        fin_depth_share: rule(&cr, "fin_depth_share")?,
        cone_len: rule(&cr, "cone_len")?,
        ball_gap: rule(&cr, "ball_gap_deg")?.to_radians(),
        ball_thread: rule(&cr, "ball_thread")?,
    };
    let cat = Catalog {
        bolts,
        classes,
        lengths,
        spacing,
        weld,
        plate,
        conn,
    };
    for (_, d) in &cat.conn.bolt_by_depth {
        cat.bolt(*d)
            .ok_or("bolt_by_depth names a bolt outside the series")?;
    }
    for a in &cat.conn.anchors {
        cat.bolt(a.1)
            .ok_or("anchors name a rod outside the series")?;
    }
    for s in &cat.conn.sphere_bolt {
        cat.bolt(s.d)
            .ok_or("sphere_bolt names a bolt outside the series")?;
    }
    for c in [&cat.conn.bolt_class, &cat.conn.anchor_class] {
        cat.class(c).ok_or("default class is not in [classes]")?;
    }
    Ok(cat)
}

/// The catalogue (parsed once; the data is compiled in and covered by tests).
pub fn catalog() -> &'static Catalog {
    static CAT: OnceLock<Catalog> = OnceLock::new();
    CAT.get_or_init(|| parse().expect("steel catalogue data"))
}

/// `20` for 0.020, `22.5` for 0.0225.
pub fn fmt_mm(x: f64) -> String {
    let mm = (x / MM * 10.0).round() / 10.0;
    if (mm - mm.round()).abs() < 1e-9 {
        format!("{}", mm.round() as i64)
    } else {
        format!("{mm}")
    }
}

fn ceil_to(x: f64, step: f64) -> f64 {
    (x / step - 1e-9).ceil() * step
}

impl Catalog {
    /// The bolt of diameter `d` (within 0.05 mm).
    pub fn bolt(&self, d: f64) -> Option<&BoltSize> {
        self.bolts.iter().find(|b| (b.d - d).abs() < 5e-5)
    }

    pub fn class(&self, name: &str) -> Option<&BoltClass> {
        self.classes.iter().find(|c| c.name == name.trim())
    }

    /// The next smaller bolt of the series.
    pub fn smaller(&self, d: f64) -> Option<&BoltSize> {
        self.bolts.iter().rev().find(|b| b.d < d - 5e-5)
    }

    /// First stock length covering `need`.
    pub fn bolt_length(&self, need: f64) -> Option<f64> {
        self.lengths.iter().copied().find(|l| *l >= need - 1e-9)
    }

    pub fn bolt_for_depth(&self, depth: f64) -> f64 {
        let t = &self.conn.bolt_by_depth;
        t.iter()
            .find(|r| depth <= r.0 + 1e-9)
            .or(t.last())
            .map_or(0.02, |r| r.1)
    }

    /// Thinnest plate of the series not thinner than `t`.
    pub fn plate_t(&self, t: f64) -> f64 {
        let s = &self.plate.thickness;
        let t = t.max(self.plate.min_t);
        s.iter()
            .copied()
            .find(|x| *x >= t - 1e-9)
            .unwrap_or(*s.last().unwrap_or(&t))
    }

    pub fn end_plate_t(&self, d: f64) -> f64 {
        let t = &self.conn.end_plate_t;
        t.iter()
            .find(|r| (r.0 - d).abs() < 5e-5 || r.0 > d)
            .or(t.last())
            .map_or(0.02, |r| r.1)
    }

    /// Anchor rod diameter and count for a section whose larger side is `size`.
    pub fn anchors_for(&self, size: f64) -> (f64, usize) {
        let t = &self.conn.anchors;
        t.iter()
            .find(|r| size <= r.0 + 1e-9)
            .or(t.last())
            .map_or((0.024, 4), |r| (r.1, r.2))
    }

    pub fn sphere_bolt(&self, tube_d: f64) -> &SphereBolt {
        let t = &self.conn.sphere_bolt;
        t.iter()
            .find(|r| tube_d <= r.tube_max + 1e-9)
            .unwrap_or(&t[t.len() - 1])
    }

    /// Smallest ball not smaller than `need` (diameter).
    pub fn ball_d(&self, need: f64) -> Option<f64> {
        self.conn.balls.iter().copied().find(|d| *d >= need - 1e-9)
    }

    /// Minimum fillet leg for parts `a`, `b` thick; None when the thinner part cannot take it.
    pub fn weld_leg(&self, a: f64, b: f64) -> Option<f64> {
        let (thick, thin) = (a.max(b), a.min(b));
        let r = &self.weld;
        let min = r
            .min_leg
            .iter()
            .find(|x| thick <= x.0 + 1e-9)
            .map_or(0.01, |x| x.1);
        let leg = r.legs.iter().copied().find(|l| *l >= min - 1e-9)?;
        (leg <= r.leg_max_t * thin + 1e-9).then_some(leg)
    }

    /// Like [`Self::weld_leg`] but never refuses: the largest leg the thinner part takes.
    pub fn weld_leg_fit(&self, a: f64, b: f64) -> f64 {
        self.weld_leg(a, b).unwrap_or_else(|| {
            let cap = self.weld.leg_max_t * a.min(b);
            let r = &self.weld.legs;
            r.iter()
                .rev()
                .copied()
                .find(|l| *l <= cap + 1e-9)
                .unwrap_or(r[0])
        })
    }

    pub fn weld_len_min(&self, leg: f64) -> f64 {
        self.weld.len_min.max(self.weld.len_min_leg * leg)
    }

    pub fn round_up(&self, x: f64) -> f64 {
        ceil_to(x, self.spacing.round)
    }

    pub fn size_up(&self, x: f64) -> f64 {
        ceil_to(x, self.plate.size_round)
    }

    /// Layout end/edge distance for hole `d0`.
    pub fn e_layout(&self, d0: f64) -> f64 {
        self.round_up(self.spacing.e_pref * d0)
    }

    /// Layout pitch for hole `d0`.
    pub fn p_layout(&self, d0: f64) -> f64 {
        self.round_up(self.spacing.p_pref * d0)
    }

    /// Bolt axis to a face standing across it: washer radius plus wrench room.
    pub fn face_clear(&self, b: &BoltSize) -> f64 {
        b.w_od / 2.0 + self.spacing.face_clear
    }

    /// Largest edge distance for an outer ply `t` thick.
    pub fn e_max(&self, t: f64) -> f64 {
        self.spacing.e_max_t * t + self.spacing.e_max_add
    }

    /// Largest pitch for an outer ply `t` thick.
    pub fn p_max(&self, t: f64) -> f64 {
        (self.spacing.p_max_t * t).min(self.spacing.p_max_abs)
    }

    /// Bolt length under the head for a grip: two washers, the nut and the thread beyond it.
    pub fn bolt_need(&self, b: &BoltSize, grip: f64) -> f64 {
        grip + 2.0 * b.w_t + b.m + self.spacing.protrusion * b.pitch
    }
}
