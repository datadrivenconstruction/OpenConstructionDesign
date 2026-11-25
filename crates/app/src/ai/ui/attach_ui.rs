// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Attachments in the chat: the Attach button, files dropped on the panel and images or file
//! paths pasted into the composer become chips (name, size, pages) above the field. A chip
//! opens a preview that shows exactly what the model may read, and can keep the file on this
//! machine. Requirements read out of the files wait on the "project requirements" card until
//! the person edits and confirms them.

use super::kit::{self, space, Button, Dialog, DialogSize};
use super::{Action, Cx, PanelUi};
use crate::ai::attach::{self, AttachData, Requirements};
use ddc_ui::theme::Role;
use egui::{self, RichText, TextEdit, Ui};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Characters of a file's text the preview shows; the model may read more page by page.
const PREVIEW_CHARS: usize = 4000;

#[derive(Default)]
pub struct AttachState {
    pub files: Vec<Arc<AttachData>>,
    pub req: Requirements,
    /// The chip whose preview is open.
    pub preview: Option<u32>,
    /// Files dropped on the chat, waiting for the next frame's actions.
    pub dropped: Vec<PathBuf>,
    /// The chat's area last frame: drops inside it attach instead of opening.
    pub rect: Option<egui::Rect>,
    next: u32,
}

impl AttachState {
    pub fn next_id(&mut self) -> u32 {
        self.next += 1;
        self.next
    }

    /// The same name again replaces the older file; limits count all files of the chat.
    pub fn add(&mut self, d: AttachData) -> Result<(), String> {
        if let Some(i) = self.files.iter().position(|f| f.name == d.name) {
            let old = self.files.remove(i);
            self.req.remove_file(&old.name);
        }
        if self.files.len() >= attach::max_files() {
            let n = self.files.len() + 1;
            return Err(ddc_ai::limits::ATTACH_FILES.refusal(format!("{n} files")));
        }
        let total: u64 = self.files.iter().map(|f| f.size).sum::<u64>() + d.size;
        if total > attach::max_total() {
            let mb = total.div_ceil(1 << 20);
            return Err(ddc_ai::limits::ATTACH_TOTAL.refusal(format!("{mb} MB of attachments")));
        }
        self.req.add_file(&d);
        self.files.push(Arc::new(d));
        Ok(())
    }

    pub fn add_path(&mut self, p: &Path) -> Result<(), String> {
        let id = self.next_id();
        let d = attach::load(p, id)?;
        self.add(d)
    }

    pub fn remove(&mut self, id: u32) {
        if let Some(i) = self.files.iter().position(|f| f.id == id) {
            let f = self.files.remove(i);
            self.req.remove_file(&f.name);
        }
        if self.preview == Some(id) {
            self.preview = None;
        }
    }

    pub fn toggle_share(&mut self, id: u32) {
        if let Some(f) = self.files.iter_mut().find(|f| f.id == id) {
            let d = Arc::make_mut(f);
            d.shared = !d.shared;
        }
    }

    pub fn get(&self, id: u32) -> Option<&Arc<AttachData>> {
        self.files.iter().find(|f| f.id == id)
    }
}

/// "brief.pdf · 1.2 MB · 3 p." — pages only for paged files, rows for tables.
pub fn chip_text(cat: &ddc_ui::Catalog, a: &AttachData) -> String {
    let mut s = format!("{} \u{b7} {}", a.name, attach::size_text(a.size));
    if !a.ex.table.is_empty() {
        s.push_str(" \u{b7} ");
        s.push_str(&cat.tf("ai.att.rows", &[("n", &a.ex.table.len().to_string())]));
    } else if a.pages() > 1 || a.kind == ddc_io::attach::Kind::Pdf {
        s.push_str(" \u{b7} ");
        s.push_str(&cat.tf("ai.att.pages", &[("n", &a.pages().to_string())]));
    }
    s
}

/// Extensions a pasted path may have to be attached rather than typed.
pub fn attachable(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    ddc_io::attach::EXTENSIONS.contains(&ext.as_str()) && p.is_file()
}

