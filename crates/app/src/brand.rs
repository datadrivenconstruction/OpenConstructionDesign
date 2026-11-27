// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! OpenConstruction brand: window icon, product mark and the family block. Marks come from the
//! logo kit in `data/brand` and are rasterized from the SVGs.

use ddc_ui::theme::{space, Mode};
use ddc_ui::typo::Role;
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText, TextureHandle, TextureOptions, Vec2};

pub const PRODUCT: &str = "OpenConstructionDesign";
// Config constants for the family links. No source in the repo or docs names these sites:
// both are confirmed before release (a wrong one only opens the wrong page).
pub const URL_ERP: &str = "https://openconstructionerp.com";
pub const URL_ACADEMY: &str = "https://openconstructionacademy.com";

const APPICON: &str = include_str!("../data/brand/design-appicon-512.svg");

#[derive(Clone, Copy)]
enum Mark {
    Erp,
    Design,
    Academy,
}

fn mark_svg(m: Mark, dark: bool) -> &'static str {
    match (m, dark) {
        (Mark::Erp, false) => include_str!("../data/brand/erp-mark-light.svg"),
        (Mark::Erp, true) => include_str!("../data/brand/erp-mark-dark.svg"),
        (Mark::Design, false) => include_str!("../data/brand/design-mark-light.svg"),
        (Mark::Design, true) => include_str!("../data/brand/design-mark-dark.svg"),
        (Mark::Academy, false) => include_str!("../data/brand/academy-mark-light.svg"),
        (Mark::Academy, true) => include_str!("../data/brand/academy-mark-dark.svg"),
    }
}

fn straight_rgba(img: &egui::ColorImage) -> Vec<u8> {
    img.pixels
        .iter()
        .flat_map(|p| {
            let a = p.a() as u32;
            let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 };
            [un(p.r()), un(p.g()), un(p.b()), p.a()]
        })
        .collect()
}

/// App icon for the window and task bar (256 px, straight alpha).
pub fn window_icon() -> egui::IconData {
    let px = 256;
    match ddc_ui::icons::rasterize_svg(APPICON, px) {
        Ok(img) => egui::IconData { rgba: straight_rgba(&img), width: px, height: px },
        Err(_) => egui::IconData::default(),
    }
}

fn texture(ctx: &egui::Context, m: Mark, dark: bool) -> Option<TextureHandle> {
    let id = egui::Id::new(("brand.mark", m as u8, dark));
    if let Some(t) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return Some(t);
    }
    let img = ddc_ui::icons::rasterize_svg(mark_svg(m, dark), 96).ok()?;
    let t = ctx.load_texture(format!("brand-mark-{}-{dark}", m as u8), img, TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    Some(t)
}

/// The Design mark at `side` logical px, for headers.
pub fn design_mark(ui: &mut egui::Ui, th: &Theme, side: f32) {
    let dark = th.mode == Mode::Dark;
    if let Some(t) = texture(ui.ctx(), Mark::Design, dark) {
        ui.add(egui::Image::new((t.id(), Vec2::splat(side))));
    }
}

/// «OpenConstruction family»: ERP, Design (this app), Academy; the other two open in the browser.
pub fn family_block(ui: &mut egui::Ui, th: &Theme, c: &Catalog) {
    let dark = th.mode == Mode::Dark;
    ui.label(
        RichText::new(c.t("brand.family.title"))
            .font(th.font(Role::Section))
            .color(th.c("text.secondary")),
    );
    ui.add_space(space::XS);
    let rows = [
        (Mark::Erp, "OpenConstructionERP", "brand.family.erp", Some(URL_ERP)),
        (Mark::Design, PRODUCT, "brand.family.design", None),
        (Mark::Academy, "OpenConstructionAcademy", "brand.family.academy", Some(URL_ACADEMY)),
    ];
    for (m, name, key, url) in rows {
        ui.horizontal(|ui| {
            if let Some(t) = texture(ui.ctx(), m, dark) {
                ui.add(egui::Image::new((t.id(), Vec2::splat(24.0))));
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = space::XXS;
                match url {
                    Some(u) => {
                        let r = ui.link(RichText::new(name).font(th.font(Role::BodyStrong)));
                        if r.on_hover_text(c.t("brand.family.open")).clicked()
                            && crate::update::open_url(u).is_err()
                        {
                            ui.ctx().copy_text(u.to_string());
                        }
                    }
                    None => {
                        ui.label(
                            RichText::new(name)
                                .font(th.font(Role::BodyStrong))
                                .color(th.c("accent")),
                        );
                    }
                }
                ui.label(
                    RichText::new(c.t(key))
                        .font(th.font(Role::Caption))
                        .color(th.c("text.secondary")),
                );
            });
        });
        ui.add_space(space::XS);
    }
}
