//! F6 / Shift+F6: keyboard focus cycles over the visible panes (browser, canvas, properties,
//! chat). Each pane draws one focusable landmark; the request lands on it and the shared focus
//! ring (`widgets::focus_ring`) shows where the focus is.

use std::cell::Cell;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Browser,
    Canvas,
    Props,
    Chat,
}

/// Fixed cycle order.
pub const ORDER: [Pane; 4] = [Pane::Browser, Pane::Canvas, Pane::Props, Pane::Chat];

impl Pane {
    fn idx(self) -> usize {
        ORDER.iter().position(|p| *p == self).unwrap_or(0)
    }
    fn id(self) -> egui::Id {
        egui::Id::new(("ddc.pane.anchor", self.idx()))
    }
    /// The canvas reuses the viewport's own widget id, remembered by [`land_canvas`].
    fn focus_id(self, ctx: &egui::Context) -> egui::Id {
        match self {
            Pane::Canvas => ctx
                .data(|d| d.get_temp::<egui::Id>(egui::Id::new("ddc.pane.canvas")))
                .unwrap_or_else(|| self.id()),
            _ => self.id(),
        }
    }
}

/// Next visible pane after `cur` (forward or backward, wrapping). Without a current pane the
/// cycle starts at the first (forward) or last (backward) visible one.
pub fn cycle(visible: [bool; 4], cur: Option<Pane>, back: bool) -> Option<Pane> {
    let n = ORDER.len();
    let start = cur.map(Pane::idx);
    (1..=n)
        .map(|step| match (start, back) {
            (Some(s), false) => (s + step) % n,
            (Some(s), true) => (s + n - step % n) % n,
            (None, false) => step - 1,
            (None, true) => n - step,
        })
        .find(|i| visible[*i])
        .map(|i| ORDER[i])
}

/// What F6 does now: nothing while a modal window is up or a text field has focus.
pub fn plan(
    visible: [bool; 4],
    cur: Option<Pane>,
    back: bool,
    modal: bool,
    typing: bool,
) -> Option<Pane> {
    if modal || typing {
        return None;
    }
    cycle(visible, cur, back)
}

thread_local! {
    /// Set by the command handlers, taken once per frame by [`apply`] (`true` = backwards).
    static PENDING: Cell<Option<bool>> = const { Cell::new(None) };
}

pub fn request(back: bool) {
    PENDING.with(|p| p.set(Some(back)));
}

fn take() -> Option<bool> {
    PENDING.with(|p| p.take())
}

const REQ: &str = "ddc.pane.req";
const LAST: &str = "ddc.pane.last";

/// Once per frame before the panels: turns a pending F6 into a landing request.
pub fn apply(ctx: &egui::Context, visible: [bool; 4], modal: bool) {
    let Some(back) = take() else { return };
    let cur = current(ctx);
    let typing = text_focused(ctx);
    if let Some(p) = plan(visible, cur, back, modal, typing) {
        let pass = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(REQ), (p.idx(), pass)));
    }
}

/// True only while a text field owns the focus (`wants_keyboard_input` is true for any focused
/// widget in egui 0.32, including the pane landmarks F6 itself focuses).
fn text_focused(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.focused())
        .is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some())
}

fn current(ctx: &egui::Context) -> Option<Pane> {
    if let Some(p) = ORDER
        .iter()
        .copied()
        .find(|p| ctx.memory(|m| m.has_focus(p.focus_id(ctx))))
    {
        return Some(p);
    }
    ctx.data(|d| d.get_temp::<usize>(egui::Id::new(LAST)))
        .map(|i| ORDER[i.min(ORDER.len() - 1)])
}

/// The project browser is the pane that last held focus (its tree rows, search field or anchor).
pub fn browser_active(ctx: &egui::Context) -> bool {
    current(ctx) == Some(Pane::Browser)
}

/// Plain Ctrl+F while the browser is active: the browser search takes it and the global Find
/// command skips it (same test on both sides, so exactly one of them runs).
pub fn browser_ctrl_f(ctx: &egui::Context, key: egui::Key, m: egui::Modifiers) -> bool {
    plain_ctrl_f(key, m) && browser_active(ctx)
}

pub fn plain_ctrl_f(key: egui::Key, m: egui::Modifiers) -> bool {
    key == egui::Key::F && m.command && !m.shift && !m.alt
}

/// Call with the pane's landmark response; takes focus when F6 chose this pane.
pub fn land(ctx: &egui::Context, pane: Pane, resp: &egui::Response) {
    let req_id = egui::Id::new(REQ);
    let req = ctx.data(|d| d.get_temp::<(usize, u64)>(req_id));
    if let Some((i, pass)) = req {
        // a request for a pane that is not drawn must not linger
        if ctx.cumulative_pass_nr() > pass + 3 {
            ctx.data_mut(|d| d.remove_temp::<(usize, u64)>(req_id));
        } else if i == pane.idx() {
            resp.request_focus();
            ctx.data_mut(|d| d.remove_temp::<(usize, u64)>(req_id));
        }
    }
    if resp.has_focus() {
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(LAST), pane.idx()));
    }
}

