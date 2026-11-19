//! Typed refusals and notes of a staged Add, so the UI can localize them without showing the
//! internal English text.

/// Why a staged Add was refused (or, for `CopyHeld`, a note after a finished one).
#[derive(Debug, Clone, PartialEq)]
pub enum StageErr {
    /// The copy of an earlier Add is still attached.
    PrevCopyAttached,
    /// The model changed while the file was read.
    ModelChanged,
    /// The imported copy carries no change record.
    NoChangeRecord,
    /// A table has other columns in the model and in the copy.
    TableDiffers(String),
    /// A unique key of the table cannot be followed.
    UniqueKey(String),
    /// The import changed a table the copy no longer has.
    TableGone(String),
    /// Note: the Add is done, its imported copy is still open.
    CopyHeld,
}

impl StageErr {
    /// Catalog key of the user message (`stage_err.stage.*`).
    pub fn key(&self) -> &'static str {
        match self {
            StageErr::PrevCopyAttached => "stage_err.stage.prev_attached",
            StageErr::ModelChanged => "stage_err.stage.model_changed",
            StageErr::NoChangeRecord => "stage_err.stage.no_record",
            StageErr::TableDiffers(_) => "stage_err.stage.table_differs",
            StageErr::UniqueKey(_) => "stage_err.stage.unique_key",
            StageErr::TableGone(_) => "stage_err.stage.table_gone",
            StageErr::CopyHeld => "stage_err.stage.copy_held",
        }
    }

    /// Message arguments by placeholder name.
    pub fn args(&self) -> Vec<(&'static str, String)> {
        match self {
            StageErr::TableDiffers(t) | StageErr::UniqueKey(t) | StageErr::TableGone(t) => {
                vec![("t", t.clone())]
            }
            _ => vec![],
        }
    }
}

impl std::fmt::Display for StageErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StageErr::PrevCopyAttached => f.write_str(
                "Add refused: the copy of the previous Add is still attached; run Add again",
            ),
            StageErr::ModelChanged => {
                f.write_str("Add refused: the model changed while the file was read; run Add again")
            }
            StageErr::NoChangeRecord => {
                f.write_str("Add refused: the imported copy has no change record")
            }
            StageErr::TableDiffers(t) => write!(
                f,
                "Add refused: table {t} differs between the model and the imported copy"
            ),
            StageErr::UniqueKey(t) => {
                write!(f, "Add refused: a unique key of table {t} cannot be followed")
            }
            StageErr::TableGone(t) => write!(
                f,
                "Add refused: the import changed table {t}, which the copy no longer has"
            ),
            StageErr::CopyHeld => f.write_str(
                "Add done; its imported copy stays open until the next Add or reopening the model",
            ),
        }
    }
}

impl From<StageErr> for crate::Error {
    fn from(e: StageErr) -> Self {
        crate::Error::Stage(e)
    }
}
