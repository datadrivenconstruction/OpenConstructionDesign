//! Values of a drafted item, editable on the card: a wrong word or number is corrected without
//! asking the model again. Text is read like the properties panel reads it. A valid value goes
//! to the session as a draft edit (the card comes back recompiled, Apply waits for it); refused
//! text stays in the field with its reason. Nothing here writes the model.
//! While a request runs a correction is held, not sent: when the agent's card is in, an
//! untouched value takes it; one the agent changed too shows both and waits for the person.

use super::kit::{self, space, Button};
use super::{Action, Cx, PanelUi};
use crate::ai::plan::{PlanItem, PlanOp, PlanView};
use crate::ai::value_edit::{self, Editable, Field, Label, Value};
use crate::units::{DecimalMark, Units};
use ddc_ui::i18n::Catalog;
use ddc_ui::theme::Role;
use ddc_ui::widgets::field::TextField;
use egui::{self, RichText, Ui};
use std::collections::HashMap;

/// What the card remembers of the person's corrections until Apply, Reject or the next request.
#[derive(Default)]
pub struct Edited {
    /// The text a value showed before its first correction.
    was: HashMap<(u32, Field), String>,
    /// The item that could not take a corrected value, and the compiler's reason.
    refused: Option<(u32, String)>,
    /// A request is running this frame (set by the card): corrections are held.
    pub busy: bool,
    /// Corrections made while a request ran, waiting for the card it brings.
    held: HashMap<(u32, Field), Held>,
    /// Held corrections whose item or value left the card: said, never dropped unsaid.
    lost: Vec<Lost>,
}

/// A correction made while a request ran.
#[derive(Clone, Debug)]
pub struct Held {
    pub value: Value,
    /// The text the person typed.
    pub mine: String,
    /// What the field showed when they typed it.
    pub base: String,
    pub title: String,
    pub label: String,
}

#[derive(Clone, Debug)]
struct Lost {
    title: String,
    label: String,
    mine: String,
}

/// What a held correction does this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeldStep {
    /// The request still runs.
    Wait,
    /// The agent left the value as it was: the person's goes in.
    Send,
    /// The agent wrote the same value: nothing to do.
    Drop,
    /// The agent changed it too: both are shown until the person picks one.
    Choose,
}

pub fn held_step(busy: bool, h: &Held, shown: &str, now: &Value) -> HeldStep {
    if busy {
        HeldStep::Wait
    } else if *now == h.value {
        HeldStep::Drop
    } else if shown == h.base {
        HeldStep::Send
    } else {
        HeldStep::Choose
    }
}

impl Edited {
    pub fn clear(&mut self) {
        let busy = self.busy;
        *self = Edited {
            busy,
            ..Edited::default()
        };
    }

    /// A new request: held corrections (one may still wait for a pick) go on to the card it
    /// brings, so none is dropped unsaid; the rest is cleared.
    pub fn next_request(&mut self) {
        let held = std::mem::take(&mut self.held);
        self.clear();
        self.held = held;
    }

    pub fn hold(&mut self, item: u32, field: Field, h: Held) {
        self.held.insert((item, field), h);
    }

    pub fn held(&self, item: u32, field: &Field) -> Option<&Held> {
        self.held.get(&(item, field.clone()))
    }

    /// A held value waits for the person's pick once the request is over: Apply waits with it.
    pub fn choosing(&self, busy: bool) -> bool {
        !busy && !self.held.is_empty()
    }

    pub fn refused(&mut self, item: u32, why: String) {
        self.refused = Some((item, why));
    }

    pub fn was(&self, item: u32, field: &Field) -> Option<&str> {
        self.was.get(&(item, field.clone())).map(String::as_str)
    }
}

fn label_text(cat: &Catalog, e: &Editable, many: bool) -> String {
    let what = match &e.label {
        Label::Key(k) => cat.t(k),
        Label::Name(n) => n.clone(),
    };
    match &e.field {
        Field::Create { index, .. } if many => {
            cat.tf("ai.value.elem", &[("n", &(index + 1).to_string()), ("what", &what)])
        }
        _ => what,
    }
}