/// Thin focusable strip at the top of a side pane (the pane's first tab stop).
pub fn anchor(ui: &mut egui::Ui, pane: Pane) -> egui::Response {
    let w = ui.available_width();
    let resp = ui.interact(
        egui::Rect::from_min_size(ui.cursor().min, egui::vec2(w, 4.0)),
        pane.id(),
        egui::Sense::focusable_noninteractive(),
    );
    ui.add_space(4.0);
    land(ui.ctx(), pane, &resp);
    resp
}

/// Landmark over an existing widget (the canvas): same id scheme, no extra strip.
pub fn land_canvas(ctx: &egui::Context, resp: &egui::Response) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("ddc.pane.canvas"), resp.id));
    // screen readers (and the layout probe) tell the drawing from the chrome around it
    ctx.accesskit_node_builder(resp.id, |n| n.set_role(egui::accesskit::Role::Canvas));
    land(ctx, Pane::Canvas, resp);
}

fn h_next(_: &mut super::EditorApp) {
    request(false);
}

fn h_prev(_: &mut super::EditorApp) {
    request(true);
}

pub fn register(app: &mut super::EditorApp) {
    app.register_command("ui.pane.next", h_next);
    app.register_command("ui.pane.prev", h_prev);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [bool; 4] = [true; 4];

    #[test]
    fn f6_cycles_visible_panes_in_order() {
        let mut cur = None;
        let mut seen = vec![];
        for _ in 0..5 {
            cur = plan(ALL, cur, false, false, false);
            seen.push(cur.unwrap());
        }
        assert_eq!(
            seen,
            [Pane::Browser, Pane::Canvas, Pane::Props, Pane::Chat, Pane::Browser]
        );
    }

    #[test]
    fn shift_f6_cycles_backwards() {
        let mut cur = None;
        let mut seen = vec![];
        for _ in 0..5 {
            cur = plan(ALL, cur, true, false, false);
            seen.push(cur.unwrap());
        }
        assert_eq!(
            seen,
            [Pane::Chat, Pane::Props, Pane::Canvas, Pane::Browser, Pane::Chat]
        );
    }

    #[test]
    fn f6_ignored_while_modal() {
        assert_eq!(plan(ALL, Some(Pane::Canvas), false, true, false), None);
        assert_eq!(plan(ALL, Some(Pane::Canvas), true, true, false), None);
    }

    #[test]
    fn f6_ignored_while_typing() {
        assert_eq!(plan(ALL, Some(Pane::Canvas), false, false, true), None);
    }

    #[test]
    fn hidden_panes_are_skipped() {
        let v = [false, true, false, true];
        assert_eq!(cycle(v, Some(Pane::Canvas), false), Some(Pane::Chat));
        assert_eq!(cycle(v, Some(Pane::Chat), false), Some(Pane::Canvas));
        assert_eq!(cycle(v, Some(Pane::Canvas), true), Some(Pane::Chat));
        assert_eq!(cycle(v, None, false), Some(Pane::Canvas));
        assert_eq!(cycle([false; 4], None, false), None);
        // only the current pane visible: stays there
        assert_eq!(
            cycle([false, true, false, false], Some(Pane::Canvas), false),
            Some(Pane::Canvas)
        );
    }

    fn frame(ctx: &egui::Context, text: &mut String, focus_text: bool) {
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let anchors: Vec<_> = [Pane::Browser, Pane::Canvas]
                    .into_iter()
                    .map(|p| anchor(ui, p))
                    .collect();
                let _ = anchors;
                let te = ui.text_edit_singleline(text);
                if focus_text {
                    te.request_focus();
                }
            });
        });
    }

    #[test]
    fn repeated_f6_keeps_cycling_over_focused_landmarks() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        frame(&ctx, &mut text, false);
        let vis = [true, true, false, false];
        let mut seen = vec![];
        for _ in 0..4 {
            request(false);
            apply(&ctx, vis, false);
            frame(&ctx, &mut text, false);
            frame(&ctx, &mut text, false);
            assert!(!text_focused(&ctx));
            seen.push(current(&ctx).unwrap());
        }
        assert_eq!(seen, [Pane::Browser, Pane::Canvas, Pane::Browser, Pane::Canvas]);
    }

    #[test]
    fn text_field_focus_blocks_f6() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        frame(&ctx, &mut text, true);
        frame(&ctx, &mut text, false);
        assert!(text_focused(&ctx));
    }

    #[test]
    fn ctrl_f_goes_to_the_browser_only_while_it_is_active() {
        let (f, cmd) = (egui::Key::F, egui::Modifiers::COMMAND);
        assert!(plain_ctrl_f(f, cmd));
        assert!(!plain_ctrl_f(f, cmd | egui::Modifiers::SHIFT));
        assert!(!plain_ctrl_f(egui::Key::H, cmd));
        let ctx = egui::Context::default();
        assert!(!browser_ctrl_f(&ctx, f, cmd), "no pane yet: the global Find runs");
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(LAST), Pane::Browser.idx()));
        assert!(browser_ctrl_f(&ctx, f, cmd), "browser active: its search takes Ctrl+F");
    }
}
