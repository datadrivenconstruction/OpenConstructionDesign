// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Coordination of the building services after every system is routed: the metrics of how the
//! runs look ([`route_metrics`]) and a clean-up pass over the finished runs ([`apply`]) that
//! straightens them without losing a joint or adding a clash.

mod lanes;
mod metrics;
mod pass;
mod segs;

pub use metrics::route_metrics;

use super::rules::rule;
use super::{GProject, ProjectSpec};

/// The clean-up pass, while the rule `mep.coord` is on.
pub(crate) fn apply(spec: &ProjectSpec, p: &mut GProject) {
    if rule("mep.coord") > 0.5 {
        pass::run(p, spec.origin);
    }
}
