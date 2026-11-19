// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Settings page «Project / Branding»: the company logo of the open project. A drop zone with a
//! live preview on a light sheet and on a dark background, «Choose file…», «Remove», and the
//! choice to start every new project with the same logo.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{row_k, Form, Out, K};
use crate::editor::OpFn;
use crate::shell::EditorApp;
use ddc_io::logo::Logo;
use ddc_model::Model;
use ddc_ui::theme::{space, Role};
use ddc_ui::widgets::{self, shell_kit};
use ddc_ui::{Catalog, Theme};
use egui::{self, Color32, Sense, Vec2};

pub(super) const LOGO: K = K("branding.title", "branding.desc");
pub(super) const DEFAULT: K = K("branding.default", "branding.default_desc");

/// Height of the drop zone.
const ZONE_H: f32 = 150.0;
/// The two backgrounds the logo is previewed on: a white sheet and a dark screen.
const PAPER: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
const NIGHT: Color32 = Color32::from_rgb(0x1E, 0x20, 0x24);
const ON_PAPER: Color32 = Color32::from_rgb(0x5B, 0x61, 0x6B);
const ON_NIGHT: Color32 = Color32::from_rgb(0xA8, 0xAE, 0xB8);
/// Room around the logo inside a swatch, and the strip under it for the caption.
const PAD: f32 = 14.0;
const CAPTION_H: f32 = 22.0;

#[derive(Default)]
pub(crate) struct LogoState {
    seen: Option<(u64, u64)>,
    pic: Option<Pic>,
    /// Catalog key of the last refusal, shown under the zone until the next try.
    pub(crate) error: Option<&'static str>,
    /// The default-for-new-projects file exists (read once per window opening and change).
    default_on: Option<bool>,
}

struct Pic {
    tex: egui::TextureHandle,
    w: u32,
    h: u32,
    kb: usize,
}

impl LogoState {
    /// The window was opened again: read everything afresh.
    pub(crate) fn reset(&mut self) {
        self.seen = None;
        self.default_on = None;
        self.error = None;
    }

    #[cfg(test)]
    pub(crate) fn has_picture(&self) -> bool {
        self.pic.is_some()
    }
}

fn refresh(app: &mut EditorApp, ctx: &egui::Context) {
    let key = app.ed.cache_key();
    let st = &mut app.chrome.settings.logo;
    if st.default_on.is_none() {
        st.default_on = Some(crate::project::app_dir().is_some_and(|d| crate::logo_ui::default_exists(&d)));
    }
    if st.seen == Some(key) {
        return;
    }
    st.seen = Some(key);
    st.pic = None;
    let Some(bytes) = app.ed.model.project_logo().ok().flatten() else {
        return;
    };
    let Ok(logo) = Logo::from_png(&bytes) else {
        return;
    };
    let img = egui::ColorImage::from_rgba_unmultiplied(
        [logo.width as usize, logo.height as usize],
        &logo.rgba,
    );
    st.pic = Some(Pic {
        tex: ctx.load_texture("logo1.preview", img, egui::TextureOptions::LINEAR),
        w: logo.width,
        h: logo.height,
        kb: bytes.len().div_ceil(1024),
    });
}

pub(super) fn page(app: &mut EditorApp, ui: &mut egui::Ui, f: &mut Form, out: &mut Vec<Out>) {
    refresh(app, ui.ctx());
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    let st = &app.chrome.settings.logo;
    let has = st.pic.is_some();
    if !f.searching() || f.hit(&c.t(LOGO.0), &c.t(LOGO.1)) {
        f.title(ui, th);
        f.rows += 1;
        card(ui, th, c, st, out);
        ui.add_space(space::MD);
    }
    let on = st.default_on.unwrap_or(false);
    row_k(ui, th, c, f, DEFAULT, |ui| {
        let mut v = on;
        let name = c.t("branding.default_on");
        ui.add_enabled_ui(has || on, |ui| {
            if widgets::toggle_bare(ui, th, &mut v, &name).changed() {
                out.push(Out::LogoDefault(v));
            }
        });
    });
}

