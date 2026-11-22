// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Steering a free layout by edits: the zones of a plan (every floor, every apartment) are
//! solved once per problem and kept, so a rebuild after an edit searches again only the zones
//! the edit changed and takes the rest as they were. The result is the same building a cold
//! build gives: the search is deterministic.

/// Zones the calling thread took from the store and zones it searched, since it started.
pub fn zones() -> (usize, usize) {
    super::place::counts()
}

/// The longest zone search of the calling thread since the last call, in ms: what the
/// `layout.search_ms` cap is measured against.
pub fn slowest_ms() -> f64 {
    super::place::slowest()
}

/// Zone searches of the calling thread the `layout.search_ms` safety cap stopped before their
/// node budget, since it started: each such plan may differ on another machine.
pub fn capped() -> usize {
    super::place::capped()
}

/// Forgets every kept zone: the next build searches everything.
pub fn forget() {
    super::place::forget()
}
