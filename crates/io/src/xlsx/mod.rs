// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! XLSX round-trip (.xlsx): `export_xlsx` writes the model, `import_xlsx` applies edits as one
//! undoable operation, all or nothing, with a per-cell report.

mod cellsafe;
pub mod measure;
mod delta;
mod dup_cols;
mod baseline;
mod book;
mod export;
mod extra;
mod family;
mod id_write;
mod import;
mod import_gate;
mod layer_match;
mod loss;
mod merge;
mod msg;
mod new_row;
mod no_base;
mod ops_adapter;
mod param_head;
mod scope;
mod sheet_alias;
mod sheet_view;
mod sync;
mod textcode;
mod type_rows;
mod units;
mod value_sources;
mod verify;

pub use cellsafe::{export_notes, take_export_notes, ExportNote, ExportNoteKind, CLIPPED_REASON};
pub use verify::{mismatches, newer, shown_pair, unreadable_message, without_baseline, Mismatch, MismatchKind, Tolerated, VERIFY_TEMPLATES};
pub use delta::{delta_rows, delta_rows_for, export_delta, ref_table, DELTA_HEADERS};
pub use export::{
    export_keyed_table_lines, export_keyed_table_lines_with_key, export_table, KEY_SHEET, export_table_lines, export_xlsx, export_xlsx_units, TableCell, TableLine,
};
pub use export::{export_xlsx_families, export_xlsx_families_from};
pub use extra::{dropped_sheets, lost_user_cells, take_lost_cells, LostCells, PHASE_CREATED, PHASE_DEMOLISHED};
pub use loss::{loss_items, reexport_losses, refusal, LOSS_PREFIX, LOSS_SEP};
pub use family::{
    apply_props, split_pinned_props, import_xlsx_families, import_xlsx_families_hooked, import_xlsx_families_keyed, import_xlsx_families_take, import_xlsx_families_take_keyed, patch_cell, sheet_ids, FamDim,
    FamilyCol, FamilySheet,
    PropEdit, RowDelete, DELETE_HEAD,
};
pub use baseline::{fingerprint, level_fp, with_kept, Applied, AppliedCell, KeptCell, Made, Take};
pub use book::{sheet_headers, sheet_names, Style};
pub use export::{export_xlsx_hooked, export_xlsx_styled, Hook, CANCELLED};
pub use id_write::{write_ids, IdCell, ID_SHEETS};
pub use import::{book_info, book_project, import_xlsx, import_xlsx_hooked, import_xlsx_keyed, import_xlsx_with, BookInfo, ImportOptions};
#[doc(hidden)]
pub use new_row::clone_row_as_new;
pub use type_rows::{changed_types, type_row};
pub use msg::{category_key, column_label, dup_conflict_rows, fill_template, match_template, ptype_key};
pub use scope::{rows_outside, Cat, Scope, Selected};
pub use sheet_alias::{Renamed, SheetCandidate};
pub use ops_adapter::{edit_name, edits_to_envelope, Adapted, Refusal};
pub use sync::{changed as book_changed, Stamp, Watch, POLL, SETTLE_CHECKS, SETTLE_STEP};

/// `Change` values of this column of the format are in metres whatever the book's unit.
pub fn change_is_metres(sheet: &str, column: &str) -> bool {
    let cols: &[Col] = match sheet {
        LEVELS => LEVELS_COLS,
        WALLS => WALLS_COLS,
        OPENINGS => OPENINGS_COLS,
        ROOMS => ROOMS_COLS,
        SLABS => SLABS_COLS,
        CURTAINS => CURTAINS_COLS,
        _ => return false,
    };
    let n = units::norm(column);
    cols.iter()
        .any(|c| c.header.contains("[m]") && units::norm(c.header) == n)
}

/// Workbook units (per project): headers and values of lengths/areas/volumes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Units {
    #[default]
    Metric,
    Imperial,
}

const FT: f64 = 0.3048;

impl Units {
    /// Factor "workbook units -> SI" for the unit given in the header brackets.
    pub(crate) fn factor(self, header: &str) -> f64 {
        if self == Units::Metric {
            return 1.0;
        }
        if header.contains("[m3]") {
            FT * FT * FT
        } else if header.contains("[m2]") {
            FT * FT
        } else if header.contains("[m]") {
            FT
        } else {
            1.0
        }
    }

