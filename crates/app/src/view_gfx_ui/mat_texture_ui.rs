// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Texture section of the material card: built-in surfaces as swatches, tile size and rotation,
//! an image file (kept as a path or stored in the project), and a preview at real scale.
//! Every change is one undo step through `run`.

use std::cell::RefCell;
use std::collections::HashMap;

use ddc_model::{
    MaterialTexture, TextureSource, BUILTIN_TEXTURES, TEXTURE_EMBED_MAX, TEXTURE_SCALE_MAX,
    TEXTURE_SCALE_MIN,
};
use ddc_render::texture::{self, TexLayer};

use super::{run, t};
use crate::editor::Editor;
use crate::range_ui::{number_field, Loc};
use crate::ui_kit as kit;
use crate::units::{active_units, Units};
use ddc_ui::theme::space;
use ddc_ui::widgets::{checkbox, Button, TextField};

/// Pixels of a swatch image (the layout size follows the UI scale).
const THUMB_PX: u32 = 48;
/// Pixels of the preview image and the tiles it shows per side.
const PREVIEW_PX: u32 = 96;
const PREVIEW_TILES: f32 = 2.0;

#[derive(Default)]
struct St {
    rev: Option<(u64, u64)>,
    name: String,
    slot: Option<MaterialTexture>,
    scale: f64,
    rot: f64,
    path: String,
    embed: bool,
    error: Option<String>,
    thumbs: HashMap<String, egui::TextureHandle>,
    preview: Option<egui::TextureHandle>,
}

thread_local! {
    static ST: RefCell<St> = RefCell::new(St::default());
}

enum Act {
    /// Tile size and rotation of the current slot only.
    Numbers(f64, f64),
    Set(MaterialTexture, Option<Vec<u8>>),
    Clear,
}

fn load(st: &mut St, ed: &Editor, material: &str) {
    st.rev = Some(ed.cache_key());
    st.name = material.to_string();
    let row = ed
        .model
        .material_textures()
        .unwrap_or_default()
        .into_iter()
        .find(|r| r.material == material);
    st.slot = row.as_ref().map(|r| r.tex.clone());
    st.scale = st.slot.as_ref().map_or(1.0, |s| s.scale_m);
    st.rot = st.slot.as_ref().map_or(0.0, |s| s.rotation_deg);
    st.embed = matches!(st.slot.as_ref().map(|s| &s.source), Some(TextureSource::Embedded));
    if let Some(TextureSource::File(p)) = st.slot.as_ref().map(|s| &s.source) {
        st.path = p.clone();
    }
    st.preview = None;
    st.error = None;
    // the preview image of the current slot
    if let (Some(tex), Some(row)) = (&st.slot, row) {
        let layer = match &tex.source {
            TextureSource::Builtin(k) => TexLayer::builtin(k, Some(tex.scale_m as f32), 0.0),
            TextureSource::File(p) => image_layer(&std::fs::read(p).unwrap_or_default()),
            TextureSource::Embedded => image_layer(row.image.as_deref().unwrap_or_default()),
        };
        PREVIEW_LAYER.with(|p| *p.borrow_mut() = layer);
    } else {
        PREVIEW_LAYER.with(|p| *p.borrow_mut() = None);
    }
}

thread_local! {
    static PREVIEW_LAYER: RefCell<Option<TexLayer>> = const { RefCell::new(None) };
}

fn image_layer(bytes: &[u8]) -> Option<TexLayer> {
    let (w, h, rgba) = crate::mat_tex::decode_image(bytes)?;
    TexLayer::from_image(w, h, &rgba, 1.0, 0.0, 0.7)
}

fn thumb(
    ctx: &egui::Context,
    thumbs: &mut HashMap<String, egui::TextureHandle>,
    key: &str,
) -> Option<egui::TextureId> {
    if !thumbs.contains_key(key) {
        let layer = TexLayer::builtin(key, None, 0.0)?;
        let rgba = texture::swatch(&layer, THUMB_PX, 1.0);
        let img = egui::ColorImage::from_rgba_unmultiplied(
            [THUMB_PX as usize, THUMB_PX as usize],
            &rgba,
        );
        thumbs.insert(
            key.to_string(),
            ctx.load_texture(format!("t1.tex.{key}"), img, egui::TextureOptions::LINEAR),
        );
    }
    thumbs.get(key).map(|h| h.id())
}

