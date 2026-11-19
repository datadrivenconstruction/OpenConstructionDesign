//! Option recipes in the op language are frozen when the option is made, on the model the
//! person sees: every `Filter` and `Set` target becomes the refs it matches there, and that
//! envelope is what is stored, shown and applied. The recipe's base head pins that model, so
//! an Apply after the model moved is refused stale before anything runs.

use super::freeze::{freeze, needs_freeze};
use super::sets::Sets;
use crate::editor::Editor;
use ddc_model::{Model, OptionApplyError, OptionRec, RecipeStore};
use ddc_ops::{Code, Envelope, Rejection};

/// `rec` with its envelope frozen against `ed`. Refused stale when `ed` is not the recipe's
/// base; a recipe that is not an op envelope (a generator recipe) comes back unchanged, and an
/// envelope without filter or set targets keeps its exact text.
pub fn freeze_recipe(
    ed: &Editor,
    sets: Option<&Sets>,
    rec: &OptionRec,
) -> Result<OptionRec, OptionApplyError> {
    let head = ed.model.ai_option_head().map_err(|e| OptionApplyError::Model(e.to_string()))?;
    if head != (rec.base_head, rec.base_stamp) {
        return Err(OptionApplyError::Stale);
    }
    let mut out = rec.clone();
    let Ok(env) = Envelope::from_json(&rec.envelope) else {
        return Ok(out);
    };
    if !needs_freeze(&env) {
        return Ok(out);
    }
    let frozen = freeze(ed, sets, &env).map_err(OptionApplyError::Rejected)?;
    out.envelope = frozen.to_json().map_err(|e| OptionApplyError::Envelope(e.to_string()))?;
    Ok(out)
}

/// The one way an option recipe is stored: frozen on `ed` (the model at the recipe's base),
/// then added; nothing is stored when it is refused. A producer without that model (`ed` is
/// `None`, the generator worker) may store only recipes that need no freezing: generator
/// recipes and envelopes whose targets are already refs.
pub fn add_option(
    ed: Option<&Editor>,
    sets: Option<&Sets>,
    store: &dyn RecipeStore,
    rec: &OptionRec,
) -> Result<i64, OptionApplyError> {
    let frozen = match ed {
        Some(ed) => freeze_recipe(ed, sets, rec)?,
        None => {
            unfrozen_refusal(&rec.envelope)?;
            rec.clone()
        }
    };
    store.add_recipe(&frozen).map_err(|e| OptionApplyError::Model(e.to_string()))
}

/// `Err` when `envelope` is an op envelope that still names a filter or a set.
fn unfrozen_refusal(envelope: &str) -> Result<(), OptionApplyError> {
    match Envelope::from_json(envelope) {
        Ok(env) if needs_freeze(&env) => Err(OptionApplyError::Rejected(
            Rejection::new(Code::Unsupported)
                .got("the option targets a filter or a set that was not frozen when it was made"),
        )),
        _ => Ok(()),
    }
}

/// Refuses a stored recipe whose envelope still names a filter or a set: Apply runs only what
/// was frozen when the option was made, never a fresh match.
pub fn check_frozen(m: &Model, id: i64) -> Result<(), OptionApplyError> {
    let Some(rec) = m.ai_option_get(id).map_err(|e| OptionApplyError::Model(e.to_string()))? else {
        return Ok(());
    };
    unfrozen_refusal(&rec.envelope)
}

/// [`check_frozen`] of a recipe not in the model (a session store's), read before any write.
pub fn check_frozen_rec(rec: &ddc_model::OptionRec) -> Result<(), OptionApplyError> {
    unfrozen_refusal(&rec.envelope)
}

#[cfg(test)]
#[path = "option_freeze_tests.rs"]
mod tests;
