//! Three-step card shown once on the sample project: select a wall, change a property, then
//! take it through XLSX (edit, preview, Apply). Skippable; once finished or dismissed it never
//! comes back (`Prefs::first_card_done`). The step logic is plain data so it is testable.
use crate::prefs::Prefs;
use crate::shell::EditorApp;
use ddc_ui::theme::{space, stroke, Role};
use ddc_ui::widgets::{self, Button};
use ddc_ui::Theme;
use std::cell::{Cell, RefCell};

/// Card width including its margins.
const CARD_W: f32 = 340.0;

/// How a step is drawn: done (check), the one in progress, or still ahead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Done,
    Current,
    Pending,
}

pub fn mark_of(i: usize, cur: usize) -> Mark {
    match i.cmp(&cur) {
        std::cmp::Ordering::Less => Mark::Done,
        std::cmp::Ordering::Equal => Mark::Current,
        std::cmp::Ordering::Greater => Mark::Pending,
    }
}

/// Height of the view chips row at the canvas bottom-left (view name, scale, Fit).
pub const VIEW_CHIPS_H: f32 = 40.0;

/// Bottom-left corner of the card: above the status bar and above the view chips (scale, Fit),
/// so it never covers the bar, its recent commands or the zoom controls.
pub fn anchor(screen: egui::Rect, status_h: f32) -> egui::Pos2 {
    egui::pos2(
        screen.left() + space::LG,
        screen.bottom() - status_h - VIEW_CHIPS_H - space::LG,
    )
}

