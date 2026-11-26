// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Levels: the active level, create/edit/delete (undo/redo), the lower-level underlay,
//! current-level rooms for plan picking, and the 3D scene of all levels.

use super::{footprints, Cmd, Editor};
use crate::status::Severity;
use crate::units::{fmt_num_lang, format_length, Units};
use ddc_geom::{layered::Strip, v2, V2};
use ddc_model::{Error, Model};

use ddc_render::{Scene, SceneMesh};

/// Scene element for bodies of inactive levels: not pickable.
pub const OTHER_LEVEL: u32 = crate::scene_ids::OTHER_LEVEL;
/// Element of site-context bodies in 3D (terrain, pads, masses): drawn, left out of zoom-to-fit.
pub const SITE: u32 = crate::scene_ids::SITE;

#[derive(Clone, Debug, PartialEq)]
pub struct RoomItem {
    pub id: i64,
    pub name: String,
    pub number: String,
    /// [outer, holes...].
    pub rings: Vec<Vec<V2>>,
}

pub(super) fn load_rooms(m: &Model, level: i64) -> Vec<RoomItem> {
    let rows: Vec<(i64, String, String)> = m
        .sql()
        .prepare("SELECT id, name, number FROM room WHERE level=?1 ORDER BY id")
        .and_then(|mut st| {
            st.query_map([level], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect()
        })
        .unwrap_or_default();
    rows.into_iter()
        .filter_map(|(id, name, number)| {
            let q = m.room_quantities(id).ok().flatten()?;
            let rings = q
                .rings
                .iter()
                .map(|r| r.iter().map(|&(x, y)| v2(x, y)).collect())
                .collect();
            Some(RoomItem {
                id,
                name,
                number,
                rings,
            })
        })
        .collect()
}

fn inside(p: V2, ring: &[V2]) -> bool {
    let mut c = false;
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) * (b.x - a.x) / (b.y - a.y) {
            c = !c;
        }
    }
    c
}

/// Level mark in project units and the language's number style, no unit suffix in metres:
/// en `+3.000`, ru `+3,000`, imperial `+9'-10 1/8"`; zero is `±`, negatives use U+2212.
pub fn elevation_mark(z: f64, units: Units, ru: bool) -> String {
    let body = match units {
        Units::Metric => fmt_num_lang(z.abs(), 3, crate::units::lang_of(ru)),
        Units::Imperial => format_length(z.abs(), units),
    };
    if z.abs() < 5e-4 {
        format!("\u{b1}{body}")
    } else if z > 0.0 {
        format!("+{body}")
    } else {
        format!("\u{2212}{body}")
    }
}