fn preview(ctx: &egui::Context, st: &mut St) -> Option<egui::TextureId> {
    if st.preview.is_none() {
        let rgba = PREVIEW_LAYER.with(|p| {
            p.borrow()
                .as_ref()
                .map(|l| texture::swatch(l, PREVIEW_PX, PREVIEW_TILES))
        })?;
        let img = egui::ColorImage::from_rgba_unmultiplied(
            [PREVIEW_PX as usize, PREVIEW_PX as usize],
            &rgba,
        );
        st.preview = Some(ctx.load_texture("mat_texture.tex.preview", img, egui::TextureOptions::LINEAR));
    }
    st.preview.as_ref().map(|h| h.id())
}

/// Chosen image file as a slot: the path as it is, or the PNG bytes stored in the project.
fn file_act(path: &str, embed: bool, scale: f64, rot: f64) -> Result<Act, String> {
    let bytes = std::fs::read(path.trim()).map_err(|_| t("mat_texture.tex.bad_image"))?;
    if crate::mat_tex::decode_image(&bytes).is_none() {
        return Err(t("mat_texture.tex.bad_image"));
    }
    let (source, img) = if embed {
        if bytes.len() > TEXTURE_EMBED_MAX {
            return Err(t("mat_texture.tex.too_big"));
        }
        (TextureSource::Embedded, Some(bytes))
    } else {
        (TextureSource::File(path.trim().to_string()), None)
    };
    Ok(Act::Set(
        MaterialTexture {
            source,
            scale_m: scale,
            rotation_deg: rot,
        },
        img,
    ))
}

/// One model operation, so one undo step.
fn apply_act(ed: &mut Editor, material: &str, act: Act) -> bool {
    let name = material.to_string();
    match act {
        Act::Clear => run(ed, move |m| m.set_material_texture(&name, None)),
        Act::Numbers(s, r) => run(ed, move |m| m.set_material_texture_numbers(&name, s, r)),
        Act::Set(tex, img) => run(ed, move |m| {
            m.set_material_texture(&name, Some((tex.clone(), img.clone())))
        }),
    }
}

/// Tile size (a length in project units, metres in the model) and rotation in degrees, as
/// kit fields in the shared label column. True once per commit; typing and bad text commit nothing.
fn numbers(ui: &mut egui::Ui, salt: &str, scale: &mut f64, rot: &mut f64, u: Units, loc: Loc) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    let (sl, rl) = (crate::tr_lang(loc.ru, "mat_texture.tex.scale", &[]), crate::tr_lang(loc.ru, "mat_texture.tex.rotation", &[]));
    let mut done = false;
    kit::labeled_row_id(ui, &sl, |ui, lid| {
        let id = egui::Id::new(("ux56e.tex.scale", salt));
        let w = ddc_ui::theme::layout::PROP_VALUE;
        done |= kit::fixed_cell(ui, w, |ui| {
            kit::len_field(ui, &th, id, scale, u, TEXTURE_SCALE_MIN, TEXTURE_SCALE_MAX, loc.ru, lid, &sl)
        });
        // feet and inches carry their marks in the text; metric shows mm after it
        if matches!(u, Units::Metric) {
            ui.weak(crate::tr_str(loc.ru, "unit_fmt.unit.mm"));
        }
    });
    ui.add_space(space::XS);
    kit::labeled_row_id(ui, &rl, |ui, lid| {
        let key = ("ux56e.tex.rot", salt);
        done |= number_field(ui, key, rot, 1, 1.0, (0.0, 360.0), "\u{b0}", loc, (lid, &rl));
    });
    done
}

