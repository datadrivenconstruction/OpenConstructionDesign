// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Temporarily hide/isolate (HH, HI, HR is reset; TOOLS_UI §3): does not change the model, only
//! visibility in plan, sections/elevations and 3D; "Apply to view" turns the temporary set into permanently hidden ones.
//! An opening is visible only together with its host; isolating a wall keeps its openings.

use crate::editor::{Editor, Sel};
use std::collections::HashSet;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Temp {
    #[default]
    Off,
    Hide(HashSet<Sel>),
    Isolate(HashSet<Sel>),
}

/// A 3D element without `Sel` (column, beam, roof, ...), by plan grip kind.
pub type Part = (crate::grips::Kind, i64);

/// The visibility key the 3D scene was built with.
pub type VisKey = (Temp, HashSet<Sel>, HashSet<Part>, HashSet<Part>, u64);

#[derive(Clone, Debug, Default)]
pub struct Vis {
    pub temp: Temp,
    /// Hidden in the view permanently ("Apply to view").
    pub hidden: HashSet<Sel>,
    /// View phase: the future and earlier demolished are hidden, the rest is halftone/dashed.
    pub phase: crate::phase_ui::PhaseLayer,
    /// Elements without `Sel` in the temporary set: hidden (Hide) or kept (Isolate).
    pub parts: HashSet<Part>,
    /// Elements without `Sel` that are permanently hidden.
    pub hidden_parts: HashSet<Part>,
    /// View graphics (VV): hidden by categories and filters, overrides.
    pub gfx: std::sync::Arc<crate::view_gfx_ui::GfxLayer>,
    /// Plan only: view range and drawing quality (outside the range, patterned lines).
    pub range: Option<std::sync::Arc<crate::range_layer::RangeLayer>>,
}

/// Visibility masks over the editor lists; see [`Vis::stash_cached`].
pub struct Masks {
    wk: Vec<bool>,
    ok: Vec<bool>,
    sk: Vec<bool>,
    rk: Vec<bool>,
    ck: Vec<bool>,
    lk: Vec<bool>,
}

/// Mask cache: (key, list lengths, masks) and a rebuild counter.
pub struct MaskCache<K>(pub Option<(K, [usize; 6], Masks)>, pub usize);

impl<K> Default for MaskCache<K> {
    fn default() -> Self {
        Self(None, 0)
    }
}

/// View elements set aside for the frame; see [`Vis::stash`].
pub struct Stash {
    rev: u64,
    walls: Vec<(usize, crate::editor::Wall)>,
    plan: Vec<(usize, Vec<Vec<ddc_geom::V2>>)>,
    openings: Vec<(usize, crate::opening_tool::Placed)>,
    slabs: Vec<(usize, crate::editor::SlabItem)>,
    rooms: Vec<(usize, crate::editor::RoomItem)>,
    curtains: Vec<(usize, crate::editor::CurtainItem)>,
    lines: Vec<(usize, (i64, ddc_geom::V2, ddc_geom::V2))>,
}

impl Vis {
    /// The temporary mode is on (view frame and banner).
    pub fn temp_on(&self) -> bool {
        self.temp != Temp::Off
    }

    pub fn any(&self) -> bool {
        self.temp_on()
            || !self.hidden.is_empty()
            || !self.phase.hide.is_empty()
            || !self.hidden_parts.is_empty()
            || !self.gfx.hide.is_empty()
            || !self.gfx.hide_parts.is_empty()
            || !self.phase.parts.is_empty()
            || self
                .range
                .as_ref()
                .is_some_and(|r| !r.hide.is_empty() || !r.hide_parts.is_empty())
    }