    pub(crate) fn header(self, header: &str) -> String {
        match self {
            Units::Metric => header.to_string(),
            Units::Imperial => header
                .replace("[m3]", "[ft3]")
                .replace("[m2]", "[ft2]")
                .replace("[m]", "[ft]"),
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Units::Metric => "metric",
            Units::Imperial => "imperial",
        }
    }
}

pub(crate) const INFO: &str = "Info";
/// Hidden sheet with the values as exported: the base of the per-cell comparison on import.
pub(crate) const BASELINE: &str = "Baseline";
/// Hidden sheet: raw column keys of the captions, one row per sheet.
pub(crate) const KEYS: &str = "Keys";
/// Hidden sheet: values of the dropdown lists.
pub(crate) const LISTS: &str = "Lists";
pub(crate) const ROOMS: &str = "Rooms";
pub(crate) const SLABS: &str = "Slabs";
pub(crate) const CURTAINS: &str = "Curtain walls";
pub(crate) const SUMMARY: &str = "QuantitiesSummary";
pub(crate) const COLUMNS: &str = "Columns";
pub(crate) const BEAMS: &str = "Beams";
pub(crate) const ROOFS: &str = "Roofs";
pub(crate) const CEILINGS: &str = "Ceilings";
pub(crate) const SLAB_TYPES: &str = "SlabTypes";
pub(crate) const MEMBER_TYPES: &str = "MemberTypes";

pub(crate) struct Col {
    pub key: &'static str,
    pub header: &'static str,
    pub edit: bool,
}

const fn c(key: &'static str, header: &'static str, edit: bool) -> Col {
    Col { key, header, edit }
}

pub(crate) const LEVELS: &str = "Levels";
pub(crate) const TYPES: &str = "WallTypes";
pub(crate) const LAYERS: &str = "Layers";
/// Layers of the floor, roof and ceiling types (`SLAB_TYPES`, one type list serves all three).
pub(crate) const SLAB_LAYERS: &str = "SlabLayers";
/// Project materials: name, colour, hatch and thermal properties.
pub(crate) const MATERIALS: &str = "Materials";
pub(crate) const WALLS: &str = "Walls";
pub(crate) const OPENINGS: &str = "Openings";
pub(crate) const QUANTITIES: &str = "Quantities";

