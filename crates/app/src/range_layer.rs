//! The view range and the drawing quality styles on the live plan. Every element kind the model
//! lists in `plan_bands` follows the range of its level: outside it the element is not drawn,
//! overhead ones are drawn with a pattern, the rest takes the pen of its band (cut or projection
//! style, halftone). Nothing changes until the level has a saved range or the project has custom
//! pens, libraries or category styles.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ddc_geom::V2;
use ddc_model::drawq::{
    screen_px, Band, DrawLook, HatchLib, HatchPattern, LinePattern, PenTable, HALFTONE_K,
};
use ddc_model::ViewKey;
use egui::{self, Color32, Pos2, Stroke};

use crate::editor::bodies::BodyKind;
use crate::editor::{Editor, Sel};
use crate::grips::Kind;
use crate::isolate::Part;
use crate::ui_kit::Tokens;
use crate::view_gfx_ui::GfxLayer;

/// Screen pixels per millimetre of paper (96 dpi).
pub const PX_PER_MM: f64 = 96.0 / 25.4;

/// Outline of an element drawn here with a line pattern.
#[derive(Clone, Debug)]
pub struct Dashed {
    pub rings: Vec<Vec<V2>>,
    pub px: f32,
    pub half: bool,
    pub color: Option<u32>,
    pub pattern: LinePattern,
}

#[derive(Clone, Debug, Default)]
pub struct RangeLayer {
    /// Outside the range, or drawn by [`paint_plan`] instead of the ordinary loops.
    pub hide: HashSet<Sel>,
    pub hide_parts: HashSet<Part>,
    pub dashed: Vec<Dashed>,
    /// View graphics with the pens of the styles merged in; None when nothing is merged.
    pub gfx: Option<Arc<GfxLayer>>,
}

/// What to do with one element.
#[derive(Clone, Debug, PartialEq)]
pub enum Draw {
    /// Outside the range.
    Hide,
    /// Keep the ordinary look.
    Plain,
    Pen { px: f32, half: bool },
    Pattern { px: f32, half: bool, color: Option<u32>, pattern: LinePattern },
}

pub struct Ctx<'a> {
    pub look: &'a DrawLook,
    /// The level has a saved range: elements outside it vanish, overhead ones are dashed.
    pub ranged: bool,
    pub custom_pens: bool,
    pub denom: u32,
    pub thin: bool,
}

impl Ctx<'_> {
    /// Look of an element of `category` in `band`. Cut lines keep a solid line on screen (the
    /// poche is drawn by the ordinary loops); patterns of cut styles apply on sheets.
    pub fn decide(&self, category: &str, band: Band) -> Draw {
        let band = if self.ranged {
            band
        } else {
            match band {
                Band::Overhead | Band::Beyond | Band::Hidden => Band::Seen,
                b => b,
            }
        };
        if band == Band::Hidden {
            return Draw::Hide;
        }
        let ov = self.look.styles.get(category);
        let styled = ov.is_some() || self.custom_pens || matches!(band, Band::Overhead | Band::Beyond);
        if !styled {
            return Draw::Plain;
        }
        let r = self.look.line(category, band, self.denom);
        let px = screen_px(r.mm, PX_PER_MM as f32, self.thin);
        let pattern = self.look.patterns.get_or_solid(&r.pattern);
        if band != Band::Cut && !pattern.solid() {
            Draw::Pattern {
                px,
                half: r.halftone,
                color: r.color,
                pattern,
            }
        } else {
            Draw::Pen {
                px,
                half: r.halftone,
            }
        }
    }
}

/// Selection key of a plan table; kinds drawn as parts of the 3D model have none.
fn sel_for(table: &str, id: i64) -> Option<Sel> {
    crate::section_view::sel_of(table, id)
        .or_else(|| BodyKind::from_table(table).map(|k| Sel::Body(k, id)))
}