impl Editor {
    /// Key of every per-frame read cache: (revision, total_changes). Inside an open transaction
    /// each call is a new key, so nothing is cached: ROLLBACK keeps total_changes.
    pub fn cache_key(&self) -> (u64, u64) {
        static IN_TX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let db = self.model.sql();
        if db.is_autocommit() {
            (self.revision, db.total_changes())
        } else {
            (u64::MAX, IN_TX.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
        }
    }

    /// Snapshot key of a result kept over time (an XLSX preview): the revision is not one, a
    /// write whose reload failed or a level walk leaves it equal over different content.
    pub(crate) fn content_snap(&self) -> Option<ddc_model::ContentMark> {
        crate::project::content_mark(&self.model)
    }

    /// A revision no earlier connection was read under: total_changes restarts with each
    /// connection, so a reused revision could hit rows of the old model.
    pub(crate) fn fresh_revision(&mut self) {
        self.revision = self.revision.max(self.rev_top) + 1;
        self.rev_top = self.revision;
        *self.levels_memo.get_mut().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Puts back a revision saved before a walk over the levels; the walk's numbers stay used.
    pub(crate) fn restore_revision(&mut self, rev: u64) {
        self.rev_top = self.rev_top.max(self.revision);
        self.revision = rev;
    }

    /// Level list, read once per cache key: the status bar and panels call it every frame.
    pub fn levels(&self) -> Vec<(i64, String, f64)> {
        let key = self.cache_key();
        let mut memo = self.levels_memo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((k, v)) = memo.as_ref() {
            if *k == key {
                return v.clone();
            }
        }
        let v = self.model.level_list().unwrap_or_default();
        *memo = Some((key, v.clone()));
        v
    }

    pub fn level_name(&self) -> String {
        self.levels()
            .into_iter()
            .find(|l| l.0 == self.level)
            .map_or_else(String::new, |l| l.1)
    }

    /// The active level exists in the model (after undoing a creation, the first by elevation).
    pub(super) fn ensure_level(&mut self) -> Result<(), Error> {
        let ls = self.model.level_list()?;
        if !ls.iter().any(|l| l.0 == self.level) {
            match ls.first() {
                Some(l) => self.level = l.0,
                None => self.level = self.model.add_level(&crate::tr("project_seed.seed_level", &[]), 0.0)?,
            }
        }
        Ok(())
    }

    /// Make a level active: new elements are placed on it, the plan shows it.
    pub fn set_level(&mut self, id: i64) {
        if id == self.level || !self.levels().iter().any(|l| l.0 == id) {
            return;
        }
        self.reset_draft();
        crate::roof_edge_tools::reset();
        self.drag = None;
        self.selection.clear();
        self.level = id;
        if let Err(e) = self.reload() {
            self.say(Severity::Error, crate::tr("level.err_open", &[("e", &e.to_string())]));
        }
    }

    /// Neighboring level by elevation: +1 above, -1 below.
    pub fn step_level(&mut self, dir: i32) {
        let ls = self.levels();
        let Some(i) = ls.iter().position(|l| l.0 == self.level) else {
            return;
        };
        let j = i as i64 + dir as i64;
        if (0..ls.len() as i64).contains(&j) {
            self.set_level(ls[j as usize].0);
        }
    }

    /// Name and elevation of the next level: the top one + 3 m.
    pub fn next_level_defaults(&self) -> (String, f64) {
        let top = self
            .levels()
            .iter()
            .map(|l| l.2)
            .fold(f64::NEG_INFINITY, f64::max);
        let z = if top.is_finite() { top + 3.0 } else { 0.0 };
        (self.next_level_name(z), (z * 1000.0).round() / 1000.0)
    }

    /// Name of a new level at elevation `z`: continues the name of the nearest level below with its numbering;
    /// below all levels it is a "Basement N" name; taken names are skipped.
    pub fn next_level_name(&self, z: f64) -> String {
        let ls = self.levels();
        let free = |name: &dyn Fn(u64) -> String, from: u64| -> String {
            let mut n = from;
            while ls.iter().any(|l| l.1 == name(n)) {
                n += 1;
            }
            name(n)
        };
        let below = ls
            .iter()
            .filter(|l| l.2 < z)
            .max_by(|a, b| a.2.total_cmp(&b.2));
        let Some(below) = below else {
            let name = |n: u64| crate::tr("level.auto_below", &[("n", n.to_string().as_str())]);
            return free(&name, 1);
        };
        let digits = below
            .1
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .count();
        let (stem, tail) = below.1.split_at(below.1.len() - digits);
        match tail.parse::<u64>() {
            Ok(k) => {
                let width = tail.len();
                let name = |n: u64| format!("{stem}{n:0width$}");
                free(&name, k + 1)
            }
            Err(_) => {
                let name = |n: u64| crate::tr("level.auto_name", &[("n", n.to_string().as_str())]);
                free(&name, ls.iter().filter(|l| l.2 < z).count() as u64 + 1)
            }
        }
    }

    /// A new level (becomes active); one undo.
    pub fn add_level(&mut self, name: &str, elevation: f64) -> Option<i64> {
        match self.try_add_level(name, elevation) {
            Ok(id) => Some(id),
            Err(e) => {
                self.say(Severity::Error, e);
                None
            }
        }
    }

    /// As `add_level`, but a refusal comes back as text and leaves `status` alone.
    pub fn try_add_level(&mut self, name: &str, elevation: f64) -> Result<i64, String> {
        let name = name.trim();
        if name.is_empty() || !elevation.is_finite() {
            return Err(crate::tr("level.need_name", &[]));
        }
        match self.model.add_level(name, elevation) {
            Ok(id) => {
                self.push(Cmd::LevelAdd(name.into(), elevation, id));
                self.level = id;
                self.selection.clear();
                self.reset_draft();
                let stale = self.reload().err().map(|e| e.to_string());
                self.status = Some(crate::editor::stale_join(
                    crate::tr("level.created", &[("name", name)]),
                    stale.as_deref(),
                ));
                Ok(id)
            }
            Err(e) => Err(crate::tr("level.err_create", &[("e", &e.to_string())])),
        }
    }

    /// Rename and/or a new elevation; one undo.
    pub fn edit_level(&mut self, id: i64, name: &str, elevation: f64) {
        if let Err(e) = self.try_edit_level(id, name, elevation) {
            self.say(Severity::Error, e);
        }
    }

    /// As `edit_level`, but a refusal comes back as text and leaves `status` alone.
    pub fn try_edit_level(&mut self, id: i64, name: &str, elevation: f64) -> Result<(), String> {
        self.edit_level_with(id, name, elevation, Self::reload)
    }

    /// The model is already changed: the command is recorded in undo even if the reload fails.
    fn edit_level_with(
        &mut self,
        id: i64,
        name: &str,
        elevation: f64,
        reload: impl FnOnce(&mut Self) -> Result<(), ddc_model::Error>,
    ) -> Result<(), String> {
        let name = name.trim();
        if let Err(e) = self.model.update_level(id, name, elevation) {
            return Err(crate::tr("level.err_edit", &[("e", &e.to_string())]));
        }
        self.push(Cmd::LevelEdit(id, name.into(), elevation));
        self.status = Some(match reload(self) {
            Ok(()) => crate::tr("level.edited", &[("name", name)]),
            Err(e) => crate::tr("level.edited_stale", &[("name", name), ("e", &e.to_string())]),
        });
        Ok(())
    }

    /// Delete an empty level; the last one and an occupied one are not deleted.
    pub fn delete_level(&mut self, id: i64) -> bool {
        let ls = self.levels();
        if ls.len() <= 1 {
            self.say(Severity::Refusal, crate::tr("level.last", &[]));
            return false;
        }
        let lost = self.model.level_attach_walls(id).unwrap_or(0);
        match self.model.delete_level(id) {
            Ok(()) => {
                self.push(Cmd::LevelDelete(id));
                if self.level == id {
                    let k = ls.iter().position(|l| l.0 == id).unwrap_or(0);
                    let other = ls.iter().filter(|l| l.0 != id).nth(k.saturating_sub(1));
                    self.level = other.map_or(self.level, |l| l.0);
                    self.selection.clear();
                }
                let stale = self.reload().err();
                let sev = if stale.is_some() { Severity::Warning } else { Severity::Info };
                let done = if lost > 0 {
                    crate::tr("lv.level.deleted_attach", &[("n", &lost.to_string())])
                } else {
                    crate::tr("level.deleted", &[])
                };
                self.say(
                    sev,
                    crate::editor::stale_join(
                        done,
                        stale.as_ref().map(|e| e.to_string()).as_deref(),
                    ),
                );
                true
            }
            Err(ddc_model::Error::Referenced { by, .. }) => {
                // a ramp can hang from this level without lying on it: name it
                let ramps: Vec<String> = by
                    .iter()
                    .filter(|b| b.0 == "ramp")
                    .map(|b| crate::tr("core_tools.browser.ramp", &[("id", b.1.to_string().as_str())]))
                    .collect();
                // the confirm window carries the summary itself: a toast saying the same
                // while it is open reads as a second, premature answer
                crate::level_ask::open(self, id, by);
                if !ramps.is_empty() {
                    let names = ramps.join(", ");
                    self.say(Severity::Refusal, crate::tr("level.blocked", &[("names", names.as_str())]));
                }
                false
            }
            Err(e) => {
                self.say(Severity::Error, crate::tr("level.err_delete", &[("e", &e.to_string())]));
                false
            }
        }
    }

    pub(super) fn level_redo(&mut self, c: &Cmd) -> Option<Result<Cmd, Error>> {
        Some(match c {
            Cmd::LevelAdd(name, z, old) => {
                let (name, z, old) = (name.clone(), *z, *old);
                self.model.add_level(&name, z).map(|new| {
                    for cmd in &mut self.redo {
                        if let Cmd::LevelEdit(id, ..) | Cmd::LevelDelete(id) = cmd {
                            if *id == old {
                                *id = new;
                            }
                        }
                    }
                    if self.level == old {
                        self.level = new;
                    }
                    let _ = self.reload();
                    Cmd::LevelAdd(name, z, new)
                })
            }
            Cmd::LevelEdit(id, name, z) => self
                .model
                .update_level(*id, name, *z)
                .and_then(|_| self.reload())
                .map(|_| c.clone()),
            Cmd::LevelDelete(id) => self
                .model
                .delete_level(*id)
                .and_then(|_| self.reload())
                .map(|_| c.clone()),
            _ => return None,
        })
    }

    /// The level immediately below the active one (by elevation).
    pub fn level_below(&self) -> Option<(i64, String, f64)> {
        let ls = self.levels();
        let i = ls.iter().position(|l| l.0 == self.level)?;
        i.checked_sub(1).map(|k| ls[k].clone())
    }

    pub(super) fn underlay_rings(&self) -> Result<Vec<Vec<Vec<V2>>>, Error> {
        if !self.underlay_on {
            return Ok(vec![]);
        }
        let Some((below, ..)) = self.level_below() else {
            return Ok(vec![]);
        };
        let ws = self.walls_on(below)?;
        let strips: Vec<Strip> = ws.iter().map(|w| self.strip(w)).collect();
        let bulges: Vec<f64> = ws.iter().map(|w| w.bulge).collect();
        let ids: Vec<i64> = ws.iter().map(|w| w.id).collect();
        let prefs = self.model.join_prefs(&ids).unwrap_or_default();
        Ok(footprints(&strips, &bulges, &prefs, strips.len()))
    }

    pub fn toggle_underlay(&mut self) {
        self.underlay_on = !self.underlay_on;
        self.underlay = self.underlay_rings().unwrap_or_default();
    }

    /// The body under a plan point: of several, the one with the smallest outline (furniture inside a room).
    /// A point inside the convex hull but outside the body projection (the corner of an L-shaped run) is a miss.
    /// `site` means terrain and pads only (picked after rooms), otherwise all others.
    pub fn body_pick(&self, p: V2, site: bool) -> Option<(super::bodies::BodyKind, i64)> {
        let area = |h: &[V2]| {
            (0..h.len())
                .map(|i| {
                    let (a, b) = (h[i], h[(i + 1) % h.len()]);
                    a.x * b.y - b.x * a.y
                })
                .sum::<f64>()
                .abs()
        };
        let range = crate::plan_range::drawn(self);
        self.bodies
            .iter()
            .filter(|b| b.kind != super::bodies::BodyKind::Ceiling)
            .filter(|b| {
                crate::plan_range::classify(&range, self.elevation, b.zmin, b.zmax)
                    != crate::plan_range::RangeClass::Hidden
            })
            .filter(|b| super::bodies::is_site(&b.category) == site)
            .filter(|b| super::bodies::inside_hull(p, &b.hull))
            .filter(|b| super::bodies::covers(b, p))
            .min_by(|a, b| area(&a.hull).total_cmp(&area(&b.hull)))
            .map(|b| (b.kind, b.id))
    }

    /// The ceiling under a plan point (after rooms, otherwise it would cover everything inside a room).
    pub fn ceiling_pick(&self, p: V2) -> Option<i64> {
        self.bodies
            .iter()
            .filter(|b| b.kind == super::bodies::BodyKind::Ceiling)
            .find(|b| super::bodies::inside_hull(p, &b.hull))
            .map(|b| b.id)
    }

    /// The current-level room whose outline contains the point.
    pub fn room_pick(&self, p: V2) -> Option<i64> {
        self.rooms.iter().find_map(|r| {
            let (o, hs) = r.rings.split_first()?;
            (inside(p, o) && !hs.iter().any(|h| inside(p, h))).then_some(r.id)
        })
    }

    /// 3D of all levels: the active one is pickable (like `scene`), the others are shifted by
    /// elevation, with element `OTHER_LEVEL`.
    pub fn scene_all(&mut self) -> Scene {
        let span = ddc_model::perf::span("compose.walls");
        let mut meshes = self.scene().meshes;
        drop(span);
        crate::topo::append_scene(self, &mut meshes);
        let (cur, z0) = (self.level, self.elevation);
        // masses of every level once, relative to the current level
        crate::mass_tools::scene_meshes(&self.model, z0, &mut meshes);
        let others: Vec<(i64, f64)> = self
            .levels()
            .into_iter()
            .filter(|l| l.0 != cur)
            .map(|l| (l.0, l.2))
            .collect();
        if others.is_empty() || self.scene_one_level {
            let mut sc = Scene::shared(meshes);
            sc.exclude_from_focus(SITE);
            return sc;
        }
        // before the level switch: the graphics cache is tied to the revision, and the loop moves it
        let gfx = crate::view_gfx_ui::layer(self, ddc_model::ViewKey::ThreeD);
        let (sel, rev, under) = (self.selection.clone(), self.revision, self.underlay_on);
        self.underlay_on = false;
        for (id, z) in others {
            self.level = id;
            if self.reload().is_err()
                || self.walls.is_empty()
                    && self.slabs.is_empty()
                    && self.bodies.is_empty()
                    && (self.scene_providers
                        || !crate::roof_edge_tools::level_has_bodies(&self.model, id))
            {
                continue;
            }
            let dz = (z - z0) as f32;
            meshes.extend(self.scene().meshes.into_iter().filter_map(|m| {
                let tag = gfx.level_tag(self.sel_of_element(m.element), m.tag)?;
                let m = std::sync::Arc::try_unwrap(m).unwrap_or_else(|a| (*a).clone());
                Some(std::sync::Arc::new(SceneMesh {
                    element: if m.element == SITE { SITE } else { OTHER_LEVEL },
                    tag,
                    positions: m
                        .positions
                        .into_iter()
                        .map(|p| [p[0], p[1], p[2] + dz])
                        .collect(),
                    triangles: m.triangles,
                }))
            }));
        }
        self.level = cur;
        self.underlay_on = under;
        let _ = self.reload();
        self.selection = sel;
        self.restore_revision(rev);
        let mut sc = Scene::shared(meshes);
        sc.exclude_from_focus(SITE);
        sc
    }
}

#[cfg(test)]
#[path = "cache_key_tests.rs"]
mod cache_key_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Editor, Sel};