pub(crate) const LEVELS_COLS: &[Col] = &[
    c("id", "id", false),
    c("name", "name", true),
    c("elevation", "elevation [m]", true),
];
pub(crate) const TYPES_COLS: &[Col] = &[
    c("id", "id", false),
    c("name", "name", true),
    c("layers", "layers (read-only)", false),
    c("total", "total thickness [m] (read-only)", false),
];
pub(crate) const LAYERS_COLS: &[Col] = &[
    c("wtype", "type_id", false),
    c("pos", "pos", false),
    c("type_name", "type (read-only)", false),
    c("material", "material", true),
    c("thickness", "thickness [m]", true),
];
pub(crate) const SLAB_TYPES_COLS: &[Col] = &[
    c("id", "id", false),
    c("name", "name", true),
    c("layers", "layers (read-only)", false),
    c("total", "total thickness [m] (read-only)", false),
];
pub(crate) const SLAB_LAYERS_COLS: &[Col] = &[
    c("stype", "type_id", false),
    c("pos", "pos", false),
    c("type_name", "type (read-only)", false),
    c("material", "material", true),
    c("thickness", "thickness [m]", true),
];
/// Row key of a slab layer in the Baseline and in kept cells: a layer has no id of its own.
pub(crate) fn layer_key(stype: i64, pos: i64) -> i64 {
    stype * 1_000_000 + pos
}
/// Existing materials only: a new material is made in the editor. Colour is `#RRGGBB`.
pub(crate) const MATERIALS_COLS: &[Col] = &[
    c("id", "id", false),
    c("name", "name", true),
    c("color", "color (#RRGGBB)", true),
    c("hatch", "hatch", true),
    c("density", "density [kg/m3]", true),
    c("conductivity", "conductivity [W/(m.K)]", true),
    c("specific_heat", "specific heat [J/(kg.K)]", true),
    c("note", "note", true),
];
pub(crate) const WALLS_COLS: &[Col] = &[
    c("id", "id", false),
    c("level", "level", true),
    c("type", "type", true),
    c("ax", "ax [m]", true),
    c("ay", "ay [m]", true),
    c("bx", "bx [m]", true),
    c("by", "by [m]", true),
    c("base_offset", "base_offset [m]", true),
    c("height", "height [m]", true),
    c("offset0", "offset0 [m]", true),
    c("bulge", "bulge (tan(angle/4), 0 = straight)", true),
    c("length", "length [m] (read-only)", false),
    c("delete", "delete", true),
];
pub(crate) const OPENINGS_COLS: &[Col] = &[
    c("id", "id", false),
    c("wall", "wall_id", true),
    c("along", "along [m]", true),
    c("width", "width [m]", true),
    c("sill", "sill [m]", true),
    c("height", "height [m]", true),
    c("kind", "kind", true),
    c("swing", "swing side (left/right)", true),
    c("hinge", "hinge (start/end)", true),
    c("delete", "delete", true),
];
pub(crate) const ROOMS_COLS: &[Col] = &[
    c("id", "id", false),
    c("level", "level", true),
    c("name", "name", true),
    c("number", "number", true),
    c("px", "point x [m]", true),
    c("py", "point y [m]", true),
    c("height", "height [m]", true),
    c("area", "area [m2] (read-only)", false),
    c("perimeter", "perimeter [m] (read-only)", false),
    c("volume", "volume [m3] (read-only)", false),
    c("delete", "delete", true),
];
pub(crate) const SLABS_COLS: &[Col] = &[
    c("id", "id", false),
    c("level", "level", true),
    c("type", "type", true),
    c("offset", "offset [m]", true),
    c("outline", "outline [m] (x y; x y; ...)", true),
    c("area", "plan area [m2] (read-only)", false),
    c(
        "surface_area",
        "surface area, sloped top [m2] (read-only)",
        false,
    ),
    c("volume", "volume [m3] (read-only)", false),
    c("delete", "delete", true),
];
/// Geometry and grid of an existing curtain wall are edited in the editor (panels are bound to
/// cells); from the workbook — name, level, bottom offset, creation and deletion.
pub(crate) const CURTAINS_COLS: &[Col] = &[
    c("id", "id", false),
    c("level", "level", true),
    c("name", "name", true),
    c("ax", "ax [m]", true),
    c("ay", "ay [m]", true),
    c("bx", "bx [m]", true),
    c("by", "by [m]", true),
    c("bulge", "bulge (tan(angle/4), 0 = straight)", true),
    c("base_offset", "base_offset [m]", true),
    c("height", "height [m]", true),
    c("ugrid", "u grid (s spacing | x positions, metres)", true),
    c("vgrid", "v grid (s spacing | x positions, metres)", true),
    c("area", "area [m2] (read-only)", false),
    c("glass", "glass area [m2] (read-only)", false),
    c("panels", "panels (read-only)", false),
    c("mullions", "mullion length [m] (read-only)", false),
    c("delete", "delete", true),
];
// Parameter sheets: geometry is edited in the editor, the book carries the parameters.
pub(crate) const COLUMNS_COLS: &[Col] = &[
    c("id", "id", false),
    c("level", "level", false),
    c("type", "type", false),
    c("x", "x [m] (read-only)", false),
    c("y", "y [m] (read-only)", false),
    c("base_offset", "base_offset [m] (read-only)", false),
    c("top_offset", "top_offset [m] (read-only)", false),
    c("volume", "volume [m3] (read-only)", false),
];
pub(crate) const BEAMS_COLS: &[Col] = &[
    c("id", "id", false),
    c("level", "level", false),
    c("type", "type", false),
    c("ax", "ax [m] (read-only)", false),
    c("ay", "ay [m] (read-only)", false),
    c("bx", "bx [m] (read-only)", false),
    c("by", "by [m] (read-only)", false),
    c("length", "length [m] (read-only)", false),
    c("volume", "volume [m3] (read-only)", false),
];
pub(crate) const ROOFS_COLS: &[Col] = &[
    c("id", "id", false),
    c("level", "level", false),
    c("type", "type", false),
    c("kind", "kind (read-only)", false),
    c("offset", "offset [m] (read-only)", false),
    c("area", "area [m2] (read-only)", false),
    c("volume", "volume [m3] (read-only)", false),
];
pub(crate) const CEILINGS_COLS: &[Col] = &[
    c("id", "id", false),
    c("level", "level", false),
    c("type", "type", false),
    c("height", "height [m] (read-only)", false),
    c("area", "area [m2] (read-only)", false),
    c("volume", "volume [m3] (read-only)", false),
];
pub(crate) const MEMBER_TYPES_COLS: &[Col] = &[
    c("id", "id", false),
    c("name", "name", false),
    c("shape", "shape (read-only)", false),
    c("material", "material (read-only)", false),
];

