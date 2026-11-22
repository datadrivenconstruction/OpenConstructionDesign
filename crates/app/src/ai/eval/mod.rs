// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! AI evals: cases are data, oracles read the saved model, the executor runs a session
//! without a window.

pub mod case;
pub mod facts;
pub mod fire_oracles;
pub mod form_oracles;
pub mod geom_oracles;
pub mod design_oracles;
pub mod material_oracles;
pub mod mep_oracles;
pub mod metrics;
pub mod oracle;
pub mod query_oracles;
pub mod recorded;
pub mod replay;
pub mod run;
pub mod shape_oracles;
pub mod triggers;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_design;
#[cfg(test)]
mod tests_fire;
#[cfg(test)]
mod tests_h01;
#[cfg(test)]
mod tests_form;
#[cfg(test)]
mod tests_gscript;
#[cfg(test)]
mod tests_mep;
#[cfg(test)]
mod tests_roof_trim;
#[cfg(test)]
mod tests_query;
#[cfg(test)]
mod tests_receipt;
#[cfg(test)]
mod tests_shape;
#[cfg(test)]
mod tests_gsuite;
#[cfg(test)]
mod tests_recorded;
#[cfg(test)]
mod tests_run;
#[cfg(test)]
mod tests_skills;