/// The item's editable values under its row; the rest waits behind the row's disclosure.
pub fn values(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    plan: &PlanView,
    it: &PlanItem,
    acts: &mut Vec<Action>,
) {
    let all = value_edit::editables(cx.ed, &plan.items, it);
    if all.is_empty() {
        return;
    }
    let (th, cat) = (cx.th, cx.cat);
    let mark = DecimalMark::for_lang(cat.lang);
    let units = cx.ed.tool.units;
    // a held value is never behind the disclosure: it may need the person's pick
    let held_past = all
        .iter()
        .skip(value_edit::SHOWN)
        .any(|e| st.values.held(it.id, &e.field).is_some());
    let open = st.expanded.contains(&it.id) || held_past;
    let n = if open { all.len() } else { all.len().min(value_edit::SHOWN) };
    let many = matches!(&it.op, PlanOp::Create { items, .. } if items.len() > 1);
    for e in &all[..n] {
        one(ui, st, cx, it, e, many, (units, mark), acts);
    }
    if n < all.len() {
        let more = (all.len() - n).to_string();
        indented(ui, cx, |ui| {
            kit::caption(ui, th, &cat.tf("ai.value.more", &[("n", &more)]));
        });
    }
    if let Some((_, why)) = st.values.refused.as_ref().filter(|(i, _)| *i == it.id) {
        kit::warn_note(ui, th, &cat.tf("ai.value.refused", &[("why", why)]));
        indented(ui, cx, |ui| {
            kit::caption(ui, th, &cat.t("ai.value.refused_next"));
        });
    }
}

/// Held corrections the card that came in has no place for: their item or value is gone.
/// Called by the card after its rows; each is said until Apply, Reject or the next request.
pub fn held_lost(ui: &mut Ui, st: &mut PanelUi, cx: &Cx<'_>, plan: &PlanView) {
    if !st.values.busy && !st.values.held.is_empty() {
        let gone: Vec<(u32, Field)> = st
            .values
            .held
            .keys()
            .filter(|(item, field)| {
                let it = plan.items.iter().find(|i| i.id == *item);
                !it.is_some_and(|it| {
                    let all = value_edit::editables(cx.ed, &plan.items, it);
                    all.iter().any(|e| e.field == *field)
                })
            })
            .cloned()
            .collect();
        for k in gone {
            if let Some(h) = st.values.held.remove(&k) {
                st.values.lost.push(Lost {
                    title: h.title,
                    label: h.label,
                    mine: h.mine,
                });
            }
        }
    }
    for l in &st.values.lost {
        let args = [
            ("mine", l.mine.as_str()),
            ("what", l.label.as_str()),
            ("title", l.title.as_str()),
        ];
        kit::warn_note(ui, cx.th, &cx.cat.tf("ai.value.lost", &args));
    }
}

fn indented(ui: &mut Ui, cx: &Cx<'_>, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_space(cx.th.d().control_sm + space::SM);
        add(ui);
    });
}