/// A sheet whose format columns are all read-only: (sheet, columns, query of its rows in
/// column order, first column the id). Export and the read-only check of the import share it.
pub(crate) struct ParamSheet {
    pub sheet: &'static str,
    pub cols: &'static [Col],
    pub sql: &'static str,
}

pub(crate) const PARAM_SHEETS: [ParamSheet; 6] = [
    ParamSheet {
        sheet: COLUMNS,
        cols: COLUMNS_COLS,
        sql: "SELECT c.id,lv.name,t.name,c.x,c.y,c.base_offset,c.top_offset,q.volume FROM col c \
              JOIN level lv ON lv.id=c.level JOIN member_type t ON t.id=c.mtype \
              LEFT JOIN member_qty q ON q.kind='column' AND q.ref=c.id ORDER BY c.id",
    },
    ParamSheet {
        sheet: BEAMS,
        cols: BEAMS_COLS,
        sql: "SELECT b.id,lv.name,t.name,b.ax,b.ay,b.bx,b.by,q.length,q.volume FROM beam b \
              JOIN level lv ON lv.id=b.level JOIN member_type t ON t.id=b.mtype \
              LEFT JOIN member_qty q ON q.kind='beam' AND q.ref=b.id ORDER BY b.id",
    },
    ParamSheet {
        sheet: ROOFS,
        cols: ROOFS_COLS,
        sql: "SELECT r.id,lv.name,t.name,r.kind,r.offset,MAX(q.area),SUM(q.volume) FROM roof r \
              JOIN level lv ON lv.id=r.level JOIN slab_type t ON t.id=r.stype \
              LEFT JOIN roof_qty q ON q.roof=r.id GROUP BY r.id ORDER BY r.id",
    },
    ParamSheet {
        sheet: CEILINGS,
        cols: CEILINGS_COLS,
        sql: "SELECT c.id,lv.name,t.name,c.height,MAX(q.area),SUM(q.volume) FROM ceiling c \
              JOIN level lv ON lv.id=c.level JOIN slab_type t ON t.id=c.stype \
              LEFT JOIN ceiling_qty q ON q.ceiling=c.id GROUP BY c.id ORDER BY c.id",
    },
    ParamSheet {
        sheet: SLAB_TYPES,
        cols: SLAB_TYPES_COLS,
        sql: "SELECT t.id,t.name,COUNT(l.pos),COALESCE(SUM(l.thickness),0) FROM slab_type t \
              LEFT JOIN slab_type_layer l ON l.stype=t.id GROUP BY t.id ORDER BY t.id",
    },
    ParamSheet {
        sheet: MEMBER_TYPES,
        cols: MEMBER_TYPES_COLS,
        sql: "SELECT id,name,shape,material FROM member_type ORDER BY id",
    },
];

pub(crate) fn param_sheet(name: &str) -> Option<&'static ParamSheet> {
    PARAM_SHEETS.iter().find(|p| p.sheet == name)
}

pub(crate) const SUMMARY_COLS: &[Col] = &[
    c("category", "category", false),
    c("type", "type", false),
    c("level", "level", false),
    c("count", "count", false),
    c("area", "area [m2]", false),
    c("volume", "volume [m3]", false),
];
pub(crate) const QUANTITIES_COLS: &[Col] = &[
    c("wall", "wall_id", false),
    c("pos", "layer_pos (read-only)", false),
    c("material", "material (read-only)", false),
    c("area", "plan_area [m2] (read-only)", false),
    c("volume", "volume [m3] (read-only)", false),
];