    /// Fingerprint of everything wall visibility depends on; computed from the hidden sets
    /// (not from model walls), element order does not matter.
    pub fn wall_stamp(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let one = |tag: u8, s: &Sel| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (tag, s).hash(&mut h);
            h.finish()
        };
        let sum = |tag: u8, set: &HashSet<Sel>| {
            set.iter()
                .fold(0u64, |a, s| a.wrapping_add(one(tag, s)))
        };
        let mut acc = self.gfx.stamp.rotate_left(17);
        acc = acc.wrapping_add(sum(1, &self.hidden));
        acc = acc.wrapping_add(sum(2, &self.phase.hide));
        if let Some(r) = &self.range {
            acc = acc.wrapping_add(sum(3, &r.hide));
        }
        match &self.temp {
            Temp::Off => {}
            Temp::Hide(h) => acc = acc.wrapping_add(sum(4, h)).wrapping_add(0x9e37),
            Temp::Isolate(k) => acc = acc.wrapping_add(sum(5, k)).wrapping_add(0x79b9),
        }
        acc
    }

    fn range_hides(&self, s: Sel) -> bool {
        self.range.as_ref().is_some_and(|r| r.hide.contains(&s))
    }

    pub fn key(&self) -> VisKey {
        (
            self.temp.clone(),
            self.hidden
                .iter()
                .chain(&self.phase.hide)
                .copied()
                .collect(),
            self.parts.clone(),
            self.hidden_parts
                .union(&self.phase.parts)
                .copied()
                .collect(),
            // what view graphics hide can be large, and the key is computed every frame
            self.gfx.stamp,
        )
    }

    /// Whether an element without `Sel` is visible (column, roof, ...).
    pub fn part_visible(&self, p: Part) -> bool {
        if self.hidden_parts.contains(&p)
            || self.phase.parts.contains(&p)
            || self.gfx.hide_parts.contains(&p)
            || self
                .range
                .as_ref()
                .is_some_and(|r| r.hide_parts.contains(&p))
        {
            return false;
        }
        match &self.temp {
            Temp::Off => true,
            Temp::Hide(_) => !self.parts.contains(&p),
            Temp::Isolate(_) => self.parts.contains(&p),
        }
    }

    fn base(&self, s: Sel) -> bool {
        if self.hidden.contains(&s)
            || self.phase.hide.contains(&s)
            || self.gfx.hide.contains(&s)
            || self.range_hides(s)
        {
            return false;
        }
        match &self.temp {
            Temp::Off => true,
            Temp::Hide(h) => !h.contains(&s),
            Temp::Isolate(k) => k.contains(&s),
        }
    }

    /// Whether an element is visible; an opening only with a visible host.
    pub fn visible(&self, ed: &Editor, s: Sel) -> bool {
        match s {
            Sel::Opening(id) => {
                let host = ed.opening(id).map(|o| Sel::Wall(o.wall));
                let host_ok = host.is_none_or(|h| self.base(h));
                let own = match &self.temp {
                    Temp::Isolate(k) => k.contains(&s) || host.is_some_and(|h| k.contains(&h)),
                    _ => self.base(s),
                };
                host_ok
                    && own
                    && !self.hidden.contains(&s)
                    && !self.phase.hide.contains(&s)
                    && !self.gfx.hide.contains(&s)
                    && !self.range_hides(s)
            }
            Sel::Panel(id, _) => self.base(Sel::Curtain(id)),
            _ => self.base(s),
        }
    }

    pub fn hide(&mut self, sel: &[Sel]) {
        self.hide_parts(sel, &[]);
    }

    /// Hide the selection and elements without `Sel` (plan grips in focus).
    pub fn hide_parts(&mut self, sel: &[Sel], parts: &[Part]) {
        if sel.is_empty() && parts.is_empty() {
            return;
        }
        match &mut self.temp {
            Temp::Hide(h) => {
                h.extend(sel.iter().copied());
                self.parts.extend(parts.iter().copied());
            }
            t => {
                *t = Temp::Hide(sel.iter().copied().collect());
                self.parts = parts.iter().copied().collect();
            }
        }
    }

    pub fn isolate(&mut self, ed: &Editor, sel: &[Sel]) {
        self.isolate_parts(ed, sel, &[]);
    }

    /// Isolate the selection together with elements without `Sel`.
    pub fn isolate_parts(&mut self, ed: &Editor, sel: &[Sel], parts: &[Part]) {
        if sel.is_empty() && parts.is_empty() {
            return;
        }
        self.parts = parts.iter().copied().collect();
        let mut k: HashSet<Sel> = sel.iter().copied().collect();
        // an isolated opening pulls its host along
        for s in sel {
            if let Sel::Opening(id) = s {
                if let Some(o) = ed.opening(*id) {
                    k.insert(Sel::Wall(o.wall));
                }
            }
        }
        self.temp = Temp::Isolate(k);
    }

    /// HR / the X on the banner: the temporary mode is cleared.
    pub fn reset(&mut self) {
        self.temp = Temp::Off;
        self.parts.clear();
    }

    /// "Apply to view": everything now hidden temporarily becomes permanently hidden.
    pub fn apply(&mut self, ed: &Editor) {
        let gone: Vec<Sel> = crate::selection::all(ed)
            .into_iter()
            .filter(|&s| !self.visible(ed, s) && !self.phase.hide.contains(&s))
            .collect();
        self.hidden.extend(gone);
        // hidden elements without `Sel` become permanent; isolation does not list them
        if matches!(self.temp, Temp::Hide(_)) {
            self.hidden_parts.extend(self.parts.drain());
        }
        self.parts.clear();
        self.temp = Temp::Off;
    }

    /// Removes hidden elements from the editor lists for the frame (drawing and picking do not
    /// see them); [`Vis::restore`] returns the lists if the model was not re-read during the frame.
    pub fn stash(&self, ed: &mut Editor) -> Option<Stash> {
        if !self.any() {
            return None;
        }
        let m = self.masks(ed);
        Some(Self::apply_masks(ed, &m))
    }

    /// Same, but visibility masks are computed only when `key` changes (O(n) per edit or
    /// hide change, not per frame); moving the lists out for the frame stays per frame.
    pub fn stash_cached<K: PartialEq + Copy>(
        &self,
        ed: &mut Editor,
        key: K,
        cache: &mut MaskCache<K>,
    ) -> Option<Stash> {
        if !self.any() {
            return None;
        }
        let lens = [
            ed.walls.len(),
            ed.openings.len(),
            ed.slabs.len(),
            ed.rooms.len(),
            ed.curtains.len(),
            ed.curtain_lines.len(),
        ];
        let fresh = cache
            .0
            .as_ref()
            .is_some_and(|(k, l, _)| *k == key && *l == lens);
        if !fresh {
            cache.0 = Some((key, lens, self.masks(ed)));
            cache.1 += 1;
        }
        let m = &cache.0.as_ref()?.2;
        Some(Self::apply_masks(ed, m))
    }

    fn masks(&self, ed: &Editor) -> Masks {
        let keep = |s: Sel| self.visible(ed, s);
        Masks {
            wk: ed.walls.iter().map(|w| keep(Sel::Wall(w.id))).collect(),
            ok: ed
                .openings
                .iter()
                .map(|o| keep(Sel::Opening(o.id)))
                .collect(),
            sk: ed.slabs.iter().map(|o| keep(Sel::Slab(o.id))).collect(),
            rk: ed.rooms.iter().map(|o| keep(Sel::Room(o.id))).collect(),
            ck: ed
                .curtains
                .iter()
                .map(|o| keep(Sel::Curtain(o.id)))
                .collect(),
            lk: ed
                .curtain_lines
                .iter()
                .map(|l| keep(Sel::Curtain(l.0)))
                .collect(),
        }
    }

    fn apply_masks(ed: &mut Editor, m: &Masks) -> Stash {
        let (wk, ok, sk, rk, ck, lk) = (&m.wk, &m.ok, &m.sk, &m.rk, &m.ck, &m.lk);
        fn take<T>(v: &mut Vec<T>, k: &[bool]) -> Vec<(usize, T)> {
            if k.iter().all(|&x| x) {
                return vec![];
            }
            let mut out = vec![];
            for (i, x) in std::mem::take(v).into_iter().enumerate() {
                if k[i] {
                    v.push(x);
                } else {
                    out.push((i, x));
                }
            }
            out
        }
        Stash {
            rev: ed.revision,
            plan: take(&mut ed.plan, wk),
            walls: take(&mut ed.walls, wk),
            openings: take(&mut ed.openings, ok),
            slabs: take(&mut ed.slabs, sk),
            rooms: take(&mut ed.rooms, rk),
            curtains: take(&mut ed.curtains, ck),
            lines: take(&mut ed.curtain_lines, lk),
        }
    }

    /// 3D: without meshes of hidden elements; with isolation other levels go too
    /// (element indices are preserved, so 3D picking is not disturbed).
    pub fn filter_scene(&self, ed: &Editor, sc: ddc_render::Scene) -> ddc_render::Scene {
        self.filter_scene_with(ed, sc, |_| None)
    }

    /// The same; `other` decides for numbers without `Sel` (parts of `edit3d::scene3d`), None means the
    /// default (visible, except under isolation).
    pub fn filter_scene_with(
        &self,
        ed: &Editor,
        sc: ddc_render::Scene,
        other: impl Fn(u32) -> Option<bool>,
    ) -> ddc_render::Scene {
        if !self.any() {
            return sc;
        }
        let iso = matches!(self.temp, Temp::Isolate(_));
        let keep = |e: u32| match ed.sel_of_element(e) {
            Some(s) => self.visible(ed, s),
            None => other(e).unwrap_or(!iso),
        };
        // meshes are shared: pointers with ready bounds are selected, vertices are not copied
        let (meshes, boxes): (Vec<_>, Vec<_>) = sc
            .meshes
            .iter()
            .cloned()
            .zip(sc.boxes().iter().copied())
            .filter(|(m, _)| keep(m.element))
            .unzip();
        let inst = sc
            .instanced
            .into_iter()
            .map(|g| {
                let v: Vec<ddc_render::Inst> =
                    g.inst.iter().filter(|i| keep(i.element)).copied().collect();
                if v.len() == g.inst.len() {
                    return g; // the same list: chunks and the index recognize it
                }
                ddc_render::Instanced {
                    inst: std::sync::Arc::new(v),
                    ..g
                }
            })
            .filter(|g| !g.inst.is_empty())
            .collect();
        ddc_render::Scene::from_parts(meshes, boxes).with_instanced(inst)
    }

    /// Section/elevation: without cuts and edges of hidden elements (axes and levels stay).
    pub fn filter_drawing(&self, ed: &Editor, d: &mut ddc_model::PlanView) {
        if !self.any() {
            return;
        }
        // what plan graphics hide does not concern a section: it has its own view graphics
        if !self.gfx.hide.is_empty() || self.range.is_some() {
            let mut own = self.clone();
            own.gfx = Default::default();
            own.range = None;
            return own.filter_drawing(ed, d);
        }
        d.prims.retain(|q| {
            let el = match q {
                ddc_model::ViewPrim::SectionCut { table, element, .. }
                | ddc_model::ViewPrim::Edge { table, element, .. } => {
                    crate::section_view::sel_of(table, *element)
                }
                _ => None,
            };
            el.is_none_or(|s| self.visible(ed, s))
        });
    }

    pub fn restore(ed: &mut Editor, s: Option<Stash>) {
        let Some(s) = s else { return };
        if ed.revision != s.rev {
            return; // reload already returned the full lists
        }
        fn put<T>(v: &mut Vec<T>, gone: Vec<(usize, T)>) {
            for (i, x) in gone {
                v.insert(i.min(v.len()), x);
            }
        }
        put(&mut ed.walls, s.walls);
        put(&mut ed.plan, s.plan);
        put(&mut ed.openings, s.openings);
        put(&mut ed.slabs, s.slabs);
        put(&mut ed.rooms, s.rooms);
        put(&mut ed.curtains, s.curtains);
        put(&mut ed.curtain_lines, s.lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modify_tools::tests::walls;

    #[test]
    fn hide_isolate_reset_apply() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(
            &mut ed,
            &[((0.0, 0.0), (4.0, 0.0)), ((0.0, 2.0), (4.0, 2.0))],
        );
        let (a, b) = (Sel::Wall(ed.walls[0].id), Sel::Wall(ed.walls[1].id));
        let mut v = Vis::default();
        v.hide(&[a]);
        assert!(!v.visible(&ed, a) && v.visible(&ed, b));
        let st = v.stash(&mut ed);
        assert_eq!(ed.walls.len(), 1);
        Vis::restore(&mut ed, st);
        assert_eq!(ed.walls.len(), 2);
        v.isolate(&ed, &[a]);
        assert!(v.visible(&ed, a) && !v.visible(&ed, b));
        v.reset();
        assert!(v.visible(&ed, b) && !v.temp_on());
        v.hide(&[b]);
        v.apply(&ed);
        assert!(!v.temp_on() && !v.visible(&ed, b) && v.visible(&ed, a));
    }

    #[test]
    fn isolation_filters_3d_scene() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(
            &mut ed,
            &[((0.0, 0.0), (4.0, 0.0)), ((0.0, 2.0), (4.0, 2.0))],
        );
        let a = Sel::Wall(ed.walls[0].id);
        let full = ed.scene_all();
        let n = full.meshes.len();
        let mut v = Vis::default();
        v.isolate(&ed, &[a]);
        let sc = ed.scene_all();
        let sc = v.filter_scene(&ed, sc);
        assert!(!sc.meshes.is_empty() && sc.meshes.len() < n);
        assert!(sc
            .meshes
            .iter()
            .all(|m| ed.sel_of_element(m.element) == Some(a)));
        v.reset();
        let sc = ed.scene_all();
        assert_eq!(v.filter_scene(&ed, sc).meshes.len(), n);
    }

    #[test]
    fn parts_without_sel_hide_isolate_and_apply() {
        use crate::grips::Kind;
        let ed = Editor::new_in_memory().unwrap();
        let col = (Kind::Column, 7);
        let roof = (Kind::Roof, 8);
        let mut v = Vis::default();
        v.hide_parts(&[], &[col]);
        assert!(v.any() && !v.part_visible(col) && v.part_visible(roof));
        let k = v.key();
        v.hide_parts(&[], &[roof]);
        assert_ne!(v.key(), k, "a second hide changes the scene key");
        v.isolate_parts(&ed, &[], &[col]);
        assert!(v.part_visible(col) && !v.part_visible(roof));
        v.reset();
        assert!(v.part_visible(roof) && !v.any());
        v.hide_parts(&[], &[roof]);
        v.apply(&ed);
        assert!(!v.temp_on() && !v.part_visible(roof) && v.part_visible(col) && v.any());
    }
}