fn part_for(table: &str, id: i64) -> Option<Part> {
    let k = match table {
        "roof" => Kind::Roof,
        "stair" => Kind::Stair,
        "railing" => Kind::Railing,
        _ => return None,
    };
    Some((k, id))
}

/// Scale denominator that the zoom stands for: one metre on screen is `px_per_m` px.
pub fn denom_of(px_per_m: f64) -> f64 {
    (1000.0 * PX_PER_MM / px_per_m.max(1e-6)).clamp(1.0, 5000.0)
}

/// Half-octave bucket of a denominator: the layer is rebuilt when the zoom crosses one.
fn bucket(denom: f64) -> i32 {
    (denom.max(1.0).log2() * 2.0).round() as i32
}

fn bucket_denom(b: i32) -> u32 {
    2f64.powf(f64::from(b) / 2.0).round().max(1.0) as u32
}

#[derive(Default)]
struct Cache {
    key: Option<(u64, u64, i64, i32, u64)>,
    layer: Option<Arc<RangeLayer>>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
    /// Hatch library of the project, for `lib:` material hatches.
    static HATCHES: RefCell<HatchLib> = RefCell::default();
}

/// Layer of a plan level at this zoom; None while nothing is customised. Call with the full
/// editor lists, before `Vis::stash`, after the view graphics `base` are known.
pub fn layer(ed: &Editor, level: i64, px_per_m: f64, base: &Arc<GfxLayer>) -> Option<Arc<RangeLayer>> {
    let b = bucket(denom_of(px_per_m));
    let key = (ed.revision, ed.model.change_head(), level, b, ed.view_thin_rev);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.key != Some(key) {
            c.layer = build(ed, level, bucket_denom(b), base).map(Arc::new);
            c.key = Some(key);
        }
        c.layer.clone()
    })
}