/// An accepted (or, on rejections, unapplied) edit.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub sheet: String,
    /// Sheet row, 1-based.
    pub row: u32,
    /// 1-based; 0 — the whole row (creation, deletion).
    pub col: u32,
    pub column: String,
    pub id: Option<i64>,
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rejection {
    pub sheet: String,
    /// 1-based; 0 — not tied to a cell (model error).
    pub row: u32,
    pub col: u32,
    pub column: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct Report {
    /// Scope the book was exported with (Info sheet); empty = the whole model.
    pub scope: String,
    /// Machine form of the scope (Info sheet "scope_spec"); empty = whole model or an older book.
    pub scope_spec: String,
    /// Ids the book's Baseline lists per category sheet (read for scoped books only).
    pub base_ids: Selected,
    /// Edits found in the workbook. Applied only if `applied`.
    pub changes: Vec<Change>,
    pub rejected: Vec<Rejection>,
    /// Accepted cells the preview flags (same shape as a refusal, never blocks or skips).
    pub warnings: Vec<Rejection>,
    pub applied: bool,
    /// Batch for the model (one operation, one undo); with rejections it is incomplete, do not apply.
    pub edits: Vec<ddc_model::Edit>,
    /// Unknown columns of category sheets → new project parameters (on opt-in).
    pub proposals: Vec<Proposal>,
    /// Edits the workbook does not pass to the model (read-only sheet, new row…).
    pub ignored: Vec<Ignored>,
    /// Edits of family sheets (`FamilySheet`); the caller applies them together with `edits`.
    pub props: Vec<PropEdit>,
    /// Rows without an id that were matched to an existing element by key (mark/name).
    pub matches: Vec<KeyMatch>,
    /// Cells changed both in the book and in the model since the export: never applied.
    pub conflicts: Vec<Conflict>,
    /// Conflicting cells left on the model value; a re-export keeps them (see `with_kept`).
    pub kept: Vec<KeptCell>,
    /// Elements whose rows are gone from the book and that stay in the model (delete option off).
    pub removed: Vec<Removed>,
    /// Sheets nothing was read from: missing, emptied or unreadable (see `SheetIssueKind`).
    pub sheet_issues: Vec<SheetIssue>,
    /// Sheets named almost like a known one (case, spaces, copy suffix): not read.
    pub look_alikes: Vec<LookAlike>,
    /// Sheets with another name whose header row matches a known sheet: read only when the user
    /// picks one (`KeyDecisions::alias`).
    pub candidates: Vec<SheetCandidate>,
    /// Cells the model changed after the export while the book kept the exported value: the
    /// model value stays, nothing is reverted.
    pub model_newer: usize,
    /// (sheet, row) of each row this report creates -> its fingerprint (see `Take::made`).
    pub fingerprints: std::collections::HashMap<(String, u32), String>,
    /// Per sheet: how many elements "delete missing" removes (see `is_mass_delete`).
    pub deletions: Vec<SheetDeletion>,
    /// The book has no whole baseline (dropped, damaged or never written): every differing cell
    /// counts as a book edit, model edits since the export cannot be seen, nothing is deleted.
    pub no_base: bool,
    /// The book does not name the project it was exported from (older or hand-made book).
    pub no_project: bool,
    /// Book cells this report takes into the model: the base of the next read when the book is
    /// not re-exported after the Apply (`Take::applied`).
    pub applied_cells: Vec<AppliedCell>,
}

/// Elements of one sheet that "delete missing" removes, out of all the sheet holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetDeletion {
    pub sheet: String,
    pub deleted: usize,
    pub total: usize,
}

impl SheetDeletion {
    /// Whole percent of the sheet's elements, rounded down.
    pub fn percent(&self) -> usize {
        if self.total == 0 {
            0
        } else {
            self.deleted * 100 / self.total
        }
    }

    pub fn is_mass(&self) -> bool {
        is_mass_delete(self.deleted, self.total)
    }
}

/// Mass deletion: more than 20 % of the sheet's elements, or the whole category. A truncated
/// or filtered sheet saved with "delete missing" on looks exactly like this.
pub fn is_mass_delete(deleted: usize, total: usize) -> bool {
    deleted > 0 && (deleted >= total || deleted * 100 > total * 20)
}

