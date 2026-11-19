//! What the assistant found or changed in the view: the chat's result card ("4 of 202" with
//! Show, Isolate, Select) and the view panel over the viewport (the legend of a color by value,
//! what is shown or isolated, Reset view). Neither touches the model.

use super::kit::{self, space, Button};
use super::{Action, Cx};
use crate::ai::sets::Key;
use crate::ai::view_state::{self, Ask, Status};
use crate::ai::view_tools::{Bucket, Found, FoundBy, Legend, LegendRow, ViewCmd};
use crate::view_gfx_ui::color_of;
use ddc_model::ElKey;
use ddc_ui::i18n::Catalog;
use ddc_ui::theme::Role;
use ddc_ui::widgets::text::paint_in;
use ddc_ui::Theme;
use egui::{self, Align, RichText, Stroke, StrokeKind, Ui};

pub const VIEW_PANEL_ID: &str = "ddc.ai.view_legend";
/// The view panel's content width: a value, its swatch and a five-digit count.
const PANEL_W: f32 = 232.0;
const SWATCH: f32 = 12.0;
const COUNT_W: f32 = 48.0;
/// The legend scrolls past about a dozen rows; the viewport stays visible.
const ROWS_H: f32 = 264.0;

/// "4 of 202".
pub fn count_line(cat: &Catalog, f: &Found) -> String {
    let (n, m) = (f.keys.len().to_string(), f.of.max(f.keys.len()).to_string());
    cat.tf("ai_found.found.count", &[("n", &n), ("m", &m)])
}

fn by_key(by: FoundBy) -> &'static str {
    match by {
        FoundBy::Query => "ai_found.found.query",
        FoundBy::Show => "ai_found.found.show",
        FoundBy::Isolate => "ai_found.found.isolate",
        FoundBy::Hide => "ai_found.found.hide",
        FoundBy::Color => "ai_found.found.color",
    }
}

/// "Found: 4 of 202".
pub fn headline(cat: &Catalog, f: &Found) -> String {
    let what = cat.t(by_key(f.by));
    cat.tf("ai_found.found.line", &[("what", &what), ("count", &count_line(cat, f))])
}

fn lit(keys: &[Key]) -> Vec<ElKey> {
    keys.iter().map(Key::el).collect()
}

/// The result card. Hovering it lights the elements; the buttons change the view only.
pub fn card(
    ui: &mut Ui,
    cx: &Cx<'_>,
    f: &Found,
    i: usize,
    acts: &mut Vec<Action>,
    hot: &mut Option<Vec<ElKey>>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let n = f.keys.len();
    let drawn = ui.push_id(("ddc.ai.found", i), |ui| {
        kit::card(ui, th, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(headline(cat, f))
                        .font(th.font(Role::BodyStrong))
                        .color(th.c("text.primary")),
                )
                .wrap(),
            );
            if !f.what.is_empty() {
                kit::elided_label(ui, &f.what, th.font(Role::Caption), th.c("text.secondary"));
            }
            if n == 0 {
                kit::caption(ui, th, &cat.t("ai_found.found.none"));
                return;
            }
            ui.add_space(space::XS);
            let buttons = [
                (
                    "ai_found.found.show_btn",
                    "ai_found.found.show_tip",
                    ViewCmd::Show {
                        keys: f.keys.clone(),
                        level: f.level,
                    },
                ),
                (
                    "ai_found.found.isolate_btn",
                    "ai_found.found.isolate_tip",
                    ViewCmd::Isolate {
                        keys: f.keys.clone(),
                        hide: false,
                    },
                ),
                (
                    "ai_found.found.select_btn",
                    "ai_found.found.select_tip",
                    ViewCmd::Select(f.keys.clone()),
                ),
            ];
            // wrapped: the Russian labels are longer and must not clip in a narrow panel
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = space::SM;
                for (label, tip, cmd) in buttons {
                    let tip = cat.t(tip);
                    let b = Button::secondary(&cat.t(label)).small().tooltip(&tip, "").show(ui, th);
                    if b.clicked() {
                        acts.push(Action::FoundView(cmd));
                    }
                }
            });
        });
    });
    if n > 0 && ui.rect_contains_pointer(drawn.response.rect) {
        *hot = Some(lit(&f.keys));
    }
}

fn title(cat: &Catalog, lg: Option<&Legend>) -> String {
    match lg {
        Some(l) => cat.tf("ai_found.legend.title", &[("by", &l.by)]),
        None => cat.t("ai_found.view.title"),
    }
}

pub fn value_text(cat: &Catalog, lg: &Legend, r: &LegendRow) -> String {
    match r.bucket {
        Bucket::Value => r.value.clone(),
        Bucket::Empty => cat.t("ai_found.legend.empty_value"),
        Bucket::Other => cat.plural("ai_found.legend.other", lg.folded as u64),
    }
}