fn build(ed: &Editor, level: i64, denom: u32, base: &Arc<GfxLayer>) -> Option<RangeLayer> {
    let m = &ed.model;
    let saved = m.plan_range_saved(level).ok().flatten();
    let look = m.drawq_look(ViewKey::Plan(level)).unwrap_or_default();
    HATCHES.with(|h| *h.borrow_mut() = look.hatches.clone());
    if saved.is_none() && look.is_default() {
        return None;
    }
    let bands = m.plan_bands(level, &saved.unwrap_or_default()).ok()?;
    let cx = Ctx {
        look: &look,
        ranged: saved.is_some(),
        custom_pens: look.pens != PenTable::default(),
        denom,
        thin: base.thin,
    };
    let mut out = RangeLayer::default();
    let mut looks: HashMap<(&'static str, i64), (f32, bool)> = HashMap::new();
    for b in &bands {
        let sel = sel_for(b.table, b.id);
        match cx.decide(b.category, b.band) {
            Draw::Plain => {}
            Draw::Hide => match (sel, part_for(b.table, b.id)) {
                (Some(s), _) => {
                    out.hide.insert(s);
                }
                (None, Some(p)) => {
                    out.hide_parts.insert(p);
                }
                _ => {}
            },
            Draw::Pen { px, half } => {
                looks.insert((b.table, b.id), (px, half));
            }
            Draw::Pattern {
                px,
                half,
                color,
                pattern,
            } => {
                let rings = sel.map(|s| crate::selection::outlines(ed, s)).unwrap_or_default();
                match sel {
                    Some(s) if !rings.is_empty() => {
                        out.hide.insert(s);
                        out.dashed.push(Dashed {
                            rings,
                            px,
                            half,
                            color,
                            pattern,
                        });
                    }
                    _ => {
                        looks.insert((b.table, b.id), (px, half));
                    }
                }
            }
        }
    }
    if !looks.is_empty() {
        out.gfx = Some(Arc::new(base.with_drawq(&looks)));
    }
    Some(out)
}

/// Pieces of a pattern along a polyline in screen px. The pattern is in mm on paper, so the line
/// is measured in mm and the pieces come back in px.
pub fn stroke_screen(p: &LinePattern, pts: &[(f64, f64)], px_per_mm: f64) -> Vec<Vec<(f64, f64)>> {
    let mm: Vec<(f64, f64)> = pts.iter().map(|q| (q.0 / px_per_mm, q.1 / px_per_mm)).collect();
    p.stroke(&mm, px_per_mm)
        .into_iter()
        .map(|piece| piece.into_iter().map(|q| (q.0 * px_per_mm, q.1 * px_per_mm)).collect())
        .collect()
}

/// Draws the elements that took a line pattern; the ordinary loops skip them.
pub fn paint_plan(
    painter: &egui::Painter,
    layer: Option<&RangeLayer>,
    to_s: &dyn Fn(V2) -> Pos2,
    t: &Tokens,
) {
    let Some(l) = layer else { return };
    for d in &l.dashed {
        let base = d.color.map_or(t.projected, crate::view_gfx_ui::color_of);
        let col = if d.half {
            base.gamma_multiply(1.0 - HALFTONE_K)
        } else {
            base
        };
        let st = Stroke::new(d.px, col);
        for r in d.rings.iter().filter(|r| r.len() > 1) {
            let mut pts: Vec<(f64, f64)> = r
                .iter()
                .map(|&p| {
                    let s = to_s(p);
                    (f64::from(s.x), f64::from(s.y))
                })
                .collect();
            pts.push(pts[0]);
            for piece in stroke_screen(&d.pattern, &pts, PX_PER_MM) {
                let line: Vec<Pos2> = piece
                    .iter()
                    .map(|q| Pos2::new(q.0 as f32, q.1 as f32))
                    .collect();
                painter.add(egui::Shape::line(line, st));
            }
        }
    }
}

/// Library hatch of the project by name (user patterns first).
pub fn lib_hatch(name: &str) -> Option<HatchPattern> {
    HATCHES.with(|h| h.borrow().get(name))
}

/// Hatch of a `lib:<name>` material key inside screen rings. Lengths of the pattern are paper
/// millimetres; the screen y axis points down, so the angle is mirrored back.
pub fn paint_lib_hatch(p: &egui::Painter, rings: &[Vec<Pos2>], name: &str, st: Stroke) {
    let Some(h) = lib_hatch(name) else { return };
    let mm: Vec<Vec<(f64, f64)>> = rings
        .iter()
        .map(|r| {
            r.iter()
                .map(|q| (f64::from(q.x) / PX_PER_MM, -f64::from(q.y) / PX_PER_MM))
                .collect()
        })
        .collect();
    let Ok(segs) = h.strokes(&mm, 1000.0) else {
        return;
    };
    let to = |q: (f64, f64)| Pos2::new((q.0 * PX_PER_MM) as f32, (-q.1 * PX_PER_MM) as f32);
    for s in segs {
        p.line_segment([to(s.0), to(s.1)], st);
    }
}

/// Colour used for a halftone line of base colour `c` on the paper (for previews).
pub fn halftone_color(c: Color32) -> Color32 {
    c.gamma_multiply(1.0 - HALFTONE_K)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::drawq::{LineStyle, Pen, StyleOverride};

    fn cx<'a>(look: &'a DrawLook, ranged: bool) -> Ctx<'a> {
        Ctx {
            look,
            ranged,
            custom_pens: look.pens != PenTable::default(),
            denom: 100,
            thin: false,
        }
    }

    #[test]
    fn nothing_changes_for_a_plain_project() {
        let look = DrawLook::default();
        let c = cx(&look, false);
        for b in [Band::Cut, Band::Seen, Band::Overhead, Band::Beyond, Band::Hidden] {
            assert_eq!(c.decide("wall", b), Draw::Plain, "{b:?}");
        }
    }

    #[test]
    fn a_saved_range_hides_dashes_and_lightens() {
        let look = DrawLook::default();
        let c = cx(&look, true);
        assert_eq!(c.decide("beam", Band::Hidden), Draw::Hide);
        assert!(matches!(
            c.decide("beam", Band::Overhead),
            Draw::Pattern { half: false, .. }
        ));
        assert!(matches!(
            c.decide("slab", Band::Beyond),
            Draw::Pen { half: true, .. }
        ));
        assert_eq!(c.decide("wall", Band::Cut), Draw::Plain);
        assert_eq!(c.decide("slab", Band::Seen), Draw::Plain);
    }

    #[test]
    fn without_a_range_nothing_leaves_the_plan() {
        let mut look = DrawLook::default();
        look.styles.insert(
            "wall".into(),
            StyleOverride {
                halftone: true,
                ..Default::default()
            },
        );
        let c = cx(&look, false);
        assert!(matches!(c.decide("wall", Band::Hidden), Draw::Pen { half: true, .. }));
        assert!(matches!(c.decide("wall", Band::Overhead), Draw::Pen { half: true, .. }));
    }

    #[test]
    fn cut_and_projection_styles_are_separate() {
        let mut look = DrawLook::default();
        look.styles.insert(
            "wall".into(),
            StyleOverride {
                cut: Some(LineStyle::new("heavy", "dash")),
                projection: Some(LineStyle::new("hair", "dash")),
                halftone: false,
            },
        );
        let c = cx(&look, true);
        let cut = c.decide("wall", Band::Cut);
        let seen = c.decide("wall", Band::Seen);
        let (Draw::Pen { px: a, .. }, Draw::Pattern { px: b, .. }) = (cut, seen) else {
            panic!("cut stays solid on screen, projection takes the pattern");
        };
        assert!(a > b, "the heavy cut pen is wider than the hair projection pen");
    }

    #[test]
    fn custom_pens_reach_every_category() {
        let mut look = DrawLook::default();
        look.pens.set(Pen {
            name: "bold".into(),
            columns: vec![(100, 1.5)],
        });
        let c = cx(&look, false);
        let Draw::Pen { px, .. } = c.decide("col", Band::Cut) else {
            panic!("styled");
        };
        assert!((px - 1.5 * PX_PER_MM as f32).abs() < 0.01);
    }

    #[test]
    fn thin_lines_floor_the_pen() {
        let mut look = DrawLook::default();
        look.pens.set(Pen {
            name: "bold".into(),
            columns: vec![(100, 1.5)],
        });
        let mut c = cx(&look, false);
        c.thin = true;
        assert_eq!(c.decide("col", Band::Cut), Draw::Pen { px: 1.0, half: false });
    }

    #[test]
    fn zoom_buckets_are_monotonic_and_stable() {
        assert!(denom_of(20.0) > denom_of(200.0));
        assert_eq!(bucket(100.0), bucket(101.0));
        assert!(bucket(50.0) < bucket(100.0));
        let d = bucket_denom(bucket(100.0));
        assert!((90..=112).contains(&d), "{d}");
    }

    #[test]
    fn screen_strokes_follow_paper_millimetres() {
        let p = ddc_model::drawq::PatternLib::default().get_or_solid("dash");
        // 100 mm of paper at 96 dpi
        let w = 100.0 * PX_PER_MM;
        let pieces = stroke_screen(&p, &[(0.0, 0.0), (w, 0.0)], PX_PER_MM);
        assert!(pieces.len() >= 20, "{}", pieces.len());
        let first = &pieces[0];
        let len = (first[first.len() - 1].0 - first[0].0).abs() / PX_PER_MM;
        assert!((len - 3.0).abs() < 1e-6, "a 3 mm dash, got {len}");
        let solid = ddc_model::drawq::PatternLib::default().get_or_solid("solid");
        assert_eq!(stroke_screen(&solid, &[(0.0, 0.0), (w, 0.0)], PX_PER_MM).len(), 1);
    }
}