/// A cell edited in the book while the model value changed after the export.
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub sheet: String,
    pub row: u32,
    pub col: u32,
    pub column: String,
    pub id: Option<i64>,
    /// Value at the export, as in the model's units.
    pub base: String,
    pub book: String,
    pub model: String,
    /// The user chose the book value: it is in `changes` and `edits` and applies.
    pub take_book: bool,
}

impl Conflict {
    pub fn address(&self) -> String {
        cell_ref(&self.sheet, self.row, self.col)
    }
}

/// An element whose row was removed from the book.
#[derive(Debug, Clone, PartialEq)]
pub struct Removed {
    pub sheet: String,
    pub id: i64,
}

/// Why nothing was read from a sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetIssueKind {
    /// The export wrote it and the book no longer has it (renamed or deleted).
    Missing,
    /// Cleared, or only its header is left, while it held elements.
    Empty,
    /// Its part of the file cannot be parsed.
    Unreadable,
    /// Its values read, its formulas do not: a calculated cell cannot be told from an edit.
    FormulasUnreadable,
}

/// A sheet this import read nothing from. Its elements stay: never a deletion, never a refusal,
/// so the other sheets still apply.
#[derive(Debug, Clone, PartialEq)]
pub struct SheetIssue {
    pub sheet: String,
    pub kind: SheetIssueKind,
    /// Elements the sheet held that stay as they are (0 when the book cannot tell).
    pub kept: usize,
}

impl SheetIssue {
    pub fn new(sheet: &str, kind: SheetIssueKind, kept: usize) -> Self {
        Self { sheet: sheet.into(), kind, kept }
    }

    pub fn missing(sheet: &str) -> Self {
        Self::new(sheet, SheetIssueKind::Missing, 0)
    }
}

/// State of a by-key match; `Pending` rows apply nothing until the user decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchState {
    Pending,
    Accepted,
    /// Row does not match: it creates a new element. Stays listed so the choice can be undone.
    Rejected,
}

/// A workbook row with no id whose key equals exactly one existing element.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyMatch {
    pub sheet: String,
    /// Row of the sheet, 1-based.
    pub row: u32,
    pub id: i64,
    /// Table of the element (`room`, `curtain_wall`).
    pub kind: &'static str,
    /// Key text that matched (mark or name).
    pub key: String,
    pub state: MatchState,
    /// Cell edits the match would make (for a pending match they are not in `Report::changes`).
    pub changes: Vec<Change>,
}

impl KeyMatch {
    pub fn address(&self) -> String {
        cell_ref(&self.sheet, self.row, 0)
    }
}

/// User decisions on by-key matches, keyed by (sheet, row). Neither set: pending.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeyDecisions {
    /// Row is applied to the matched element.
    pub accept: std::collections::HashSet<(String, u32)>,
    /// Row is not matched: it creates a new element, as before. Also a copied row repeating
    /// an id with other edits: read without its id (`dup.rs`).
    pub reject: std::collections::HashSet<(String, u32)>,
    /// Per import: known sheet name -> the sheet of the book to read as it. The book is not
    /// renamed or written.
    pub alias: std::collections::HashMap<String, String>,
}

/// Sheets whose names differ from the known sheet `expected` only by case, spaces or a copy
/// suffix. They are not read: the user renames one to `expected`.
#[derive(Debug, Clone, PartialEq)]
pub struct LookAlike {
    pub expected: String,
    pub found: Vec<String>,
}

/// Proposal: an unknown column → a project parameter; values belong to existing rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub sheet: String,
    /// Header cell, 1-based.
    pub row: u32,
    pub col: u32,
    pub header: String,
    pub spec: ddc_model::ParamSpec,
    /// (sheet row, row table, id, value).
    pub values: Vec<(u32, String, i64, ddc_model::PVal)>,
    pub accept: bool,
    /// Sheet rows the import refused: their values were taken out of `values`, never applied.
    pub refused: Vec<u32>,
}

impl Proposal {
    /// Every row with a value was refused: the column cannot be ticked and adds nothing.
    pub fn blocked(&self) -> bool {
        self.values.is_empty() && !self.refused.is_empty()
    }