pub(super) fn section(ui: &mut egui::Ui, ed: &mut Editor, material: &str) {
    let mut st = ST.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if st.rev != Some(ed.cache_key()) || st.name != material {
        load(&mut st, ed, material);
    }
    let act = body(ui, &mut st);
    if let Some(act) = act {
        if apply_act(ed, material, act) {
            load(&mut st, ed, material);
        } else {
            st.error = ed.status.clone();
        }
    }
    ST.with(|s| *s.borrow_mut() = st);
}

fn body(ui: &mut egui::Ui, st: &mut St) -> Option<Act> {
    let mut act = None;
    ui.separator();
    ui.label(egui::RichText::new(t("mat_texture.tex.title")).strong());
    let side = ui.spacing().interact_size.y * 1.7;
    let current = st.slot.as_ref().map(|s| s.source.clone());
    ui.horizontal_wrapped(|ui| {
        let none = current.is_none();
        // already plain colour: the button is the current state, so it is off
        let r = ui
            .add_enabled_ui(!none, |ui| kit::secondary_button(ui, t("mat_texture.tex.none")))
            .inner
            .on_hover_text(t("mat_texture.tex.none_tip"))
            .on_disabled_hover_text(t("mat_texture.tex.none_tip"));
        if r.clicked() && !none {
            act = Some(Act::Clear);
        }
        for key in BUILTIN_TEXTURES {
            let Some(id) = thumb(ui.ctx(), &mut st.thumbs, key) else {
                continue;
            };
            let on = matches!(&current, Some(TextureSource::Builtin(k)) if k == key);
            let btn = egui::ImageButton::new(egui::Image::new((id, egui::vec2(side, side))))
                .selected(on);
            if ui
                .add(btn)
                .on_hover_text(t(&format!("mat_texture.tex.{key}")))
                .clicked()
                && !on
            {
                let (tile, _) = texture::builtin_info(key).unwrap_or((1.0, 0.5));
                act = Some(Act::Set(MaterialTexture::builtin(key, f64::from(tile)), None));
            }
        }
    });
    if st.slot.is_some() {
        let mut numbers_done = false;
        let (u, loc) = (active_units(), Loc::of(crate::ui_is_ru()));
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                let salt = st.name.clone();
                numbers_done = numbers(ui, &salt, &mut st.scale, &mut st.rot, u, loc);
            });
            if let Some(id) = preview(ui.ctx(), st) {
                ui.add_space(ui.spacing().item_spacing.x);
                ui.vertical(|ui| {
                    ui.image((id, egui::vec2(side * 2.0, side * 2.0)));
                    ui.weak(t("mat_texture.tex.preview"));
                });
            }
        });
        let changed = st.slot.as_ref().is_some_and(|s| {
            (s.scale_m - st.scale).abs() > 1e-9 || (s.rotation_deg - st.rot).abs() > 1e-9
        });
        if numbers_done && changed && act.is_none() {
            act = Some(Act::Numbers(st.scale, st.rot));
        }
    }
    let th = ddc_ui::theme::of(ui.ctx());
    ui.add_space(space::SM);
    let file_l = t("mat_texture.tex.image_file");
    let lid = ui.label(&file_l).id;
    ui.horizontal(|ui| {
        // Browse takes its own width at the right edge, the path field the rest
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let browse = t("mat_texture.tex.browse");
            if Button::quiet(&browse).show(ui, &th).clicked() {
                if let Some(p) = crate::file_pick::Dlg::new()
                    .add_filter("PNG, JPEG", &["png", "jpg", "jpeg"])
                    .pick_file()
                {
                    st.path = p.display().to_string();
                }
            }
            let hint = t("mat_texture.tex.path_hint");
            TextField::new(&mut st.path)
                .id_salt("ux56e.tex.path")
                .labelled_by(lid, &file_l)
                .hint(&hint)
                .show(ui, &th);
        });
    });
    ui.add_space(space::XS);
    ui.horizontal(|ui| {
        checkbox(ui, &th, &mut st.embed, &t("mat_texture.tex.embed")).on_hover_text(t("mat_texture.tex.embed_tip"));
        let can = !st.path.trim().is_empty();
        let use_l = t("mat_texture.tex.use_image");
        if ui
            .add_enabled_ui(can, |ui| kit::secondary_button(ui, use_l))
            .inner
            .on_disabled_hover_text(t("mat_texture.tex.use_image_off"))
            .clicked()
        {
            match file_act(&st.path, st.embed, st.scale.max(TEXTURE_SCALE_MIN), st.rot) {
                Ok(a) => {
                    st.error = None;
                    act = Some(a);
                }
                Err(e) => st.error = Some(e),
            }
        }
    });
    ui.weak(t("texture_hint.tex.hint"));
    if let Some(e) = &st.error {
        kit::error_note(ui, e);
    }
    act
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Context with the named font weights (table headers, tabs, labels): a bare one panics.
    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }


    fn key_ev(key: egui::Key) -> Vec<egui::Event> {
        [true, false]
            .into_iter()
            .map(|pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .collect()
    }

    /// Erases the shown text, then types `s`.
    fn typed(s: &str) -> Vec<egui::Event> {
        let mut v = Vec::new();
        for _ in 0..24 {
            v.extend(key_ev(egui::Key::Backspace));
        }
        v.push(egui::Event::Text(s.to_string()));
        v
    }

    struct Fields {
        ctx: egui::Context,
        scale: f64,
        rot: f64,
        u: Units,
        loc: Loc,
        done: bool,
    }

    impl Fields {
        fn new(u: Units, ru: bool) -> Self {
            Self { ctx: font_ctx(), scale: 1.0, rot: 10.0, u, loc: Loc::of(ru), done: false }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> usize {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0))),
                events,
                ..Default::default()
            };
            let (u, loc) = (self.u, self.loc);
            let (mut s, mut r, mut done) = (self.scale, self.rot, false);
            let full = self.ctx.run(input, |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    done = numbers(ui, "t.ux56e", &mut s, &mut r, u, loc);
                });
            });
            (self.scale, self.rot, self.done) = (s, r, done);
            full.shapes.len()
        }

        /// Tab to the `n`-th field (1 = tile size, 2 = rotation), then type `text`.
        fn type_in(&mut self, n: usize, text: &str) {
            self.frame(vec![]);
            for _ in 0..n {
                self.frame(key_ev(egui::Key::Tab));
                self.frame(vec![]);
            }
            self.frame(typed(text));
        }
    }

    #[test]
    fn tile_size_commits_in_feet_and_inches() {
        let mut f = Fields::new(Units::Imperial, false);
        f.type_in(1, "2'");
        assert_eq!(f.scale, 1.0, "typing alone commits nothing");
        f.frame(key_ev(egui::Key::Enter));
        f.frame(vec![]);
        assert!((f.scale - 2.0 * crate::units::FOOT).abs() < 1e-9, "{}", f.scale);
    }

    #[test]
    fn ru_rotation_commits_with_a_comma() {
        let mut f = Fields::new(Units::Metric, true);
        f.type_in(2, "45,5");
        f.frame(key_ev(egui::Key::Enter));
        assert!(f.done && (f.rot - 45.5).abs() < 1e-9, "{}", f.rot);
        assert_eq!(f.scale, 1.0);
    }

    #[test]
    fn esc_reverts_and_refused_text_keeps_the_value() {
        let mut f = Fields::new(Units::Metric, false);
        f.type_in(1, "500");
        f.frame(key_ev(egui::Key::Escape));
        f.frame(vec![]);
        assert_eq!(f.scale, 1.0, "Esc restores");
        let mut g = Fields::new(Units::Metric, false);
        g.type_in(2, "abc");
        g.frame(key_ev(egui::Key::Enter));
        g.frame(vec![]);
        assert!(!g.done && g.rot == 10.0 && g.scale == 1.0);
    }

    #[test]
    fn numbers_apply_as_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let tex = MaterialTexture::builtin("brick", 0.6);
        assert!(apply_act(&mut ed, "brick", Act::Set(tex, None)));
        assert!(apply_act(&mut ed, "brick", Act::Numbers(0.25, 45.0)));
        let now = ed.model.material_textures().unwrap();
        assert_eq!((now[0].tex.scale_m, now[0].tex.rotation_deg), (0.25, 45.0));
        ed.undo();
        let back = ed.model.material_textures().unwrap();
        assert_eq!(back[0].tex.scale_m, 0.6);
        // a refused value changes nothing
        assert!(!apply_act(&mut ed, "brick", Act::Numbers(0.0, 0.0)));
    }

    fn draw(ctx: &egui::Context, ed: &mut Editor) -> (f64, f64) {
        let _ = ctx.run(egui::RawInput::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| section(ui, ed, "brick"));
        });
        ST.with(|s| (s.borrow().scale, s.borrow().rot))
    }

    #[test]
    fn slot_reads_anew_when_an_older_revision_is_put_back() {
        let (ctx, mut ed) = (font_ctx(), Editor::new_in_memory().unwrap());
        let tex = MaterialTexture::builtin("brick", 0.6);
        assert!(apply_act(&mut ed, "brick", Act::Set(tex, None)));
        assert_eq!(draw(&ctx, &mut ed).0, 0.6);
        let rev = ed.revision;
        assert!(apply_act(&mut ed, "brick", Act::Numbers(0.25, 45.0)));
        ed.restore_revision(rev);
        assert_eq!(draw(&ctx, &mut ed), (0.25, 45.0), "keyed on cache_key");
        ST.with(|s| *s.borrow_mut() = St::default());
    }

    #[test]
    fn slot_cache_survives_an_unrelated_frame() {
        let (ctx, mut ed) = (font_ctx(), Editor::new_in_memory().unwrap());
        let tex = MaterialTexture::builtin("brick", 0.6);
        assert!(apply_act(&mut ed, "brick", Act::Set(tex, None)));
        let _ = draw(&ctx, &mut ed);
        ST.with(|s| s.borrow_mut().scale = 9.0);
        assert_eq!(draw(&ctx, &mut ed).0, 9.0, "no reload without a model change");
        ST.with(|s| *s.borrow_mut() = St::default());
    }

    #[test]
    fn fields_render_in_both_languages_and_unit_systems() {
        for (u, ru) in [(Units::Metric, false), (Units::Metric, true), (Units::Imperial, false), (Units::Imperial, true)] {
            let mut f = Fields::new(u, ru);
            let mut shapes = 0;
            for _ in 0..3 {
                shapes = f.frame(vec![]);
            }
            assert!(shapes > 0, "{ru}");
            assert_eq!((f.scale, f.rot), (1.0, 10.0));
        }
    }

    #[test]
    fn texture_section_uses_kit_widgets_only() {
        let src = include_str!("mat_texture_ui.rs");
        let body = &src[..src.find("#[cfg(test)]").unwrap()];
        for raw in ["DragValue", "egui::Grid", "ui.button(", "selectable_label", "selectable_value", "TextEdit", "ui.checkbox(", ".suffix("] {
            assert!(!body.contains(raw), "raw widget left: {raw}");
        }
        assert!(body.contains("len_field(") && body.contains("number_field(") && body.contains("TextField::new("));
    }

    #[test]
    fn locale_json_is_paired_and_utf8() {
        let en = include_str!("../../../ui/data/i18n/en/texture_hint.json");
        let ru = include_str!("../../../ui/data/i18n/ru/texture_hint.json");
        let (en, ru): (serde_json::Value, serde_json::Value) =
            (serde_json::from_str(en).unwrap(), serde_json::from_str(ru).unwrap());
        let keys = |v: &serde_json::Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(&en), keys(&ru));
        assert!(crate::tr_lang(true, "texture_hint.tex.hint", &[]).chars().any(|c| !c.is_ascii()));
        assert!(crate::tr_lang(false, "texture_hint.tex.hint", &[]).is_ascii());
    }
}