/// Zone, caption line, hint and buttons.
fn card(ui: &mut egui::Ui, th: &Theme, c: &Catalog, st: &LogoState, out: &mut Vec<Out>) {
    let w = ui.available_width();
    let (zone, resp) = ui.allocate_exact_size(Vec2::new(w, ZONE_H), Sense::click());
    let name = c.t("branding.zone");
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
    let drag = ui.ctx().input(|i| !i.raw.hovered_files.is_empty())
        && ui.ctx().pointer_hover_pos().is_some_and(|p| zone.contains(p));
    let p = ui.painter();
    let edge = if drag || resp.hovered() {
        th.c("accent")
    } else {
        th.c("border.subtle")
    };
    p.rect_filled(zone, 8.0, th.c("bg.sunken"));
    let stroke = egui::Stroke::new(1.5, edge);
    let r = zone.shrink(1.0);
    let ring = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    p.extend(egui::Shape::dashed_line(&ring, stroke, 6.0, 4.0));
    match &st.pic {
        None => empty_zone(ui, th, c, zone),
        Some(pic) => {
            let half = (zone.width() - space::MD) / 2.0;
            let left = egui::Rect::from_min_size(zone.min, Vec2::new(half, zone.height()));
            let right = egui::Rect::from_min_size(
                zone.min + Vec2::new(half + space::MD, 0.0),
                Vec2::new(half, zone.height()),
            );
            swatch(ui, th, left, pic, PAPER, ON_PAPER, &c.t("branding.on_light"));
            swatch(ui, th, right, pic, NIGHT, ON_NIGHT, &c.t("branding.on_dark"));
        }
    }
    if resp.clicked() {
        out.push(Out::LogoPick);
    }
    ui.add_space(space::SM);
    if let Some(pic) = &st.pic {
        let info = c.tf(
            "branding.info",
            &[
                ("w", &pic.w.to_string()),
                ("h", &pic.h.to_string()),
                ("kb", &pic.kb.to_string()),
            ],
        );
        caption(ui, th, &info, th.c("text.secondary"));
    }
    if let Some(key) = st.error {
        caption(ui, th, &c.t(key), th.c("error"));
    }
    caption(ui, th, &c.t("branding.hint"), th.c("text.secondary"));
    ui.add_space(space::SM);
    ui.horizontal(|ui| {
        if widgets::Button::primary(&c.t("branding.choose")).show(ui, th).clicked() {
            out.push(Out::LogoPick);
        }
        ui.add_enabled_ui(st.pic.is_some(), |ui| {
            if widgets::Button::secondary(&c.t("branding.remove")).show(ui, th).clicked() {
                out.push(Out::LogoRemove);
            }
        });
    });
}

fn caption(ui: &mut egui::Ui, th: &Theme, text: &str, col: Color32) {
    ui.add(egui::Label::new(egui::RichText::new(text).font(th.font(Role::Caption)).color(col)).wrap());
}

fn empty_zone(ui: &egui::Ui, th: &Theme, c: &Catalog, zone: egui::Rect) {
    let max_w = zone.width() - 2.0 * space::LG;
    let head = shell_kit::elided(ui, &c.t("branding.drop"), th.font(Role::BodyStrong), th.c("text.primary"), max_w);
    let sub = shell_kit::elided(ui, &c.t("branding.formats"), th.font(Role::Caption), th.c("text.secondary"), max_w);
    let gap = space::XS;
    let total = head.size().y + gap + sub.size().y;
    let top = zone.center().y - total / 2.0;
    let p = ui.painter();
    let hx = zone.center().x - head.size().x / 2.0;
    p.galley(egui::pos2(hx, top), head.clone(), th.c("text.primary"));
    let sx = zone.center().x - sub.size().x / 2.0;
    p.galley(egui::pos2(sx, top + head.size().y + gap), sub, th.c("text.secondary"));
}

/// One preview background with the logo fitted inside and a caption under it.
fn swatch(ui: &egui::Ui, th: &Theme, rect: egui::Rect, pic: &Pic, bg: Color32, ink: Color32, label: &str) {
    let p = ui.painter();
    let rect = rect.shrink(4.0);
    p.rect_filled(rect, 6.0, bg);
    let area = egui::Rect::from_min_max(
        rect.min + Vec2::splat(PAD),
        egui::pos2(rect.max.x - PAD, rect.max.y - PAD - CAPTION_H),
    );
    if area.width() > 1.0 && area.height() > 1.0 {
        let k = (area.width() / pic.w as f32).min(area.height() / pic.h as f32);
        let size = Vec2::new(pic.w as f32 * k, pic.h as f32 * k);
        let at = egui::Rect::from_center_size(area.center(), size);
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        p.image(pic.tex.id(), at, uv, Color32::WHITE);
    }
    let g = shell_kit::elided(ui, label, th.font(Role::Caption), ink, rect.width() - 2.0 * PAD);
    let pos = egui::pos2(rect.min.x + PAD, rect.max.y - PAD / 2.0 - g.size().y - 2.0);
    p.galley(pos, g, ink);
}