    /// Takes out the values of the refused row `row` (sheet row) or of element `kind`/`id`.
    pub fn refuse(&mut self, row: Option<u32>, kind: &str, id: Option<i64>) {
        let hit = |v: &(u32, String, i64, ddc_model::PVal)| {
            Some(v.0) == row || (v.1 == kind && Some(v.2) == id)
        };
        let gone: Vec<u32> = self.values.iter().filter(|&v| hit(v)).map(|v| v.0).collect();
        if gone.is_empty() {
            return;
        }
        self.values.retain(|v| !hit(v));
        for r in gone {
            if !self.refused.contains(&r) {
                self.refused.push(r);
            }
        }
        if self.blocked() {
            self.accept = false;
        }
    }

    pub fn address(&self) -> String {
        cell_ref(&self.sheet, self.row, self.col)
    }

    /// Definition, then values: in one batch with the other edits.
    pub fn edits(&self) -> Vec<ddc_model::Edit> {
        let mut v = vec![ddc_model::Edit::NewParam(self.spec.clone())];
        v.extend(
            self.values
                .iter()
                .map(|(_, kind, id, val)| match *id {
                    // a negative id marks a new row: `-1 - id` is its sheet row
                    i if i < 0 => ddc_model::Edit::ParamValueNew {
                        param: self.spec.name.clone(),
                        kind: kind.clone(),
                        row: (-1 - i) as u32,
                        value: Some(val.clone()),
                    },
                    _ => ddc_model::Edit::ParamValue {
                        param: self.spec.name.clone(),
                        kind: kind.clone(),
                        id: *id,
                        value: Some(val.clone()),
                    },
                }),
        );
        v
    }

    /// Locale key of the parameter type (`xl.ptype.*`).
    pub fn type_key(&self) -> &'static str {
        ptype_key(self.spec.ptype)
    }

    /// Locale keys of the target categories (`xl.cat.*`), in spec order.
    pub fn category_keys(&self) -> Vec<&'static str> {
        self.spec.categories.iter().map(|c| category_key(c)).collect()
    }
}

/// Workbook edit that does not reach the model; `reason` is the English template text (the app maps it to the UI language).
#[derive(Debug, Clone, PartialEq)]
pub struct Ignored {
    pub sheet: String,
    pub row: u32,
    pub col: u32,
    pub column: String,
    pub id: Option<i64>,
    pub old: String,
    pub new: String,
    pub reason: String,
}

impl Ignored {
    pub fn address(&self) -> String {
        cell_ref(&self.sheet, self.row, self.col)
    }
}

/// Row totals: created, changed (rows with cell edits), deleted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub added: usize,
    pub changed: usize,
    pub deleted: usize,
}

pub(crate) const CREATED: &str = "created";
pub(crate) const DELETED: &str = "deleted";
pub(crate) const MISSING: &str = "deleted (row removed)";

impl Change {
    pub fn kind(&self) -> &'static str {
        match (self.column.as_str(), self.new.as_str()) {
            ("id", CREATED) => "added",
            ("id", DELETED | MISSING) => "deleted",
            _ => "changed",
        }
    }
}

impl Rejection {
    /// Book-level problems stop the whole import; a bad cell or row is only skipped.
    pub fn blocks(&self) -> bool {
        self.row == 0
            || self.sheet == "(model)"
            || self.reason.starts_with("required column missing")
            || self.reason.starts_with("unknown unit in header")
            || self.reason.starts_with("units must be")
            || self.reason.starts_with("header cells are merged")
    }
}

impl Report {
    /// Accepted cells the preview flags.
    pub fn warned(&self) -> usize {
        self.warnings.len()
    }

    /// Problems that stop the import (the rest are cells and rows skipped with a reason).
    pub fn blocking(&self) -> usize {
        self.rejected.iter().filter(|r| r.blocks()).count()
    }

    /// Cells and rows left out of the import, each with its reason.
    pub fn skipped(&self) -> usize {
        self.rejected.len() - self.blocking()
    }

    /// Batch to apply: workbook edits and accepted parameter proposals (one undo).
    pub fn batch(&self) -> Vec<ddc_model::Edit> {
        let mut v = self.edits.clone();
        for p in self.proposals.iter().filter(|p| p.accept && !p.blocked()) {
            v.extend(p.edits());
        }
        v
    }

    /// By-key matches that still wait for Accept or Reject.
    pub fn pending_matches(&self) -> usize {
        self.matches.iter().filter(|m| m.state == MatchState::Pending).count()
    }