/// One legend row: swatch, value, count. The whole row is the click target.
fn row(ui: &mut Ui, th: &Theme, cat: &Catalog, text: &str, rgb: u32, n: usize) -> egui::Response {
    let font = th.font(Role::Label);
    let h = ui.fonts(|f| f.row_height(&font)) + space::XS * 2.0;
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text));
    if resp.hovered() {
        kit::row_highlight(ui, th, rect, false);
    }
    let inner = rect.shrink2(egui::vec2(space::XS, 0.0));
    let sw = egui::Rect::from_center_size(
        egui::pos2(inner.left() + SWATCH / 2.0, inner.center().y),
        egui::vec2(SWATCH, SWATCH),
    );
    let radius = th.radius("sm");
    ui.painter().rect_filled(sw, radius, color_of(rgb));
    let edge = Stroke::new(1.0, th.c("border.subtle"));
    ui.painter().rect_stroke(sw, radius, edge, StrokeKind::Inside);
    let count = egui::Rect::from_min_max(
        egui::pos2(inner.right() - COUNT_W, inner.top()),
        inner.right_bottom(),
    );
    let value = egui::Rect::from_min_max(
        egui::pos2(sw.right() + space::SM, inner.top()),
        egui::pos2(count.left() - space::SM, inner.bottom()),
    );
    let cut = paint_in(ui, value, text, font, th.c("text.primary"), Align::Min);
    let num = th.font(Role::Numeric);
    paint_in(ui, count, &n.to_string(), num, th.c("text.secondary"), Align::Max);
    let tip = cat.t("ai_found.legend.row_tip");
    resp.on_hover_text(if cut { format!("{text}\n{tip}") } else { tip })
}

fn body(
    ui: &mut Ui,
    th: &Theme,
    cat: &Catalog,
    lg: Option<&Legend>,
    s: Status,
    over: &mut Option<usize>,
) {
    kit::elided_label(ui, &title(cat, lg), th.font(Role::BodyStrong), th.c("text.primary"));
    let lines = [
        ("ai_found.view.shown", s.shown),
        ("ai_found.view.isolated", s.isolated),
        ("ai_found.view.hidden", s.hidden),
    ];
    for (key, n) in lines {
        if n > 0 {
            kit::caption(ui, th, &cat.plural(key, n as u64));
        }
    }
    if let Some(lg) = lg {
        ui.add_space(space::XS);
        egui::ScrollArea::vertical()
            .id_salt("ddc.ai.view_legend.rows")
            .max_height(ROWS_H)
            .show(ui, |ui| {
                for (i, r) in lg.rows.iter().enumerate() {
                    let text = value_text(cat, lg, r);
                    let resp = row(ui, th, cat, &text, r.rgb, r.keys.len());
                    if resp.hovered() {
                        *over = Some(i);
                    }
                    if resp.clicked() {
                        view_state::ask(Ask::Row(i));
                    }
                }
            });
    }
    ui.add_space(space::XS);
    let tip = cat.t("ai_found.view.reset_tip");
    if Button::secondary(&cat.t("ai_found.view.reset"))
        .small()
        .tooltip(&tip, "")
        .show(ui, th)
        .clicked()
    {
        view_state::ask(Ask::Reset);
    }
}

/// Over the viewport's lower right corner while the assistant's view is on; nothing otherwise.
pub fn view_panel(ctx: &egui::Context, th: &Theme, cat: &Catalog, area: egui::Rect) {
    let lg = view_state::legend();
    if lg.is_none() && !view_state::active() {
        view_state::hover(None);
        return;
    }
    let pad = space::SM;
    let mut over = None;
    egui::Area::new(egui::Id::new(VIEW_PANEL_ID))
        .order(egui::Order::Middle)
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(area.right_bottom() + egui::vec2(-pad, -pad))
        .show(ctx, |ui| {
            kit::card(ui, th, |ui| {
                ui.set_width(PANEL_W);
                body(ui, th, cat, lg.as_deref(), view_state::status(), &mut over);
            })
        });
    view_state::hover(over);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::view_tools;
    use ddc_ui::Lang;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn found(n: usize, of: usize, by: FoundBy) -> Found {
        let keys = (0..n as i64).map(|id| Key { cat: "window", id }).collect();
        Found {
            by,
            what: "window fire_rating=empty".into(),
            keys: Arc::new(keys),
            of,
            level: None,
        }
    }

    #[test]
    fn the_card_counts_n_of_m_in_both_languages() {
        let f = found(4, 202, FoundBy::Query);
        let en = Catalog::new(Lang::En);
        let ru = Catalog::new(Lang::Ru);
        assert_eq!(count_line(&en, &f), "4 of 202");
        assert_eq!(count_line(&ru, &f), "4 \u{438}\u{437} 202");
        assert!(headline(&en, &f).ends_with("4 of 202"));
        assert!(headline(&ru, &f).ends_with("4 \u{438}\u{437} 202"));
        for by in [FoundBy::Show, FoundBy::Isolate, FoundBy::Hide, FoundBy::Color] {
            assert_ne!(headline(&en, &found(1, 1, by)), headline(&en, &f), "{by:?}");
        }
        // the scope never reads less than what was found
        assert_eq!(count_line(&en, &found(3, 0, FoundBy::Show)), "3 of 3");
    }

    #[test]
    fn legend_rows_name_the_empty_and_the_folded_values() {
        let mut g: BTreeMap<String, Vec<Key>> = BTreeMap::new();
        for i in 0..40i64 {
            g.entry(format!("v{i}")).or_default().push(Key { cat: "wall", id: i });
        }
        g.insert(String::new(), vec![Key { cat: "wall", id: 99 }]);
        let lg = view_tools::legend("mark", g);
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            let texts: Vec<String> = lg.rows.iter().map(|r| value_text(&cat, &lg, r)).collect();
            assert!(texts.iter().all(|t| !t.is_empty() && !t.contains('#')), "{lang:?}");
            let other = lg.rows.iter().position(|r| r.bucket == Bucket::Other).unwrap();
            assert!(texts[other].contains(&lg.folded.to_string()), "{lang:?}");
            assert!(title(&cat, Some(&lg)).contains("mark"), "{lang:?}");
            assert!(!title(&cat, None).is_empty());
        }
    }
}
