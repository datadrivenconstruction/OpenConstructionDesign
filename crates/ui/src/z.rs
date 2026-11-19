// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The one stacking order of the window, bottom to top:
//!
//! 1. `Canvas`: the views and what floats on them (edit bar, hints, overlays, previews);
//! 2. `Dock`: panels docked at the window edges (the canvas and docks share egui's
//!    background layer and never overlap);
//! 3. `Float`: windows over the canvas: floating panels and non-modal dialogs, the last one
//!    clicked on top;
//! 4. `Modal`: modal dialogs, with a backdrop over everything under them and the keyboard
//!    trapped inside;
//! 5. `Menu`: menus, popups, dropdowns and tooltips, including those of the active dialog;
//! 6. `Toast`: notifications.
//!
//! egui keeps the areas of one order in click order, so a canvas overlay that was clicked
//! would rise above a dialog. [`begin`] shows an empty anchor at the bottom of the middle order;
//! windows say so with [`window`]; [`settle`] at the end of the frame ties every other middle
//! layer directly above the anchor, under all windows.
use egui::{Area, Context, Id, LayerId, Order};

/// Windows shown in this pass.
#[derive(Clone, Default)]
struct Windows {
    pass: u64,
    ids: Vec<Id>,
}

fn windows_id() -> Id {
    Id::new("ddc.z.windows")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Z {
    Canvas,
    Dock,
    Float,
    Modal,
    Menu,
    Toast,
}

impl Z {
    pub const fn order(self) -> Order {
        match self {
            Self::Dock => Order::Background,
            Self::Canvas | Self::Float => Order::Middle,
            Self::Modal | Self::Menu => Order::Foreground,
            Self::Toast => Order::Tooltip,
        }
    }
}

fn anchor() -> LayerId {
    LayerId::new(Order::Middle, Id::new("ddc.z.canvas"))
}

/// First thing in a frame: the anchor the canvas layers sit on. Shown before any window, it
/// is the lowest middle layer for good, as egui only ever raises layers.
pub fn begin(ctx: &Context) {
    Area::new(anchor().id)
        .order(Order::Middle)
        .interactable(false)
        .fixed_pos(egui::Pos2::ZERO)
        .show(ctx, |_| {});
}

/// Keeps `layer` on the canvas: above the views, under every window. Call each frame it shows.
/// A layer egui does not order (painted without an area) is entered into the order first.
pub fn canvas(ctx: &Context, layer: LayerId) {
    if layer.order != Order::Middle {
        return;
    }
    ctx.memory_mut(|m| {
        let known = m.layer_ids().any(|l| l == layer);
        let areas = m.areas_mut();
        if !known {
            areas.move_to_top(layer);
        }
        areas.set_sublayer(anchor(), layer);
    });
}

/// The middle-order area `id` shown this pass is a window: it stays above the canvas.
pub fn window(ctx: &Context, id: Id) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        let w = d.get_temp_mut_or_default::<Windows>(windows_id());
        if w.pass != pass {
            *w = Windows { pass, ids: vec![] };
        }
        w.ids.push(id);
    });
}

/// `id` was a window in the last pass that showed any (floating windows stack by focus).
pub fn is_window(ctx: &Context, id: Id) -> bool {
    ctx.data(|d| d.get_temp::<Windows>(windows_id()))
        .is_some_and(|w| w.ids.contains(&id))
}

/// The topmost window still visible from the last pass that showed any.
pub fn top_window(ctx: &Context) -> Option<Id> {
    let w = ctx.data(|d| d.get_temp::<Windows>(windows_id()))?;
    ctx.memory(|m| {
        let seen = m.areas().visible_layer_ids();
        m.layer_ids()
            .filter(|l| l.order == Order::Middle && w.ids.contains(&l.id) && seen.contains(l))
            .last()
            .map(|l| l.id)
    })
}

/// Last thing in a frame: every middle layer shown this pass that is not a window goes on the
/// canvas, under the windows.
pub fn settle(ctx: &Context) {
    let pass = ctx.cumulative_pass_nr();
    let wins = ctx
        .data(|d| d.get_temp::<Windows>(windows_id()))
        .filter(|w| w.pass == pass)
        .unwrap_or_default();
    let a = anchor();
    let on_canvas: Vec<LayerId> = ctx.memory(|m| {
        let seen = m.areas().visible_layer_ids();
        m.layer_ids()
            .filter(|l| l.order == Order::Middle && *l != a && seen.contains(l))
            .filter(|l| !wins.ids.contains(&l.id))
            .collect()
    });
    for l in on_canvas {
        canvas(ctx, l);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ctx: &Context, dialog_first: bool, click: Option<egui::Pos2>) -> egui::FullOutput {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let mut input = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        if let Some(p) = click {
            input.events.push(egui::Event::PointerMoved(p));
            for pressed in [true, false] {
                input.events.push(egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                });
            }
        }
        ctx.run(input, |ctx| {
            begin(ctx);
            let over = |ctx: &Context| {
                Area::new(Id::new("t.overlay"))
                    .order(Z::Canvas.order())
                    .fixed_pos(egui::pos2(100.0, 100.0))
                    .show(ctx, |ui| ui.add_sized([400.0, 60.0], egui::Button::new("o")))
            };
            let win = |ctx: &Context| {
                window(ctx, Id::new("t.win"));
                egui::Window::new("w")
                    .id(Id::new("t.win"))
                    .fixed_pos(egui::pos2(80.0, 80.0))
                    .show(ctx, |ui| ui.allocate_space(egui::vec2(200.0, 100.0)))
            };
            if dialog_first {
                win(ctx);
                over(ctx);
            } else {
                over(ctx);
                win(ctx);
            }
            settle(ctx);
        })
    }

    fn rank(ctx: &Context, id: &str) -> usize {
        let l = LayerId::new(Order::Middle, Id::new(id));
        ctx.memory(|m| m.layer_ids().position(|x| x == l)).expect("layer is ordered")
    }

    #[test]
    fn a_canvas_layer_stays_under_a_window_even_when_clicked() {
        for dialog_first in [false, true] {
            let ctx = Context::default();
            for _ in 0..3 {
                frame(&ctx, dialog_first, None);
            }
            assert!(rank(&ctx, "t.overlay") < rank(&ctx, "t.win"));
            // a press on the part of the overlay outside the window raises it in plain egui
            frame(&ctx, dialog_first, Some(egui::pos2(480.0, 140.0)));
            frame(&ctx, dialog_first, None);
            assert!(rank(&ctx, "t.overlay") < rank(&ctx, "t.win"), "first: {dialog_first}");
            assert!(rank(&ctx, "ddc.z.canvas") < rank(&ctx, "t.overlay"));
        }
    }

    #[test]
    fn orders_rise_with_the_stack() {
        let all = [Z::Canvas, Z::Dock, Z::Float, Z::Modal, Z::Menu, Z::Toast];
        assert!(all.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(Z::Dock.order(), Order::Background);
        assert_eq!(Z::Toast.order(), Order::Tooltip);
    }
}