    #[test]
    fn elevation_mark_table() {
        let m = Units::Metric;
        let i = Units::Imperial;
        let cases = [
            (3.0, m, false, "+3.000"),
            (3.0, m, true, "+3,000"),
            (0.0, m, false, "\u{b1}0.000"),
            (0.0, m, true, "\u{b1}0,000"),
            (-0.2, m, false, "\u{2212}0.200"),
            (-0.2, m, true, "\u{2212}0,200"),
            (1234.5, m, false, "+1,234.500"),
            (1234.5, m, true, "+1\u{a0}234,500"),
            (-0.3048, i, false, "\u{2212}1'-0\""),
            (0.0, i, true, "\u{b1}0'-0\""),
        ];
        for (z, u, ru, want) in cases {
            assert_eq!(elevation_mark(z, u, ru), want, "z={z} ru={ru}");
        }
        // The fraction denominator is a process-wide setting: compare with the same helper.
        let ft = format_length(3.0, i);
        assert!(ft.contains('\''), "{ft}");
        assert_eq!(elevation_mark(3.0, i, false), format!("+{ft}"));
        assert_eq!(elevation_mark(3.0, i, true), format!("+{ft}"));
    }

    fn wall(e: &mut Editor, a: (f64, f64), b: (f64, f64)) {
        let t = e.types[1].id;
        e.model
            .add_wall(&ddc_model::WallSpec {
                a,
                b,
                level: e.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        e.reload().unwrap();
    }

    #[test]
    fn level_op_reload_fail_keeps_undo() {
        let mut e = Editor::new_in_memory().unwrap();
        let first = e.level;
        let n = e.undo.len();
        let id = e.add_level("ZZ", 9.0).unwrap();
        assert_eq!(e.undo.len(), n + 1);
        e.reload().unwrap();
        e.model
            .sql()
            .execute_batch("ALTER TABLE wall_type_layer RENAME TO zz_layer")
            .unwrap();
        assert!(e.reload().is_err());
        assert!(e.delete_level(id), "the model delete committed");
        assert_eq!(e.undo.len(), n + 2, "undo entry kept though the view refresh failed");
        assert_ne!(e.level, 0);
        let _ = first;
    }

    fn done_status(f: impl FnOnce(&mut Editor)) -> String {
        let mut e = Editor::new_in_memory().unwrap();
        f(&mut e);
        e.status.clone().unwrap()
    }

    #[test]
    fn add_level_stale_keeps_done() {
        let ok = done_status(|e| {
            e.add_level("ZZ", 9.0).unwrap();
        });
        let stale = done_status(|e| {
            crate::editor::tests::break_views(e);
            assert!(e.add_level("ZZ", 9.0).is_some());
        });
        assert!(stale.starts_with(&ok), "{stale}");
        assert!(stale.contains(&crate::tr("edst.view_stale", &[("e", "")])), "{stale}");
    }

    #[test]
    fn delete_level_stale_keeps_done() {
        let ok = done_status(|e| {
            let id = e.add_level("ZZ", 9.0).unwrap();
            assert!(e.delete_level(id));
        });
        let stale = done_status(|e| {
            let id = e.add_level("ZZ", 9.0).unwrap();
            crate::editor::tests::break_views(e);
            assert!(e.delete_level(id));
        });
        assert!(stale.starts_with(&ok), "{stale}");
        assert!(stale.contains(&crate::tr("edst.view_stale", &[("e", "")])), "{stale}");
    }

    #[test]
    fn level_edit_stays_undoable_when_reload_fails() {
        let mut e = Editor::new_in_memory().unwrap();
        let l = e.level;
        e.edit_level_with(l, "X", 1.5, |_| {
            Err(ddc_model::Error::Invalid("injected".into()))
        })
        .unwrap();
        e.undo();
        e.reload().unwrap();
        assert_ne!(e.level_name(), "X");
    }

    #[test]
    fn delete_active_level_view_valid_undo_restores() {
        let mut e = Editor::new_in_memory().unwrap();
        let l1 = e.level;
        let l2 = e.add_level("L2", 3.0).unwrap();
        assert_eq!(e.level, l2);
        assert!(e.delete_level(l2));
        assert_eq!(e.level, l1);
        assert!(e.levels().iter().any(|l| l.0 == e.level));
        e.undo();
        assert!(e.levels().iter().any(|l| l.0 == e.level), "view level exists");
        assert!(e.levels().iter().any(|l| l.1 == "L2"), "undo restores the level");
        e.redo();
        assert_eq!(e.levels().len(), 1);
        assert!(e.levels().iter().any(|l| l.0 == e.level));
    }

    #[test]
    fn rename_level_to_taken_name_refused_unchanged() {
        let mut e = Editor::new_in_memory().unwrap();
        let l1 = e.level;
        e.add_level("L2", 3.0).unwrap();
        let before = e.levels();
        assert!(e.try_edit_level(l1, "L2", 0.0).is_err());
        assert_eq!(e.levels(), before, "refused rename writes nothing");
    }

    #[test]
    fn levels_add_edit_delete_undo_redo() {
        let mut e = Editor::new_in_memory().unwrap();
        let l1 = e.level;
        let (n, z) = e.next_level_defaults();
        assert_eq!((n.as_str(), z), (crate::app2_fx::fx("level_floor2").as_str(), 3.0));
        let l2 = e.add_level(&n, z).unwrap();
        assert_eq!(e.level, l2);
        e.edit_level(l2, "Roof", 6.0);
        assert_eq!(e.level_name(), "Roof");
        e.undo();
        assert_eq!(e.level_name(), crate::app2_fx::fx("level_floor2"));
        e.undo();
        assert_eq!((e.levels().len(), e.level), (1, l1));
        e.redo();
        e.redo();
        assert_eq!(e.levels().len(), 2);
        let l2 = e.levels()[1].0;
        assert_eq!(e.levels()[1].1, "Roof");
        assert!(!e.delete_level(l1) || e.levels().len() == 1);
        e.set_level(l2);
        wall(&mut e, (0.0, 0.0), (4.0, 0.0));
        assert!(!e.delete_level(l2), "occupied level stays");
    }

    #[test]
    fn walls_go_to_active_level_and_underlay_shows_below() {
        let mut e = Editor::new_in_memory().unwrap();
        wall(&mut e, (0.0, 0.0), (5.0, 0.0));
        let l2 = e.add_level("Floor 2", 3.0).unwrap();
        assert!(e.walls.is_empty());
        assert_eq!(e.underlay.len(), 1);
        e.toggle_underlay();
        assert!(e.underlay.is_empty());
        e.toggle_underlay();
        wall(&mut e, (0.0, 2.0), (5.0, 2.0));
        assert_eq!(e.walls.len(), 1);
        let sc = e.scene_all();
        let hi = |el: u32| {
            sc.meshes
                .iter()
                .filter(|m| m.element == el)
                .flat_map(|m| m.positions.iter().map(|p| p[2]))
                .fold(f32::MIN, f32::max)
        };
        assert!(
            (hi(OTHER_LEVEL) - 0.0).abs() < 1e-3,
            "lower level under active"
        );
        assert!(sc.meshes.iter().any(|m| m.element == 0));
        assert_eq!((e.level, e.walls.len()), (l2, 1));
    }

    #[test]
    fn room_pick_inside_boundary() {
        let mut e = Editor::new_in_memory().unwrap();
        for (a, b) in [
            ((0.0, 0.0), (6.0, 0.0)),
            ((6.0, 0.0), (6.0, 4.0)),
            ((6.0, 4.0), (0.0, 4.0)),
            ((0.0, 4.0), (0.0, 0.0)),
        ] {
            wall(&mut e, a, b);
        }
        let id = e
            .model
            .add_room(&ddc_model::RoomSpec {
                level: e.level,
                name: "Kitchen".into(),
                number: "1".into(),
                point: (3.0, 2.0),
                height: 2.7,
            })
            .unwrap();
        e.reload().unwrap();
        assert_eq!(e.room_pick(v2(3.0, 2.0)), Some(id));
        assert_eq!(e.room_pick(v2(9.0, 2.0)), None);
        e.set_mode(crate::wall_tool::Mode::Select);
        e.handle(crate::wall_tool::Event::Click(v2(2.0, 1.0)));
        assert_eq!(e.selection, vec![Sel::Room(id)]);
    }

    const KEYS: [&str; 11] = [
        "level.err_open",
        "level.need_name",
        "level.created",
        "level.err_create",
        "level.err_edit",
        "level.edited",
        "level.edited_stale",
        "level.last",
        "level.deleted",
        "level.not_empty",
        "level.err_delete",
    ];

    fn has_cyr(s: &str) -> bool {
        s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c))
    }

