//! The one question card of the open flow, on the shared dialog scaffold (`ui_kit::Dialog`),
//! small and non-modal, low on the window: Enter takes the default, Esc drops everything.
use super::decide::{Ask, Door};
use crate::ui_kit as kit;
use ddc_ui::widgets::Button;
use ddc_ui::Catalog;
use std::path::PathBuf;

pub struct Card {
    /// What the user gave: files and folders as they were.
    pub inputs: Vec<PathBuf>,
    pub door: Door,
    pub kind: Ask,
    pub include_sub: bool,
    pub has_folder: bool,
    pub count: usize,
    pub bytes: u64,
    /// First file name, for the title.
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Wait,
    /// Enter: "Open as new project" / "Open all".
    Primary,
    /// "Add to this project".
    Secondary,
    Cancel,
}

pub fn show(ctx: &egui::Context, c: &mut Card, cat: &Catalog) -> Choice {
    let mut open = true;
    let mut choice = Choice::Wait;
    let screen = ctx.screen_rect();
    let title = match c.kind {
        Ask::ModelDrop => cat.tf("open_file.card.model.title", &[("name", c.name.as_str())]),
        Ask::Large => cat.tf(
            "open_file.card.large.title",
            &[("n", c.count.to_string().as_str())],
        ),
    };
    let th = ddc_ui::theme::of(ctx);
    // a dropped model offers three choices: their run is wider than the confirm class
    let class = if c.kind == Ask::ModelDrop {
        kit::DialogSize::Form
    } else {
        kit::DialogSize::Confirm
    };
    kit::Dialog::new("open.card", title, class)
        .open(&mut open)
        .first_pos(egui::pos2(
            (screen.center().x - 180.0).max(16.0),
            screen.top() + screen.height() * 0.6,
        ))
        .show(ctx, |ui| {
            match c.kind {
                Ask::ModelDrop => {
                    ui.label(cat.t("open_file.card.model.body"));
                }
                Ask::Large => {
                    let body = cat.tf(
                        "open_file.card.large.body",
                        &[
                            ("n", c.count.to_string().as_str()),
                            ("size", crate::ui_kit::fmt_size(cat, c.bytes).as_str()),
                        ],
                    );
                    ui.label(body);
                }
            }
            if c.has_folder {
                ui.checkbox(&mut c.include_sub, cat.t("open_file.card.subfolders"));
            }
            kit::dialog_footer(ui, |ui| {
                let primary = match c.kind {
                    Ask::ModelDrop => cat.t("open_file.card.new"),
                    Ask::Large => cat.t("open_file.card.open_all"),
                };
                if kit::primary_action(ui, primary, None) {
                    choice = Choice::Primary;
                }
                if kit::cancel_button(ui, cat.t("open_file.card.cancel")).clicked() {
                    choice = Choice::Cancel;
                }
                if c.kind == Ask::ModelDrop
                    && Button::quiet(&cat.t("open_file.card.add")).show(ui, &th).clicked()
                {
                    choice = Choice::Secondary;
                }
            });
        });
    if !open && choice == Choice::Wait {
        choice = Choice::Cancel;
    }
    choice
}

