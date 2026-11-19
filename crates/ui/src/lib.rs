//! ddc-ui: application shell kit on egui — design tokens → style, icons, command registry,
//! fuzzy palette, ribbon, context/status bars, toasts, en/ru localization.
pub mod cmdargs;
pub mod cmdref;
pub mod commands;
pub mod ctx_tips;
pub mod diff_kind;
/// Per-feature fragments embedded by build.rs (`data/commands.d`, `data/i18n/<lang>`).
pub mod fragments {
    include!(concat!(env!("OUT_DIR"), "/fragments.rs"));
}
pub mod frecency;
pub mod fuzzy;
pub mod help;
pub mod hint_card;
pub mod i18n;
pub mod i18n_fonts;
pub mod icon_top;
pub mod icons;
pub mod keymap;
pub mod looks;
pub mod notify;
pub mod numparse;
pub mod px;
pub mod rtl;
mod rtl_table;
pub mod sem;
#[cfg(test)]
mod test_fx;
pub mod shell;
pub mod shortcuts2;
pub mod theme;
pub mod typo;
pub mod tips;
pub mod uxstate;
pub mod widgets;
pub mod saved_ws;
pub mod workspace;
pub mod z;

pub use commands::{Command, Registry};
pub use i18n::{Catalog, Lang};
pub use shell::{Shell, ShellLayout};
pub use theme::{Density, Mode, Theme};