fn step_row(ui: &mut egui::Ui, th: &Theme, mark: Mark, text: &str) {
    let s = th.sem();
    ui.horizontal(|ui| {
        let side = th.d().icon;
        let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::hover());
        let c = rect.center();
        match mark {
            Mark::Done => {
                widgets::with_icons(ui.ctx(), |icons| {
                    icons.paint(ui, "check", th.icon_px(), rect, s.success)
                });
            }
            Mark::Current => {
                let r = side / 2.0 - 2.0;
                ui.painter()
                    .circle_stroke(c, r, egui::Stroke::new(1.5, s.accent));
                ui.painter().circle_filled(c, r / 2.0, s.accent);
            }
            Mark::Pending => {
                let r = side / 2.0 - 2.0;
                ui.painter()
                    .circle_stroke(c, r, egui::Stroke::new(stroke::HAIRLINE, s.border_control));
            }
        }
        let (role, col) = match mark {
            Mark::Done => (Role::Body, s.text_secondary),
            Mark::Current => (Role::BodyStrong, s.text_primary),
            Mark::Pending => (Role::Body, s.text_primary),
        };
        ui.add(egui::Label::new(th.rich(role, text).color(col)).wrap());
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    SelectWall,
    ChangeProperty,
    XlsxRoundTrip,
}

pub const STEPS: [Step; 3] = [Step::SelectWall, Step::ChangeProperty, Step::XlsxRoundTrip];

/// What the editor shows this frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Seen {
    pub wall_selected: bool,
    pub revision: u64,
    /// The XLSX round trip was applied since the last frame.
    pub xlsx_applied: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Card {
    pub active: bool,
    /// Index into `STEPS`; `STEPS.len()` means finished.
    pub step: usize,
    base_rev: u64,
}

impl Card {
    pub fn start(&mut self) {
        *self = Card {
            active: true,
            ..Card::default()
        };
    }

    pub fn current(&self) -> Option<Step> {
        if self.active {
            STEPS.get(self.step).copied()
        } else {
            None
        }
    }

    /// Advance on what is seen. True when the last step has just been completed.
    pub fn observe(&mut self, s: Seen) -> bool {
        match self.current() {
            Some(Step::SelectWall) if s.wall_selected => {
                self.step = 1;
                self.base_rev = s.revision;
            }
            Some(Step::ChangeProperty) if s.revision != self.base_rev => self.step = 2,
            Some(Step::XlsxRoundTrip) if s.xlsx_applied => {
                self.step = STEPS.len();
                self.active = false;
                return true;
            }
            _ => {}
        }
        false
    }

    pub fn dismiss(&mut self) {
        self.active = false;
    }
}

/// The card is offered only while neither finished nor dismissed before.
pub fn wanted(p: &Prefs) -> bool {
    !p.first_card_done
}

thread_local! {
    static CARD: RefCell<Card> = RefCell::new(Card::default());
    static APPLIED: Cell<bool> = const { Cell::new(false) };
}

/// The XLSX Apply went through (called from `xlsx_win::apply_preview`).
pub fn note_xlsx_applied() {
    APPLIED.with(|a| a.set(true));
    super::tutorial::note_xlsx_applied();
}

/// After the sample project is open: show the card unless it was done or dismissed before.
pub fn offer(app: &EditorApp) {
    if wanted(&app.prefs) {
        APPLIED.with(|a| a.set(false));
        CARD.with(|c| c.borrow_mut().start());
    }
}

pub fn frame(app: &mut EditorApp, ctx: &egui::Context) {
    let mut card = CARD.with(|c| c.borrow().clone());
    if !card.active {
        return;
    }
    if !wanted(&app.prefs) {
        card.dismiss();
        CARD.with(|c| *c.borrow_mut() = card);
        return;
    }
    let seen = Seen {
        wall_selected: app
            .ed
            .selection
            .iter()
            .any(|s| matches!(s, crate::editor::Sel::Wall(_))),
        revision: app.ed.revision,
        xlsx_applied: APPLIED.with(|a| a.take()),
    };
    let finished = card.observe(seen);
    let cur = card.step;
    let c = &app.sh.cat;
    let (title, skip, xlsx_btn) = (
        c.t("first_card.card.title"),
        c.t("first_card.card.skip"),
        c.t("first_card.card.spreadsheet"),
    );
    let rows: Vec<String> = (0..STEPS.len())
        .map(|i| c.t(&format!("first_card.card.step{}", i + 1)))
        .collect();
    let finish_text = c.t("first_card.card.done");
    let (mut skipped, mut xlsx) = (false, false);
    if card.active {
        let th = ddc_ui::theme::of(ctx);
        let pos = anchor(ctx.screen_rect(), crate::shell::status_bar::BAR_H);
        egui::Area::new(egui::Id::new("ddc.help.first_card"))
            .order(ddc_ui::z::Z::Canvas.order())
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                let s = th.sem();
                egui::Frame::new()
                    .fill(s.bg_elevated)
                    .stroke(egui::Stroke::new(stroke::HAIRLINE, s.border_overlay))
                    .corner_radius(th.radius("lg"))
                    .shadow(th.shadow(2))
                    .inner_margin(egui::Margin::same(space::MD as i8))
                    .show(ui, |ui| {
                        ui.set_width(CARD_W - 2.0 * space::MD);
                        ui.label(th.rich(Role::BodyStrong, &title).color(s.text_primary));
                        ui.add_space(space::SM);
                        for (i, text) in rows.iter().enumerate() {
                            step_row(ui, &th, mark_of(i, cur), text);
                            ui.add_space(space::XS);
                        }
                        ui.add_space(space::SM);
                        ui.horizontal(|ui| {
                            if cur == 2 && Button::primary(&xlsx_btn).small().show(ui, &th).clicked() {
                                xlsx = true;
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if Button::quiet(&skip).small().show(ui, &th).clicked() {
                                        skipped = true;
                                    }
                                },
                            );
                        });
                    });
            });
    }
    if finished {
        app.sh
            .toasts
            .push(ddc_ui::notify::Level::Info, finish_text, None);
    }
    if finished || skipped {
        card.dismiss();
        app.prefs.first_card_done = true;
        app.prefs.save();
    }
    CARD.with(|c| *c.borrow_mut() = card);
    if xlsx {
        app.xlsx_export();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(wall: bool, rev: u64, applied: bool) -> Seen {
        Seen {
            wall_selected: wall,
            revision: rev,
            xlsx_applied: applied,
        }
    }

    #[test]
    fn first_run_card_steps() {
        let mut c = Card::default();
        assert_eq!(c.current(), None, "not started: nothing shown");
        c.start();
        assert_eq!(c.current(), Some(Step::SelectWall));
        // a model change before a wall is selected does not skip ahead
        assert!(!c.observe(seen(false, 5, false)));
        assert_eq!(c.current(), Some(Step::SelectWall));
        assert!(!c.observe(seen(true, 5, false)));
        assert_eq!(c.current(), Some(Step::ChangeProperty));
        assert!(!c.observe(seen(true, 5, false)), "same revision: no change yet");
        assert!(!c.observe(seen(true, 6, false)));
        assert_eq!(c.current(), Some(Step::XlsxRoundTrip));
        assert!(!c.observe(seen(true, 6, false)), "XLSX step needs the Apply");
        assert!(c.observe(seen(true, 7, true)));
        assert_eq!(c.current(), None);
        assert_eq!(c.step, STEPS.len());
    }

    #[test]
    fn first_card_clears_status_bar() {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1280.0, 800.0));
        let bar = crate::shell::status_bar::BAR_H;
        let p = anchor(screen, bar);
        // the card grows upward from `p`, so its bottom edge must sit above the bar
        assert!(p.y <= screen.bottom() - bar, "card bottom {} overlaps bar", p.y);
        // and clears the view chips (scale, Fit) at the canvas bottom-left
        assert!(p.y <= screen.bottom() - bar - VIEW_CHIPS_H, "card covers the zoom chips");
        assert!(p.x >= screen.left() + space::LG);
        assert!(CARD_W < screen.width());
    }

    #[test]
    fn first_card_marks_follow_the_step() {
        assert_eq!(mark_of(0, 1), Mark::Done);
        assert_eq!(mark_of(1, 1), Mark::Current);
        assert_eq!(mark_of(2, 1), Mark::Pending);
        assert!((0..STEPS.len()).all(|i| mark_of(i, STEPS.len()) == Mark::Done));
    }

    #[test]
    fn first_card_has_no_literal_glyphs() {
        let src = include_str!("first_card.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or(src);
        let esc: String = ['\\', 'u', '{'].iter().collect();
        assert!(!code.contains(&esc), "unicode escape glyph in card code");
        assert!(code.is_ascii(), "non-ASCII glyph in card code");
    }

    #[test]
    fn first_card_texts_exist_in_both_languages() {
        for lang in ["en", "ru"] {
            let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../ui/data/i18n/");
            let t = std::fs::read_to_string(format!("{dir}{lang}/first_card.json")).unwrap();
            for k in ["title", "step1", "step2", "step3", "spreadsheet", "skip", "done"] {
                assert!(t.contains(&format!("\"first_card.card.{k}\"")), "{lang}: first_card.card.{k}");
            }
        }
    }

    #[test]
    fn first_run_card_never_returns_after_dismiss() {
        let mut p = Prefs::default();
        assert!(wanted(&p));
        let mut c = Card::default();
        c.start();
        c.dismiss();
        assert_eq!(c.current(), None);
        assert!(!c.observe(seen(true, 1, true)), "a dismissed card stays closed");
        // dismissal is stored in prefs and survives a restart
        p.first_card_done = true;
        let back = Prefs::parse(&p.text());
        assert!(back.first_card_done && !wanted(&back));
    }
}
