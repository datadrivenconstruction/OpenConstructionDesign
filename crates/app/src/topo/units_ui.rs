//! Unit of a survey import: the picker, where its default comes from (file header or a guess by
//! the extent, never silent) and the resulting extent in project units before Import.

use super::window::{pick_row, ImportDlg, ImportKind, LenUnit};
use crate::ui_kit as kit;
use ddc_io::topo::{self, Span, SurveyUnit, UnitGuess, UnitSource};
use ddc_ui::i18n::{Catalog, Lang};
use ddc_ui::theme::Theme;

/// Label key of a unit in the picker.
pub fn unit_key(u: SurveyUnit) -> &'static str {
    match u {
        SurveyUnit::M => "topo_units.unit.m",
        SurveyUnit::Cm => "topo_units.unit.cm",
        SurveyUnit::Mm => "topo_units.unit.mm",
        SurveyUnit::Ft => "topo_units.unit.ft",
        SurveyUnit::UsFt => "topo_units.unit.usft",
        SurveyUnit::In => "topo_units.unit.in",
    }
}

/// Where the default came from, in an explicit language.
pub fn source_text(g: UnitGuess, lang: Lang) -> String {
    let unit = crate::tr_in(lang, unit_key(g.unit), &[]);
    let key = match g.source {
        UnitSource::Header => "topo_units.unit.from_file",
        UnitSource::Extent => "topo_units.unit.suggested",
    };
    crate::tr_in(lang, key, &[("unit", &unit)])
}

/// Extent of the import in project units for the chosen file unit (`raw` in file units).
pub fn extent_text(raw: Option<Span>, unit: SurveyUnit, lu: LenUnit, lang: Lang) -> String {
    let Some(s) = raw else {
        return crate::tr_in(lang, "topo_units.extent.none", &[]);
    };
    let m = s.scaled(unit.meters());
    let (x, y, z) = (lu.format(m.dx, lang), lu.format(m.dy, lang), lu.format(m.dz, lang));
    let u = lu.unit_text(lang);
    crate::tr_in(
        lang,
        "topo_units.extent",
        &[("x", &x), ("y", &y), ("z", &z), ("u", &u)],
    )
}

/// Whether the extent for the chosen unit looks wrong for a site.
pub fn extent_doubtful(raw: Option<Span>, unit: SurveyUnit) -> bool {
    raw.is_some_and(|s| topo::implausible(&s.scaled(unit.meters())))
}

/// What the span depends on; it is read again when this changes.
pub fn span_key(kind: &ImportKind) -> String {
    match kind {
        ImportKind::Xyz { order, .. } => format!("{order:?}"),
        ImportKind::Dxf {
            layers,
            keep_flat,
            inserts,
            ..
        } => {
            let on: Vec<bool> = layers.iter().map(|l| l.1).collect();
            format!("{on:?} {keep_flat} {inserts}")
        }
    }
}

/// Span in file units of what Import would read with the dialog's current choices.
pub fn raw_span(kind: &ImportKind) -> Option<Span> {
    kind.read(SurveyUnit::M).ok().as_ref().and_then(topo::span)
}

/// Unit the file names: DXF `$INSUNITS`, a unit in the survey header or a `# units:` comment.
pub fn file_unit(kind: &ImportKind) -> Option<SurveyUnit> {
    match kind {
        ImportKind::Xyz { text, .. } => topo::xyz_unit(text),
        ImportKind::Dxf { doc, .. } => topo::dxf_unit(doc),
    }
}

/// Reads the span again when the columns, layers or filters changed.
pub fn refresh(d: &mut ImportDlg) {
    let key = span_key(&d.kind);
    if key != d.span_key {
        d.span = raw_span(&d.kind);
        d.span_key = key;
    }
}

/// Unit picker, the default's source and the resulting extent.
pub fn unit_rows(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, d: &mut ImportDlg, lu: LenUnit) {
    kit::section_title(ui, cat.t("topo_units.unit.section"));
    let items: Vec<String> = SurveyUnit::ALL.iter().map(|u| cat.t(unit_key(*u))).collect();
    let mut sel = SurveyUnit::ALL.iter().position(|u| *u == d.unit).unwrap_or(0);
    if pick_row(ui, th, &cat.t("topo_units.unit"), "unit", &mut sel, &items) {
        d.unit = SurveyUnit::ALL[sel];
    }
    kit::hint(ui, source_text(d.guess, cat.lang));
    ui.label(extent_text(d.span, d.unit, lu, cat.lang));
    if extent_doubtful(d.span, d.unit) {
        kit::warn_note(ui, cat.t("topo_units.extent.check"));
    }
}