    fn is_either(s: &str, key: &str) -> bool {
        s == crate::tr_lang(false, key, &[]) || s == crate::tr_lang(true, key, &[])
    }

    #[test]
    fn level_status_en_has_no_cyrillic() {
        for k in KEYS {
            let s = crate::tr_lang(false, k, &[("e", "x"), ("name", "n")]);
            assert_ne!(s, k, "{k} missing");
            assert!(!has_cyr(&s), "{k}: {s}");
        }
        let mut e = Editor::new_in_memory().unwrap();
        let l = e.level;
        assert!(!e.delete_level(l));
        assert!(is_either(&e.status.clone().unwrap(), "level.last"));
        assert!(e.add_level("  ", 0.0).is_none());
        assert!(is_either(&e.status.clone().unwrap(), "level.need_name"));
    }

    /// Expected texts come from the ru locale file, not from literals.
    #[test]
    fn level_status_ru_from_locale() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/data/i18n/ru/level.json");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        for k in KEYS {
            let want = v[k].as_str().unwrap_or_else(|| panic!("{k} missing in ru/level.json"));
            let want = want.replace("{e}", "x").replace("{name}", "n");
            assert_eq!(crate::tr_lang(true, k, &[("e", "x"), ("name", "n")]), want, "{k}");
        }
    }

    fn seed_names(ru: bool) -> (String, Vec<String>) {
        let mut m = Model::open(":memory:").unwrap();
        let l = Editor::seed_model_in(&mut m, ru).unwrap();
        let level = m.level_list().unwrap().into_iter().find(|x| x.0 == l).unwrap().1;
        let mut st = m.sql().prepare("SELECT name FROM wall_type ORDER BY id").unwrap();
        let types = st.query_map([], |r| r.get::<_, String>(0)).unwrap().map(Result::unwrap).collect();
        (level, types)
    }

    #[test]
    fn seed_names_en_no_cyrillic() {
        let (level, types) = seed_names(false);
        assert_eq!(level, "Level 1");
        assert_eq!(types.len(), 3);
        for n in std::iter::once(&level).chain(&types) {
            assert!(!n.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)), "{n}");
        }
    }

    #[test]
    fn seed_names_ru_unchanged() {
        let (level, types) = seed_names(true);
        assert_eq!(level, "\u{42d}\u{442}\u{430}\u{436} 1");
        assert_eq!(types.len(), 3);
        let ru: Vec<String> = ["partition", "masonry", "exterior"]
            .iter()
            .map(|k| crate::tr_lang(true, &format!("project_seed.seed_wall_{k}"), &[]))
            .collect();
        assert_eq!(types, ru);
    }

    #[test]
    fn seed_keys_in_both_catalogs() {
        let cyr = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
        for k in ["seed_level", "seed_wall_partition", "seed_wall_masonry", "seed_wall_exterior"] {
            let key = format!("project_seed.{k}");
            for ru in [true, false] {
                let v = crate::tr_lang(ru, &key, &[]);
                assert_ne!(v, key, "{key} missing (ru={ru})");
                assert_eq!(cyr(&v), ru, "{key}: {v}");
            }
        }
    }

    /// Names already stored in a project stay; the default-level check accepts either language.
    #[test]
    fn stored_seed_names_not_renamed() {
        let mut m = Model::open(":memory:").unwrap();
        let l = Editor::seed_model_in(&mut m, true).unwrap();
        let before = seed_names(true);
        let name = m.level_list().unwrap().into_iter().find(|x| x.0 == l).unwrap().1;
        let ok = crate::editor::default_level(&m, l);
        assert_eq!(name, before.0);
        assert!(ok);
        let mut m2 = Model::open(":memory:").unwrap();
        let l2 = Editor::seed_model_in(&mut m2, false).unwrap();
        assert!(crate::editor::default_level(&m2, l2));
    }

    #[test]
    fn level_keys_defined_once() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/data/i18n");
        for lang in ["en", "ru"] {
            let mut texts = Vec::new();
            for f in std::fs::read_dir(root.join(lang)).unwrap().flatten() {
                if f.path().extension().is_some_and(|x| x == "json") {
                    texts.push(std::fs::read_to_string(f.path()).unwrap());
                }
            }
            for k in KEYS {
                let pat = format!("\"{k}\":");
                let n: usize = texts.iter().map(|t| t.matches(&pat).count()).sum();
                assert_eq!(n, 1, "{lang}: {k} defined {n} times");
            }
        }
    }
}