    /// By-key matches the user accepted.
    pub fn accepted_matches(&self) -> usize {
        self.matches.iter().filter(|m| m.state == MatchState::Accepted).count()
    }

    /// Rows removed from the book whose elements stay in the model.
    pub fn removed_count(&self) -> usize {
        self.removed.len()
    }

    pub fn accepted(&self) -> usize {
        self.proposals.iter().filter(|p| p.accept && !p.blocked()).count()
    }

    /// Sheets whose deletion needs a second, explicit confirmation.
    pub fn mass_deletes(&self) -> Vec<&SheetDeletion> {
        self.deletions.iter().filter(|d| d.is_mass()).collect()
    }

    /// Records the deletions of one sheet (nothing when `deleted` is 0).
    pub(crate) fn note_deletions(&mut self, sheet: &str, deleted: usize, total: usize) {
        if deleted > 0 {
            self.deletions.push(SheetDeletion { sheet: sheet.into(), deleted, total });
        }
    }

    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        let mut rows = std::collections::HashSet::new();
        for ch in &self.changes {
            match ch.kind() {
                "added" => c.added += 1,
                "deleted" => c.deleted += 1,
                _ => {
                    if rows.insert((ch.sheet.as_str(), ch.row, ch.id)) {
                        c.changed += 1
                    }
                }
            }
        }
        c
    }
}

/// Cell address in workbook notation: `Walls!D7` (`row`/`col` 1-based; 0 — no address).
pub fn cell_ref(sheet: &str, row: u32, col: u32) -> String {
    let q = if sheet.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        sheet.to_string()
    } else {
        format!("'{sheet}'")
    };
    if row == 0 {
        return q;
    }
    if col == 0 {
        return format!("{q}!{row}:{row}");
    }
    let (mut n, mut letters) = (col, String::new());
    while n > 0 {
        let r = ((n - 1) % 26) as u8;
        letters.insert(0, (b'A' + r) as char);
        n = (n - 1) / 26;
    }
    format!("{q}!{letters}{row}")
}

impl Change {
    pub fn address(&self) -> String {
        cell_ref(&self.sheet, self.row, self.col)
    }
}

impl Rejection {
    pub fn address(&self) -> String {
        cell_ref(&self.sheet, self.row, self.col)
    }
}

mod sheet_cells;
pub use sheet_cells::{read_first_of, read_sheet_cells, ReadSheet, SheetCell};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_big;
#[cfg(test)]
mod tests_book;
#[cfg(test)]
mod tests_family;
#[cfg(test)]
mod tests_merge;
#[cfg(test)]
mod tests_loss;
#[cfg(test)]
mod tests_view;
#[cfg(test)]
mod tests_formula_keep;
#[cfg(test)]
mod tests_layer_match;
#[cfg(test)]
mod tests_materials;
#[cfg(test)]
mod tests_scope;
#[cfg(test)]
mod tests_fp;
#[cfg(test)]
mod tests_slab_types;
#[cfg(test)]
mod tests_sync;
#[cfg(test)]
mod tests_alias;
#[cfg(test)]
mod tests_sheet_issues;
#[cfg(test)]
mod tests_exchange;

#[cfg(test)]
mod tests_ri1;
#[cfg(test)]
mod tests_roomdup;
#[cfg(test)]
mod tests_ops;
#[cfg(test)]
mod tests_ops_parity;
#[cfg(test)]
mod tests_rename;
#[cfg(test)]
mod tests_edge;
#[cfg(test)]
mod tests_params;
#[cfg(test)]
mod tests_param_head;
#[cfg(test)]
mod tests_dup_cols;
#[cfg(test)]
mod roundtrip_prop_tests;
#[cfg(test)]
mod tests_xlprop;
#[cfg(test)]
mod roundtrip_sheets_tests;
#[cfg(test)]
mod tests_ri1_fuzz;
#[cfg(test)]
mod tests_cell;
#[cfg(test)]
mod tests_locks;
#[cfg(test)]
mod tests_value_sources;
#[cfg(test)]
mod tests_delword;
#[cfg(test)]
mod verify_tests;
#[cfg(test)]
mod tests_ws;
#[cfg(test)]
mod tests_numtext;
#[cfg(test)]
mod tests_reexport_fp;
#[cfg(test)]
mod tests_untouched_matrix;
