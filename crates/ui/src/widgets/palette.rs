// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Command palette (Ctrl+K): fuzzy search, recent-first on empty query, keyboard navigation.
//! The host adds non-command rows (`extras`: elements, views, saved queries) for the current
//! query; picking one returns its `key` (hosts prefix keys with `@`).
use crate::{fuzzy, icons::IconCache, Catalog, Registry, Theme};
use egui::{Align2, Key, RichText, Sense, Vec2};

pub const RECENT_MAX: usize = 8;
/// Palette geometry (spec 2.13): width, top offset, result row, search field, rows before scrolling.
pub const WIDTH: f32 = 640.0;
pub const TOP: f32 = 96.0;
pub const ROW_H: f32 = 32.0;
pub const FIELD_H: f32 = 40.0;
pub const VISIBLE_ROWS: usize = 9;
const PAD: f32 = 8.0;

/// Height of the scroll area that shows at most `VISIBLE_ROWS` rows.
pub fn list_max_h() -> f32 {
    VISIBLE_ROWS as f32 * ROW_H
}
const ROWS_MAX: usize = 12;

/// Host-provided row: picked → `key` is returned instead of a command id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extra {
    pub key: String,
    pub label: String,
    /// Right-aligned secondary text (count, level, id).
    pub detail: String,
    pub icon: String,
}

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    pub query: String,
    pub sel: usize,
    pub recent: Vec<String>,
    /// Rows for `extras_for` (the query they were built for); above commands if `extras_first`.
    pub extras: Vec<Extra>,
    pub extras_for: Option<String>,
    pub extras_first: bool,
    pub extras_head: String,
    /// Host's reading of the arguments for the highlighted row («300 mm · level 2»), shown
    /// under the query before anything is applied.
    pub hint: String,
    /// Ids the host can run (aliases follow their target); `None` lists every command.
    /// Commands outside it never appear as rows, so a pick cannot end in «unavailable».
    pub live: Option<std::collections::HashSet<String>>,
    /// Host's catch-all row (index into `extras`): Ctrl+Enter picks it from anywhere in the list.
    pub catch_all: Option<usize>,
    /// The catch-all row is shown after every other row, even when extras go first.
    pub catch_all_last: bool,
}

/// An alias runs its target under the same name: the list keeps the first row of each command,
/// so one command is one row (ribbon twins of a tool never show twice).
fn one_row_per_command(reg: &Registry, ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter()
        .filter(|id| seen.insert(reg.resolve(id).to_string()))
        .collect()
}

/// One visible row: a command id or an index into `extras`.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Cmd(String),
    Extra(usize),
}