#[allow(clippy::too_many_arguments)]
fn one(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    it: &PlanItem,
    e: &Editable,
    many: bool,
    (units, mark): (Units, DecimalMark),
    acts: &mut Vec<Action>,
) {
    let (th, cat) = (cx.th, cx.cat);
    let item = it.id;
    let id = egui::Id::new(("ai.value", item, &e.field));
    let (buf_id, err_id) = (id.with("buf"), id.with("err"));
    let shown = value_edit::shown(e, units, mark);
    let mut text = ui
        .data(|d| d.get_temp::<String>(buf_id))
        .unwrap_or_else(|| shown.clone());
    let mut err = ui.data(|d| d.get_temp::<String>(err_id));
    let label = label_text(cat, e, many);
    let mut out = None;
    indented(ui, cx, |ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        let l = ui.add(
            egui::Label::new(
                RichText::new(&label)
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
            )
            .truncate(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let tip = cat.t("ai.value.tip");
            let o = TextField::new(&mut text)
                .id_salt(id)
                .labelled_by(l.id, &label)
                .error(err.is_some())
                .error_text(err.as_deref().unwrap_or(""))
                .right_aligned()
                .width(ddc_ui::theme::layout::PROP_VALUE)
                .show(ui, th);
            let _ = o.response.clone().on_hover_text(tip);
            out = Some(o);
        });
    });
    let Some(o) = out else { return };
    if o.changed {
        err = None;
    }
    let mut keep = o.response.has_focus();
    if o.cancelled {
        err = None;
        keep = false;
    } else if o.committed && text.trim() != shown {
        match value_edit::check(e, &text, units, cat) {
            // while a request runs the value waits: sent now, it would land on top of what
            // the agent is drafting for the same row
            Ok(v) if v != e.value && st.values.busy => {
                let h = Held {
                    value: v,
                    mine: text.trim().to_owned(),
                    base: shown.clone(),
                    title: it.title.clone(),
                    label: label.clone(),
                };
                st.values.hold(item, e.field.clone(), h);
                err = None;
                keep = false;
            }
            Ok(v) if v != e.value => {
                send(st, item, &e.field, v, &shown, acts);
                err = None;
                keep = false;
            }
            Ok(_) => {
                err = None;
                keep = false;
            }
            // refused text stays in the field with its reason; the draft keeps its value
            Err(why) => {
                err = Some(why);
                keep = true;
            }
        }
    }
    ui.data_mut(|d| {
        if keep || err.is_some() {
            d.insert_temp(buf_id, text.clone());
        } else {
            d.remove_temp::<String>(buf_id);
        }
        if let Some(w) = &err {
            d.insert_temp(err_id, w.clone());
        } else {
            d.remove_temp::<String>(err_id);
        }
    });
    if err.is_none() && held_row(ui, st, cx, item, e, &shown, acts) {
        return;
    }
    if let Some(w) = &err {
        indented(ui, cx, |ui| kit::warn_note(ui, th, w));
    } else if let Some(old) = st.values.was(item, &e.field).filter(|o| *o != shown) {
        let line = cat.tf("ai.value.was", &[("old", old)]);
        indented(ui, cx, |ui| {
            kit::caption(ui, th, &line);
        });
    }
}

/// A correction on its way to the draft: what the field showed is kept as "was", and a
/// refusal of the item is cleared.
fn send(
    st: &mut PanelUi,
    item: u32,
    field: &Field,
    value: Value,
    shown: &str,
    acts: &mut Vec<Action>,
) {
    let key = (item, field.clone());
    st.values.was.entry(key).or_insert_with(|| shown.to_owned());
    if st.values.refused.as_ref().is_some_and(|(i, _)| *i == item) {
        st.values.refused = None;
    }
    acts.push(Action::SetValue {
        item,
        field: field.clone(),
        value,
    });
}

/// The held correction of this value: a caption while the request runs; once the agent changed
/// the value too, both values and a pick. True when it drew the value's note.
fn held_row(
    ui: &mut Ui,
    st: &mut PanelUi,
    cx: &Cx<'_>,
    item: u32,
    e: &Editable,
    shown: &str,
    acts: &mut Vec<Action>,
) -> bool {
    let Some(h) = st.values.held(item, &e.field).cloned() else {
        return false;
    };
    let (th, cat) = (cx.th, cx.cat);
    let key = (item, e.field.clone());
    match held_step(st.values.busy, &h, shown, &e.value) {
        HeldStep::Wait => {
            let line = cat.tf("ai.value.held", &[("mine", h.mine.as_str())]);
            indented(ui, cx, |ui| {
                kit::caption(ui, th, &line);
            });
            true
        }
        HeldStep::Drop => {
            st.values.held.remove(&key);
            false
        }
        HeldStep::Send => {
            st.values.held.remove(&key);
            send(st, item, &e.field, h.value, shown, acts);
            false
        }
        HeldStep::Choose => {
            let args = [("agent", shown), ("mine", h.mine.as_str())];
            kit::warn_note(ui, th, &cat.tf("ai.value.both", &args));
            let mut pick = None;
            indented(ui, cx, |ui| {
                ui.spacing_mut().item_spacing.x = space::SM;
                let mine = cat.tf("ai.value.use_mine", &args);
                if Button::secondary(&mine).show(ui, th).clicked() {
                    pick = Some(true);
                }
                let agent = cat.tf("ai.value.keep_agent", &args);
                if Button::secondary(&agent).show(ui, th).clicked() {
                    pick = Some(false);
                }
            });
            if let Some(mine) = pick {
                st.values.held.remove(&key);
                if mine {
                    send(st, item, &e.field, h.value, shown, acts);
                }
            }
            true
        }
    }
}