/// File dialog, then `use_file`.
pub(super) fn pick(app: &mut EditorApp) {
    let c = &app.sh.cat;
    let dlg = crate::file_pick::Dlg::new()
        .set_title(c.t("branding.pick_title"))
        .add_filter(c.t("branding.filter"), &["png", "jpg", "jpeg"]);
    if let Some(path) = dlg.pick_file() {
        use_file(app, &path);
    }
}

/// A picture file for the open project; a refusal is shown on the page.
pub(crate) fn use_file(app: &mut EditorApp, path: &Path) {
    match crate::logo_ui::read_file(path) {
        Ok(logo) => set(app, Some(logo)),
        Err(e) => app.chrome.settings.logo.error = Some(e.key()),
    }
}

/// Sets or removes (`None`) the logo as one undo step; the default for new projects follows
/// when it is on.
pub(super) fn set(app: &mut EditorApp, logo: Option<Logo>) {
    app.chrome.settings.logo.error = None;
    let png = logo.as_ref().map(Logo::to_png);
    let bytes = png.clone();
    let f = OpFn(Arc::new(move |m: &mut Model| {
        m.set_project_logo(bytes.as_deref()).map(|_| ())
    }));
    if let Err(e) = app.ed.run_op(f) {
        let msg = app.sh.cat.tf("branding.failed", &[("error", &e.to_string())]);
        app.sh.toasts.push(ddc_ui::notify::Level::Warning, msg, None);
        return;
    }
    if let Some(dir) = crate::project::app_dir() {
        if app.chrome.settings.logo.default_on == Some(true) {
            match &png {
                Some(b) => {
                    let _ = crate::logo_ui::save_default(&dir, b);
                }
                None => {
                    crate::logo_ui::clear_default(&dir);
                    app.chrome.settings.logo.default_on = Some(false);
                }
            }
        }
    }
    let key = if png.is_some() { "branding.added" } else { "branding.removed" };
    let msg = app.sh.cat.t(key);
    app.sh.toasts.push(ddc_ui::notify::Level::Info, msg, None);
}

/// «Use for new projects»: the open project's logo becomes the default, or the default goes.
pub(super) fn set_default(app: &mut EditorApp, on: bool) {
    let Some(dir) = crate::project::app_dir() else {
        return;
    };
    if on {
        if let Some(b) = app.ed.model.project_logo().ok().flatten() {
            let _ = crate::logo_ui::save_default(&dir, &b);
        }
    } else {
        crate::logo_ui::clear_default(&dir);
    }
    app.chrome.settings.logo.default_on = Some(crate::logo_ui::default_exists(&dir));
}

