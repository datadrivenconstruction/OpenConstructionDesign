// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Swatch of a material as it looks in the realistic 3D style: the class texture tinted to the
//! material colour at its real tile size, with a soft sheen for smooth and metallic classes. One
//! picture per (class, colour), shared by the material list, the properties panel and legends.

use std::cell::RefCell;
use std::collections::HashMap;

use ddc_model::material_look::{resolve, Resolved};
use ddc_render::texture::{self, TexLayer};

/// Pixels of one swatch image; the layout size follows the UI scale.
pub const SWATCH_PX: u32 = 48;

thread_local! {
    static CACHE: RefCell<HashMap<(&'static str, u32, u32), egui::TextureHandle>> =
        RefCell::new(HashMap::new());
}

/// RGBA8 picture of a resolved look, `px` square: texture at real scale plus sheen.
pub fn look_rgba(r: &Resolved, px: u32) -> Option<Vec<u8>> {
    let rgb = [(r.rgb >> 16) as u8, (r.rgb >> 8) as u8, r.rgb as u8];
    let layer = TexLayer::look(r.look.texture, rgb, r.look.tile_m, r.roughness, r.metallic)?;
    // a swatch shows about half a metre, so a 0.5 m brick shows its bond and a 2 m render stays calm
    let repeats = (0.6 / layer.scale_m.max(0.05)).clamp(0.5, 4.0);
    let mut img = texture::swatch(&layer, px, repeats);
    add_sheen(&mut img, px, r.roughness, r.metallic);
    Some(img)
}

/// Diagonal highlight in the upper left, stronger for smooth and metallic surfaces.
fn add_sheen(img: &mut [u8], px: u32, roughness: f32, metallic: f32) {
    let strength = ((1.0 - roughness).clamp(0.0, 1.0) * 0.55 + metallic.clamp(0.0, 1.0) * 0.2).min(0.6);
    if strength < 0.05 {
        return;
    }
    let n = px as f32;
    for y in 0..px {
        for x in 0..px {
            let d = (x as f32 + y as f32) / (2.0 * n);
            // band across the upper-left corner, fading out by the middle
            let k = (1.0 - (d / 0.45)).clamp(0.0, 1.0).powi(2) * strength;
            let i = ((y * px + x) * 4) as usize;
            for c in 0..3 {
                let v = img[i + c] as f32;
                img[i + c] = (v + (255.0 - v) * k).round() as u8;
            }
        }
    }
}

/// Texture of the material's look in the realistic style, None when the name and colour name no
/// class (the caller paints the plain colour).
pub fn look_swatch(ctx: &egui::Context, name: &str, color: Option<u32>) -> Option<egui::TextureId> {
    let r = resolve(name, color)?;
    let key = (r.look.key, r.rgb, (r.roughness * 100.0) as u32 * 2 + u32::from(r.metallic > 0.5));
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if !c.contains_key(&key) {
            let rgba = look_rgba(&r, SWATCH_PX)?;
            let img = egui::ColorImage::from_rgba_unmultiplied([SWATCH_PX as usize; 2], &rgba);
            let h = ctx.load_texture(
                format!("mat_swatch.{}.{:06x}", key.0, key.1),
                img,
                egui::TextureOptions::LINEAR,
            );
            c.insert(key, h);
        }
        c.get(&key).map(|h| h.id())
    })
}

/// A properties row: the material's swatch and its (localised) name.
pub fn material_row(ui: &mut egui::Ui, key: &str, name: &str) {
    crate::ui_kit::kv_widget(ui, key, |ui| {
        ui.horizontal(|ui| {
            let side = ui.spacing().interact_size.y - 4.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
            match look_swatch(ui.ctx(), name, None) {
                Some(id) => {
                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                    ui.painter().image(id, rect, uv, egui::Color32::WHITE);
                }
                None => {
                    ui.painter().rect_filled(rect, 0.0, ui.visuals().widgets.inactive.bg_fill);
                }
            }
            ui.painter()
                .rect_stroke(rect, 0.0, egui::Stroke::new(1.0, ui.visuals().weak_text_color()), egui::StrokeKind::Inside);
            ui.add(egui::Label::new(super::materials_win::label(name)).truncate());
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mean(img: &[u8]) -> [f32; 3] {
        let n = (img.len() / 4) as f32;
        let mut s = [0.0f32; 3];
        for p in img.chunks_exact(4) {
            for c in 0..3 {
                s[c] += p[c] as f32;
            }
        }
        s.map(|v| v / n)
    }

    #[test]
    fn swatch_of_a_class_is_not_flat_and_follows_the_colour() {
        let red = resolve("brick", Some(0xB03020)).unwrap();
        let img = look_rgba(&red, 32).unwrap();
        assert_eq!(img.len(), 32 * 32 * 4);
        let first = &img[..3];
        assert!(img.chunks_exact(4).any(|p| &p[..3] != first), "pattern shows");
        let m = mean(&img);
        assert!(m[0] > m[2] + 20.0, "reddish: {m:?}");
        let blue = resolve("brick", Some(0x2030B0)).unwrap();
        let mb = mean(&look_rgba(&blue, 32).unwrap());
        assert!(mb[2] > mb[0] + 20.0, "{mb:?}");
    }

    #[test]
    fn classes_have_different_pictures() {
        let a = look_rgba(&resolve("stone", None).unwrap(), 32).unwrap();
        let b = look_rgba(&resolve("concrete", None).unwrap(), 32).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn smooth_classes_get_a_sheen_and_rough_ones_do_not() {
        let mut flat = vec![100u8; 16 * 16 * 4];
        add_sheen(&mut flat, 16, 1.0, 0.0);
        assert!(flat.iter().all(|&b| b == 100));
        let mut shiny = vec![100u8; 16 * 16 * 4];
        add_sheen(&mut shiny, 16, 0.1, 1.0);
        assert!(shiny[0] > 150 && shiny[(15 * 16 + 15) * 4] == 100);
    }

    #[test]
    fn an_unknown_text_has_no_swatch_and_a_colour_makes_one() {
        assert!(resolve("zzz unknown", None).is_none());
        assert!(resolve("zzz unknown", Some(0x123456)).is_some());
    }
}
