//! One-click fix of a check finding, without a plan: one journal row, one undo step.

use crate::editor::{Editor, OpFn};
use ddc_model::checks::Finding;
use ddc_model::Model;
use std::sync::Arc;

pub fn fix_now(ed: &mut Editor, f: &Finding) -> Result<(), ddc_model::Error> {
    let fix = f.fix.clone();
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.apply_check_fix(&fix))))
}