/// A file dropped on the window while this page is open.
pub(crate) fn dropped(app: &mut EditorApp, path: PathBuf) {
    use_file(app, &path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::Lang;

    fn app() -> EditorApp {
        let mut a = EditorApp::new(crate::editor::Editor::new_in_memory().unwrap(), None, "t".into());
        // the default-logo file of the data folder is never touched by these tests
        a.chrome.settings.logo.default_on = Some(false);
        // the app starts in the language of the machine; the search words below are English
        a.sh.cat.lang = Lang::En;
        a
    }

    fn logo(w: u32, h: u32) -> Logo {
        Logo::from_rgba(w, h, (0..w * h).flat_map(|_| [10u8, 90, 170, 255]).collect()).unwrap()
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-brand-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn frame(a: &mut EditorApp, query: &str) -> usize {
        let mut rows = 0;
        ddc_ui::theme::headless_ctx().run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut f = Form::new(query);
                f.enter("page".to_string());
                let mut out = vec![];
                page(a, ui, &mut f, &mut out);
                rows = f.rows;
            });
        });
        rows
    }

    #[test]
    fn every_key_exists_in_both_languages() {
        let mut keys = vec!["settings.page.branding", "branding.zone", "branding.drop", "branding.formats"];
        keys.extend([LOGO.0, LOGO.1, DEFAULT.0, DEFAULT.1, "branding.default_on", "branding.on_light"]);
        keys.extend(["branding.on_dark", "branding.info", "branding.hint", "branding.choose", "branding.remove"]);
        keys.extend(["branding.pick_title", "branding.filter", "branding.added", "branding.removed"]);
        keys.extend(["branding.failed", "branding.info.none", "branding.info.have", "branding.info.open"]);
        use ddc_io::logo::LogoError::*;
        let errors = [TooLarge, Unsupported, Corrupt, Empty, Aspect].map(|e| e.key());
        keys.extend(errors);
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            let table = cat.table(lang);
            for k in &keys {
                let ok = matches!(table.get(*k), Some(serde_json::Value::String(s)) if !s.trim().is_empty());
                assert!(ok, "{k} missing in {lang:?}");
            }
        }
        // the page is named in the language of the user's request: "Project / Branding" in Russian
        assert_eq!(Catalog::new(Lang::Ru).t("settings.page.branding"), crate::app2_fx::fx("branding_page"));
    }

    #[test]
    fn search_finds_the_logo_rows_by_name_and_description() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            for k in [LOGO, DEFAULT] {
                let (label, desc) = (c.t(k.0), c.t(k.1));
                assert!(Form::new(&label).hit(&label, &desc));
                assert!(!Form::new("zzqqxx").hit(&label, &desc));
            }
        }
        // the app starts in Russian; the English query needs the English catalog
        let mut a = app();
        a.sh.cat = Catalog::new(Lang::En);
        assert_eq!(frame(&mut a, "logo"), 2, "both rows match a search for logo");
        a.sh.cat = Catalog::new(Lang::Ru);
        let word = a.sh.cat.t(LOGO.0).split_whitespace().next().unwrap().to_lowercase();
        assert_eq!(frame(&mut a, &word), 2, "both rows match a search for {word}");
        assert_eq!(frame(&mut a, "zzqqxx"), 0);
    }

    #[test]
    fn setting_and_removing_are_undo_steps_and_the_preview_follows() {
        let mut a = app();
        frame(&mut a, "");
        assert!(!a.chrome.settings.logo.has_picture());
        set(&mut a, Some(logo(60, 20)));
        assert!(a.ed.model.project_logo().unwrap().is_some());
        frame(&mut a, "");
        assert!(a.chrome.settings.logo.has_picture());
        set(&mut a, None);
        assert!(a.ed.model.project_logo().unwrap().is_none());
        frame(&mut a, "");
        assert!(!a.chrome.settings.logo.has_picture());
        a.ed.undo();
        assert!(a.ed.model.project_logo().unwrap().is_some(), "undo brings the logo back");
        frame(&mut a, "");
        assert!(a.chrome.settings.logo.has_picture());
    }

    #[test]
    fn a_bad_file_is_refused_with_a_message_and_changes_nothing() {
        let d = scratch("bad");
        let mut a = app();
        let junk = d.join("junk.png");
        std::fs::write(&junk, b"definitely not a picture").unwrap();
        use_file(&mut a, &junk);
        assert_eq!(a.chrome.settings.logo.error, Some("branding.err.unsupported"));
        assert!(a.ed.model.project_logo().unwrap().is_none());
        let wide = d.join("wide.png");
        let thin = Logo {
            width: 300,
            height: 10,
            rgba: vec![255; 300 * 10 * 4],
        };
        std::fs::write(&wide, thin.to_png()).unwrap();
        use_file(&mut a, &wide);
        assert_eq!(a.chrome.settings.logo.error, Some("branding.err.aspect"));
        let good = d.join("good.png");
        std::fs::write(&good, logo(120, 60).to_png()).unwrap();
        dropped(&mut a, good);
        assert_eq!(a.chrome.settings.logo.error, None, "a good file clears the message");
        assert!(a.ed.model.project_logo().unwrap().is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn page_code_uses_only_kit_widgets_and_tokens() {
        let src = include_str!("branding.rs");
        let body = src.split("mod tests {").next().unwrap_or("");
        for raw in [".checkbox(", "ComboBox", "egui::Button", "ui.button(", "selectable_value"] {
            assert!(!body.contains(raw), "raw widget {raw}");
        }
        for line in body.lines() {
            if let Some(i) = line.find("add_space(") {
                assert!(line[i + "add_space(".len()..].starts_with("space::"), "literal spacing: {line}");
            }
        }
    }
}