/// The Attach button and one chip per file, above the composer.
pub fn chips(ui: &mut Ui, st: &mut PanelUi, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    if !st.attach.dropped.is_empty() {
        acts.push(Action::AttachPaths(std::mem::take(&mut st.attach.dropped)));
    }
    // the attach action is the composer's plus; this row is only the files already attached
    if st.attach.files.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for f in &st.attach.files {
            let mut text = chip_text(cat, f);
            if !f.shared {
                text = format!("\u{1f512} {text}");
            }
            let tip = if f.shared {
                cat.t("ai.att.chip_tip")
            } else {
                cat.t("ai.att.local_tip")
            };
            let r = ui.add(egui::Button::new(RichText::new(text).font(th.font(Role::Caption))).small());
            if r.on_hover_text(tip).clicked() {
                acts.push(Action::AttachPreview(Some(f.id)));
            }
            if ui
                .add(egui::Button::new(RichText::new("\u{d7}").font(th.font(Role::Caption))).small().frame(false))
                .on_hover_text(cat.t("ai.att.remove"))
                .clicked()
            {
                acts.push(Action::AttachRemove(f.id));
            }
        }
    });
}

/// The requirements card under the chips: every fact editable, with its source on hover.
pub fn req_card(ui: &mut Ui, st: &mut PanelUi, cx: &Cx<'_>, acts: &mut Vec<Action>) {
    let (th, cat) = (cx.th, cx.cat);
    if st.attach.req.facts.is_empty() {
        return;
    }
    let order = attach::card_order(&st.attach.req);
    let mut changed = false;
    kit::card(ui, th, |ui| {
        ui.horizontal(|ui| {
            kit::section_title(ui, th, &cat.t("ai.att.req.title"));
            if st.attach.req.confirmed {
                kit::caption(ui, th, &cat.t("ai.att.req.confirmed"));
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("ddc.ai.att.req")
            .max_height(th.d().row * 8.0)
            .show(ui, |ui| {
                for i in order {
                    let rf = &mut st.attach.req.facts[i];
                    ui.horizontal(|ui| {
                        changed |= ddc_ui::widgets::checkbox(ui, th, &mut rf.keep, "")
                            .on_hover_text(cat.t("ai.att.req.keep_tip"))
                            .changed();
                        let kind = cat.t(&format!("ai.att.req.kind.{}", rf.fact.kind.word()));
                        let head = if rf.fact.label.is_empty() {
                            kind
                        } else {
                            format!("{kind}: {}", rf.fact.label)
                        };
                        ui.label(RichText::new(head).font(th.font(Role::Caption)));
                        let w = (ui.available_width() - space::MD).max(60.0);
                        changed |= ui
                            .add(TextEdit::singleline(&mut rf.fact.value).desired_width(w))
                            .on_hover_text(cat.tf("ai.att.req.source", &[("src", &rf.fact.src.show())]))
                            .changed();
                    });
                }
            });
        ui.add_space(space::XS);
        let label = cat.t("ai.att.req.confirm");
        if Button::secondary(&label)
            .small()
            .enabled(!st.attach.req.confirmed)
            .tooltip(&cat.t("ai.att.req.confirm_tip"), "")
            .show(ui, th)
            .clicked()
        {
            acts.push(Action::ReqConfirm);
        }
    });
    if changed {
        // an edit after Confirm needs a new look: the model builds from what was confirmed
        st.attach.req.confirmed = false;
    }
}

/// The preview of one file: what was read, what the model may get, and where it goes.
pub fn preview(ctx: &egui::Context, st: &mut PanelUi, cx: &Cx<'_>, host: &str, acts: &mut Vec<Action>) {
    let Some(id) = st.attach.preview else { return };
    let Some(a) = st.attach.get(id).cloned() else {
        st.attach.preview = None;
        return;
    };
    let (th, cat) = (cx.th, cx.cat);
    let mut open = true;
    Dialog::new("ddc.ai.att.preview", &a.name, DialogSize::Tool)
        .open(&mut open)
        .scroll_body()
        .show(ctx, |ui| {
            kit::caption(ui, th, &chip_text(cat, &a));
            let (chars, image) = attach::sent_size(&a);
            let what = if !a.shared {
                cat.t("ai.att.sent.none")
            } else if image {
                cat.tf("ai.att.sent.text_image", &[("n", &chars.to_string()), ("host", host)])
            } else {
                cat.tf("ai.att.sent.text", &[("n", &chars.to_string()), ("host", host)])
            };
            kit::warn_note(ui, th, &what);
            if let Some(s) = &a.ex.scale {
                kit::caption(ui, th, &cat.tf("ai.att.scale", &[("n", &format!("{}", s.ratio)), ("page", &(s.page + 1).to_string())]));
            }
            for n in &a.ex.notes {
                kit::caption(ui, th, n);
            }
            ui.horizontal(|ui| {
                let share = if a.shared { "ai.att.keep_local" } else { "ai.att.share" };
                if Button::secondary(&cat.t(share)).small().show(ui, th).clicked() {
                    acts.push(Action::AttachShare(id));
                }
                if a.kind.is_drawing()
                    && Button::quiet(&cat.t("ai.att.underlay"))
                        .small()
                        .tooltip(&cat.t("ai.att.underlay_tip"), "")
                        .show(ui, th)
                        .clicked()
                {
                    acts.push(Action::AttachUnderlay(id));
                }
            });
            ui.separator();
            let text = preview_text(&a);
            if text.is_empty() {
                kit::caption(ui, th, &cat.t("ai.att.no_text"));
            } else {
                ui.add(egui::Label::new(RichText::new(text).font(th.font(Role::Small))).wrap());
            }
        });
    if !open {
        st.attach.preview = None;
    }
}

/// The first characters of the text, or the first table rows, as the model would read them.
pub fn preview_text(a: &AttachData) -> String {
    let mut s = String::new();
    if !a.ex.table.is_empty() {
        for r in &a.ex.table {
            s.push_str(&r.join(" | "));
            s.push('\n');
            if s.chars().count() > PREVIEW_CHARS {
                break;
            }
        }
    } else {
        for (i, p) in a.ex.pages.iter().enumerate() {
            if a.ex.pages.len() > 1 {
                s.push_str(&format!("\u{2014} {} \u{2014}\n", i + 1));
            }
            s.push_str(p);
            s.push('\n');
            if s.chars().count() > PREVIEW_CHARS {
                break;
            }
        }
    }
    let cut = s.char_indices().nth(PREVIEW_CHARS).map_or(s.len(), |(i, _)| i);
    s.truncate(cut);
    s.trim_end().to_string()
}

/// Files dropped over the chat: true when they were taken here and must not open as projects.
pub fn take_drop(st: &mut PanelUi, pointer: Option<egui::Pos2>, files: &[PathBuf]) -> bool {
    let over = st.open && pointer.zip(st.attach.rect).is_some_and(|(p, r)| r.contains(p));
    if over && !files.is_empty() {
        st.attach.dropped.extend(files.iter().cloned());
    }
    over
}

/// A pasted text that is a path of an attachable file attaches it and leaves the composer.
pub fn pasted_paths(text: &str) -> Vec<PathBuf> {
    let ps: Vec<PathBuf> = text
        .lines()
        .map(|l| PathBuf::from(l.trim().trim_matches('"')))
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    if !ps.is_empty() && ps.iter().all(|p| attachable(p)) {
        ps
    } else {
        vec![]
    }
}

/// An image on the clipboard, as RGBA; a headless build has no clipboard.
pub fn clipboard_image() -> Option<(u32, u32, Vec<u8>)> {
    #[cfg(feature = "gui")]
    {
        let mut board = arboard::Clipboard::new().ok()?;
        let img = board.get_image().ok()?;
        Some((img.width as u32, img.height as u32, img.bytes.into_owned()))
    }
    #[cfg(not(feature = "gui"))]
    None
}

/// The preview of a read table drawn alone, for the layout contract.
#[cfg(test)]
pub(crate) fn layout_probe(lang: ddc_ui::Lang) -> crate::layout_contract::Probe {
    let cat = ddc_ui::Catalog::new(lang);
    let ed = crate::editor::Editor::new_in_memory().unwrap();
    let mut st = PanelUi::default();
    let id = st.attach.next_id();
    let body = b"Room;Area\nHall;20\nOffice;14\n".to_vec();
    st.attach.add(attach::from_bytes("programme.csv", body, id).unwrap()).unwrap();
    st.attach.preview = Some(id);
    let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
    crate::layout_contract::probe("ddc.ai.att.preview", |ctx| {
        let _ = th.apply(ctx);
        let cx = Cx { th: &th, cat: &cat, ed: &ed, view3d: false, palette_open: false };
        preview(ctx, &mut st, &cx, "localhost", &mut Vec::new());
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn txt(name: &str, body: &str, st: &mut AttachState) -> u32 {
        let id = st.next_id();
        st.add(attach::from_bytes(name, body.as_bytes().to_vec(), id).unwrap()).unwrap();
        id
    }

    #[test]
    fn chips_name_size_and_pages_in_both_languages() {
        let mut st = AttachState::default();
        txt("prog.csv", "Room;Area\nHall;20\n", &mut st);
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let t = chip_text(&cat, &st.files[0]);
            assert!(t.starts_with("prog.csv \u{b7} 1 KB \u{b7} "), "{t}");
            assert!(t.ends_with('2') || t.contains(" 2"), "{t}");
            for k in [
                "ai.att.attach", "ai.att.attach_tip", "ai.att.chip_tip", "ai.att.local_tip", "ai.att.remove",
                "ai.att.rows", "ai.att.pages", "ai.att.req.title", "ai.att.req.confirmed", "ai.att.req.keep_tip",
                "ai.att.req.source", "ai.att.req.confirm", "ai.att.req.confirm_tip", "ai.att.sent.none",
                "ai.att.sent.text", "ai.att.sent.text_image", "ai.att.scale", "ai.att.keep_local", "ai.att.share",
                "ai.att.underlay", "ai.att.underlay_tip", "ai.att.no_text", "ai.att.failed", "ai.att.added",
                "ai.att.drop_hint", "ai.att.req.kind.site", "ai.att.req.kind.storeys", "ai.att.req.kind.room",
                "ai.att.req.kind.total_area", "ai.att.req.kind.height", "ai.att.req.kind.constraint",
                "ai.att.req.kind.norm", "ai.att.req.kind.scale", "ai.tool.attachments_list",
                "ai.tool.attachments_read", "ai.tool.underlay_trace", "ai.tool.plan_from_image",
            ] {
                assert_ne!(cat.t(k), k, "{k} in {lang:?}");
            }
        }
    }

    #[test]
    fn same_name_replaces_and_its_facts_follow() {
        let mut st = AttachState::default();
        txt("b.txt", "Site 10 x 10 m", &mut st);
        txt("b.txt", "Site 20 x 20 m\nKitchen 12 m2", &mut st);
        assert_eq!(st.files.len(), 1);
        assert_eq!(st.req.facts.len(), 2);
        assert_eq!(st.req.facts[0].fact.value, "20 \u{d7} 20 m");
        let id = st.files[0].id;
        st.toggle_share(id);
        assert!(!st.files[0].shared);
        st.remove(id);
        assert!(st.files.is_empty() && st.req.facts.is_empty());
    }

    #[test]
    fn the_file_limit_holds() {
        let mut st = AttachState::default();
        for i in 0..attach::max_files() {
            txt(&format!("{i}.txt"), "x", &mut st);
        }
        let id = st.next_id();
        assert!(st.add(attach::from_bytes("over.txt", b"x".to_vec(), id).unwrap()).is_err());
    }

    #[test]
    fn a_drop_over_the_chat_attaches_and_elsewhere_opens() {
        let mut p = PanelUi::default();
        p.open = true;
        p.attach.rect = Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(300.0, 600.0)));
        let f = vec![PathBuf::from("a.pdf")];
        assert!(!take_drop(&mut p, Some(egui::pos2(500.0, 10.0)), &f));
        assert!(p.attach.dropped.is_empty());
        assert!(take_drop(&mut p, Some(egui::pos2(10.0, 10.0)), &f));
        assert_eq!(p.attach.dropped, f);
        p.open = false;
        assert!(!take_drop(&mut p, Some(egui::pos2(10.0, 10.0)), &f));
    }

    #[test]
    fn preview_shows_what_was_read() {
        let mut st = AttachState::default();
        txt("prog.csv", "Room;Area\nHall;20\n", &mut st);
        assert_eq!(preview_text(&st.files[0]), "Room | Area\nHall | 20");
        assert!(pasted_paths("not a path").is_empty());
    }

    /// The chips and the card sit in a 320 px panel without running past its edge.
    #[test]
    fn chips_and_card_fit_a_narrow_panel() {
        let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let ctx = ddc_ui::theme::headless_ctx();
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            let ed = crate::editor::Editor::new_in_memory().unwrap();
            let mut p = PanelUi::default();
            for (n, b) in [("brief.txt", crate::app2_fx::fx("attach_brief3")), ("programme.csv", "Room;Area\nHall;20\n".to_string())] {
                let id = p.attach.next_id();
                p.attach.add(attach::from_bytes(n, b.as_bytes().to_vec(), id).unwrap()).unwrap();
            }
            let mut inner = egui::Rect::NOTHING;
            for _ in 0..2 {
                let _ = ctx.run(egui::RawInput::default(), |ctx| {
                    th.apply(ctx);
                    egui::SidePanel::left("t").exact_width(320.0).resizable(false).show(ctx, |ui| {
                        let cx = Cx { th: &th, cat: &cat, ed: &ed, view3d: false, palette_open: false };
                        let mut acts = vec![];
                        chips(ui, &mut p, &cx, &mut acts);
                        req_card(ui, &mut p, &cx, &mut acts);
                        inner = ui.min_rect();
                    });
                });
            }
            assert!(inner.width() <= 320.0 + 0.5, "{lang:?}: {}", inner.width());
            assert!(inner.height() > 0.0);
        }
    }
}