impl Palette {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.query.clear();
        self.sel = 0;
    }
    /// A command really ran: recent list and usage. The host calls it once per run (picking a
    /// row does not count by itself — the command may refuse to run).
    pub fn note_used(&mut self, id: &str) {
        if id.starts_with('@') {
            return;
        }
        self.recent.retain(|r| r != id);
        self.recent.insert(0, id.to_string());
        self.recent.truncate(RECENT_MAX);
        crate::frecency::note(id);
    }
    /// Rows shown for the current query (ids).
    pub fn rows(&self, reg: &Registry, cat: &Catalog) -> Vec<String> {
        let ok = |id: &str| self.runs(reg, id);
        if self.query.trim().is_empty() {
            let recent = self.recent.iter().filter(|i| ok(i)).cloned();
            let mut v: Vec<String> = one_row_per_command(reg, recent);
            // an alias of a listed command is that command: it never takes a second row
            let has = |v: &[String], id: &str| v.iter().any(|x| reg.resolve(x) == reg.resolve(id));
            let t = crate::frecency::now();
            for id in crate::frecency::with(|u| u.top(t, ROWS_MAX)) {
                if v.len() < ROWS_MAX && !has(&v, &id) && reg.get(&id).is_some() && ok(&id) {
                    v.push(id);
                }
            }
            for c in reg.all() {
                if v.len() >= ROWS_MAX {
                    break;
                }
                if !has(&v, &c.id) && c.stage == "M1" && ok(&c.id) {
                    v.push(c.id.clone());
                }
            }
            return v;
        }
        let v = fuzzy::ranked(reg, cat, &self.query)
            .into_iter()
            .filter(|c| ok(&c.id))
            .map(|c| c.id.clone());
        one_row_per_command(reg, v).into_iter().take(ROWS_MAX).collect()
    }

    /// The host can run `id` (always true while `live` is unset).
    pub fn runs(&self, reg: &Registry, id: &str) -> bool {
        self.live
            .as_ref()
            .is_none_or(|a| a.contains(id) || a.contains(reg.resolve(id)))
    }

    /// Commands and extras (if built for this query); fewer commands beside extras.
    pub fn items(&self, reg: &Registry, cat: &Catalog) -> Vec<Item> {
        let ex = self.extras_for.as_deref() == Some(self.query.as_str());
        let n = if ex { self.extras.len() } else { 0 };
        let tail = self.catch_all.filter(|i| self.catch_all_last && *i < n);
        let extras = (0..n).filter(move |i| Some(*i) != tail).map(Item::Extra);
        let keep = if n > 0 {
            ROWS_MAX.saturating_sub(n).max(4)
        } else {
            ROWS_MAX
        };
        let cmds = self.rows(reg, cat).into_iter().take(keep).map(Item::Cmd);
        let mut v: Vec<Item> = if self.extras_first {
            extras.chain(cmds).collect()
        } else {
            cmds.chain(extras).collect()
        };
        v.extend(tail.map(Item::Extra));
        v
    }

    /// The catch-all row's index while the list shows the host's extras (not the dead-end rows).
    pub fn catch_all_row(&self, reg: &Registry, cat: &Catalog) -> Option<usize> {
        let ex = self.extras_for.as_deref() == Some(self.query.as_str());
        let i = self.catch_all.filter(|i| ex && *i < self.extras.len())?;
        self.items(reg, cat).contains(&Item::Extra(i)).then_some(i)
    }

    /// Next steps for a query nothing matches: find elements by name, search help.
    pub fn fallbacks(&self, cat: &Catalog) -> Vec<Extra> {
        let q = self.query.trim();
        let row = |kind: &str, label: &str, icon: &str| Extra {
            key: format!("@{kind}:{q}"),
            label: cat.t(label).replace("{q}", q),
            detail: String::new(),
            icon: icon.into(),
        };
        vec![
            row("find", "palette.find_named", "find"),
            row("help", "palette.search_help", "help"),
        ]
    }

    /// Visible rows and the extras they index; a dead-end query gets the next-step rows.
    pub fn view(&self, reg: &Registry, cat: &Catalog) -> (Vec<Item>, Vec<Extra>) {
        let rows = self.items(reg, cat);
        if rows.is_empty() && !self.query.trim().is_empty() {
            let fb = self.fallbacks(cat);
            return ((0..fb.len()).map(Item::Extra).collect(), fb);
        }
        (rows, self.extras.clone())
    }

    /// Returns a command id (or an extra's key) when the user picks one.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        th: &Theme,
        cat: &Catalog,
        reg: &Registry,
        icons: &mut IconCache,
    ) -> Option<String> {
        if !self.open {
            return None;
        }
        let (rows, extras) = self.view(reg, cat);
        let key_of = |it: &Item| match it {
            Item::Cmd(id) => id.clone(),
            Item::Extra(i) => extras[*i].key.clone(),
        };
        // taken before plain Enter, or the same press would pick the highlighted row too
        let ctrl_enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::Enter));
        let (up, down, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, Key::Enter),
                i.consume_key(egui::Modifiers::NONE, Key::Escape),
            )
        });
        if esc {
            self.open = false;
            return None;
        }
        if !rows.is_empty() {
            if down {
                self.sel = (self.sel + 1) % rows.len();
            }
            if up {
                self.sel = (self.sel + rows.len() - 1) % rows.len();
            }
            self.sel = self.sel.min(rows.len() - 1);
        }
        let catch_all = self.catch_all_row(reg, cat);
        let mut picked = if ctrl_enter {
            catch_all.map(|i| extras[i].key.clone())
        } else {
            enter.then(|| rows.get(self.sel).map(key_of)).flatten()
        };
        let d = th.d();
        crate::z::cover(egui::Id::new("ddc.palette"), crate::z::Z::Menu)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, TOP))
            .show(ctx, |ui| {
                // dark levels 2-3 carry a 1 px outline: their shadow does not show
                let outline = th.elevation_outline(3).unwrap_or(egui::Stroke::NONE);
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::same(PAD as i8))
                    .corner_radius(th.radius("lg") as u8)
                    .shadow(th.shadow(3))
                    .stroke(outline)
                    .show(ui, |ui| {
                        // the frame's outline takes layout room too: WIDTH is the outer edge
                        ui.set_width(WIDTH - 2.0 * (PAD + outline.width));
                        let before = self.query.clone();
                        let te = egui::TextEdit::singleline(&mut self.query)
                            .hint_text(cat.t("palette.placeholder"))
                            .font(th.font(crate::theme::Role::Title))
                            .frame(false)
                            .margin(egui::Margin::symmetric(8, 0))
                            .vertical_align(egui::Align::Center)
                            .desired_width(f32::INFINITY);
                        ui.add_sized(Vec2::new(ui.available_width(), FIELD_H), te).request_focus();
                        if !self.hint.is_empty() && !self.query.trim().is_empty() {
                            ui.label(
                                RichText::new(&self.hint)
                                    .font(th.font(crate::theme::Role::Numeric))
                                    .color(th.c("text.secondary")),
                            );
                        }
                        ui.separator();
                        if self.query != before {
                            self.sel = 0;
                        }
                        let head = if self.query.trim().is_empty() && !self.recent.is_empty() {
                            "palette.recent"
                        } else {
                            "palette.all"
                        };
                        let section = |ui: &mut egui::Ui, t: &str| {
                            ui.add_space(crate::theme::space::XXS);
                            ui.label(
                                RichText::new(t.to_uppercase())
                                    .font(th.font(crate::theme::Role::Section))
                                    .color(th.c("text.secondary")),
                            );
                        };
                        if rows.is_empty() {
                            section(ui, &cat.t(head));
                            ui.label(
                                RichText::new(cat.t("palette.empty")).color(th.c("text.secondary")),
                            );
                        }
                        let moved = up || down;
                        egui::ScrollArea::vertical()
                            .max_height(list_max_h())
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                        for (i, it) in rows.iter().enumerate() {
                            let ex = matches!(it, Item::Extra(_));
                            if i == 0 || ex != matches!(rows[i - 1], Item::Extra(_)) {
                                if i > 0 {
                                    ui.add_space(crate::theme::space::XS);
                                }
                                let tail = self.catch_all_last
                                    && matches!(it, Item::Extra(k) if Some(*k) == catch_all);
                                if ex && self.items(reg, cat).is_empty() {
                                    section(ui, &cat.t("palette.empty"));
                                } else if tail {
                                    // the catch-all row stands apart, without a section title
                                } else if ex {
                                    section(ui, &self.extras_head);
                                } else {
                                    section(ui, &cat.t(head));
                                }
                            }
                            let (icon, label, keys) = match it {
                                Item::Cmd(id) => {
                                    let Some(cmd) = reg.get(id) else { continue };
                                    let mut keys = cmd.mnemonic.clone().unwrap_or_default();
                                    if let Some(s) = cmd.shortcuts.first() {
                                        keys = if keys.is_empty() {
                                            s.clone()
                                        } else {
                                            format!("{keys}  {s}")
                                        };
                                    }
                                    (cmd.icon.clone(), cat.t(&cmd.label_key()), Some(keys))
                                }
                                Item::Extra(k) => {
                                    let e = &extras[*k];
                                    (e.icon.clone(), e.label.clone(), None)
                                }
                            };
                            let w = ui.available_width();
                            let (rect, resp) =
                                ui.allocate_exact_size(Vec2::new(w, ROW_H), Sense::click());
                            let on = i == self.sel;
                            if on && moved {
                                resp.scroll_to_me(None);
                            }
                            let mut fg = super::paint_state(ui, th, rect, &resp, on);
                            // the ask-AI row reads in the AI role colour
                            if matches!(it, Item::Extra(k) if Some(*k) == catch_all) {
                                fg = th.c("ai.accent");
                            }
                            let ic = egui::Rect::from_min_size(
                                rect.left_center() - Vec2::new(-d.pad_x, d.icon / 2.0),
                                Vec2::splat(d.icon),
                            );
                            icons.paint(ui, &icon, th.icon_px(), ic, fg);
                            let p = ui.painter();
                            let font = egui::TextStyle::Body.resolve(ui.style());
                            let x = ic.right() + d.gap * 2.0;
                            let right = rect.right_center() - Vec2::new(d.pad_x, 0.0);
                            let rw = match (&keys, it) {
                                (Some(k), _) => {
                                    super::paint_keycaps(p, th, right, k);
                                    k.chars().count() as f32 * 8.0
                                }
                                (None, Item::Extra(k)) => p
                                    .text(
                                        right,
                                        Align2::RIGHT_CENTER,
                                        &extras[*k].detail,
                                        th.font(crate::theme::Role::Numeric),
                                        th.c("text.secondary"),
                                    )
                                    .width(),
                                _ => 0.0,
                            };
                            let room = (right.x - rw - d.gap * 2.0 - x).max(40.0);
                            let galley = p.layout_no_wrap(label, font, th.c("text.primary"));
                            let clip = egui::Rect::from_min_max(
                                egui::pos2(x, rect.top()),
                                egui::pos2(x + room, rect.bottom()),
                            );
                            p.with_clip_rect(clip).galley(
                                egui::pos2(x, rect.center().y - galley.size().y / 2.0),
                                galley,
                                th.c("text.primary"),
                            );
                            if resp.clicked() {
                                picked = Some(key_of(it));
                            }
                        }
                            });
                    });
            });
        if picked.is_some() {
            self.open = false;
        }
        picked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_geometry_is_on_the_grid_and_shows_nine_rows() {
        for v in [WIDTH, TOP, ROW_H, FIELD_H, PAD] {
            assert_eq!(v % 4.0, 0.0, "{v}");
        }
        assert_eq!(VISIBLE_ROWS, 9);
        assert_eq!(list_max_h(), 288.0);
    }

    #[test]
    fn palette_is_640_wide_with_32_rows_and_40_field() {
        use crate::theme::{Density, Mode};
        let ctx = crate::theme::headless_ctx();
        let th = Theme::new(Mode::Dark, Density::Comfortable);
        let cat = Catalog::new(crate::Lang::Ru);
        let reg = Registry::builtin();
        let mut icons = IconCache::default();
        let mut p = Palette::default();
        p.open = true;
        let mut w = 0.0;
        let _ = ctx.run(Default::default(), |c| {
            p.show(c, &th, &cat, &reg, &mut icons);
            w = c.memory(|m| m.area_rect(egui::Id::new("ddc.palette"))).map_or(0.0, |r| r.width());
        });
        assert_eq!(w, WIDTH, "frame = content + 2 x pad");
    }

    /// The palette covers the window: on the frame it opens, its fill is opaque, so no text
    /// under it reads through (a fading area is see-through for its first frames).
    #[test]
    fn palette_is_opaque_on_the_frame_it_opens() {
        use crate::theme::{Density, Mode};
        let ctx = crate::theme::headless_ctx();
        let th = Theme::new(Mode::Dark, Density::Comfortable);
        let (cat, reg) = (Catalog::new(crate::Lang::En), Registry::builtin());
        let (mut icons, mut p) = (IconCache::default(), Palette::default());
        p.open = true;
        fn opaque_over(s: &egui::Shape, area: egui::Rect) -> bool {
            match s {
                egui::Shape::Rect(r) => r.fill.a() == 255 && r.rect.expand(1.0).contains_rect(area),
                egui::Shape::Vec(v) => v.iter().any(|x| opaque_over(x, area)),
                _ => false,
            }
        }
        // the first pass of a new area only sizes it and paints nothing
        for pass in 0..3 {
            let out = ctx.run(Default::default(), |c| {
                p.show(c, &th, &cat, &reg, &mut icons);
            });
            if pass == 0 {
                continue;
            }
            let area = ctx.memory(|m| m.area_rect(egui::Id::new("ddc.palette"))).expect("shown");
            let opaque = out.shapes.iter().any(|c| opaque_over(&c.shape, area.shrink(2.0)));
            assert!(opaque, "pass {pass}: no opaque fill over the palette {area:?}");
        }
    }

    #[test]
    fn palette_is_a_single_ai_row_colour_token() {
        use crate::theme::{Density, Mode};
        for m in [Mode::Light, Mode::Dark] {
            let th = Theme::new(m, Density::Comfortable);
            assert_ne!(th.c("ai.accent"), egui::Color32::from_rgb(255, 0, 255));
            assert_ne!(th.c("ai.accent"), th.c("preview"));
        }
    }

    #[test]
    fn palette_recent_and_query() {
        let (r, c) = (Registry::builtin(), Catalog::new(crate::Lang::Ru));
        let mut p = Palette::default();
        for id in ["mod.move", "arch.wall", "mod.move"] {
            p.note_used(id);
        }
        let rows = p.rows(&r, &c);
        assert_eq!(&rows[..2], ["mod.move", "arch.wall"]);
        assert_eq!(rows.len(), ROWS_MAX);
        use crate::test_fx::fx;
        let walls_key = format!("@q:{}", fx("walls"));
        p.query = fx("wall_pre").into();
        assert_eq!(p.rows(&r, &c)[0], "arch.wall");
        p.query = fx("nomatch").into();
        assert!(p.rows(&r, &c).is_empty());
        // extras only for the query they were built for; picked keys skip «recent»
        p.query = fx("wall_pre").into();
        p.extras = vec![Extra {
            key: walls_key.clone(),
            ..Default::default()
        }];
        p.extras_for = Some(fx("wall_pre").into());
        assert_eq!(p.items(&r, &c)[0], Item::Cmd("arch.wall".into()));
        p.extras_first = true;
        let it = p.items(&r, &c);
        assert_eq!(it[0], Item::Extra(0));
        assert!(it.len() <= ROWS_MAX);
        p.query = fx("wall").into();
        assert!(!p.items(&r, &c).contains(&Item::Extra(0)));
        // a dead-end query offers two next steps; Enter on the first searches elements
        let mut z = Palette::default();
        z.query = "zzzz".into();
        let (it, ex) = z.view(&r, &c);
        assert_eq!((it.len(), ex.len()), (2, 2));
        assert_eq!(ex[0].key, "@find:zzzz");
        assert_eq!(ex[1].key, "@help:zzzz");
        assert!(ex[0].label.contains("zzzz") && !ex[1].label.is_empty());
        p.note_used(&walls_key);
        assert!(!p.recent.iter().any(|x| x.starts_with('@')));
    }

    #[test]
    fn dead_commands_are_not_rows() {
        let (r, c) = (Registry::builtin(), Catalog::new(crate::Lang::Ru));
        let mut p = Palette::default();
        p.live = Some(["arch.wall".to_string()].into_iter().collect());
        p.note_used("mod.move");
        assert_eq!(p.rows(&r, &c), ["arch.wall"]);
        p.query = crate::test_fx::fx("wall_pre").into();
        assert_eq!(p.rows(&r, &c), ["arch.wall"]);
        // an alias is live through its target
        let alias = r.all().iter().find_map(|x| x.alias.clone().map(|t| (x.id.clone(), t)));
        let (a, t) = alias.expect("catalog has aliases");
        p.live = Some([t].into_iter().collect());
        assert!(p.runs(&r, &a));
    }
}
