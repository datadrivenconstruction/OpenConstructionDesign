// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! IFC: step — STEP writer and reference check, export — walls to IFC4X3_ADD2,
//! reader — STEP parser, import — IFC2X3/IFC4/IFC4X1..IFC4X3 (and IFCZIP) into native
//! elements, generic meshes and properties.
//! big — big files: streaming open, plan off the UI thread, bulk commit.

mod big;
mod export;
mod import;
pub mod patch;
mod reader;
mod schema_spec;
mod schema_table;
mod source_ids;
mod step;
#[cfg(test)]
mod steel_frame_tests;
mod validate;
mod zip;

pub use big::{commit_plan, commit_plan_ctl, commit_plan_with, import_ifc_path_ctl, plan_file};
pub use export::sink::{
    export_ifc_file, export_ifc_file_with, export_ifc_to, file_sha256, file_sha256_until, verify_ifc_over_source,
    ExportStats, NotExported,
};
pub use export::comp_types::{
    classification as component_classification, type_class as component_type_class,
    CLASSIFICATION_PARAM,
};
pub use export::{
    export_ifc, export_ifc_with, export_ifc_with_warnings, write_ifc_file, IfcOptions, IfcSchema,
};
pub use validate::{validate_ifc, IfcIssue, IfcReport, RULES};
pub use import::{
    import_ifc, import_ifc_file, import_ifc_file_with, import_ifc_path, import_plan, plan,
    plan_bytes, IfcFile, IfcImporter, ImportError, ImportReport, PElem, PEnd, PLevel, PMember,
    PMepLink, PMesh, POpening, PProp, PRailing, PRamp, PSolid, PStair, PType, PVal, PWall, Plan,
};
pub use reader::{Args, OpenError, StepError, StepErrorKind, StepFile, StreamOptions, Value};
pub use source_ids::{export_ifc_source_guids, is_global_id, write_ifc_source_guids};
pub use step::{check_step, check_step_file, real_checked};
pub use import::{type_drafts, TypeDraft};
pub(crate) use import::item_parts;
