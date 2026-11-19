//! What a save did: `SaveReport` (kept in `src_target.last_report`), the one-line result strip and
//! the report window. A dedicated panel can take over the window; the data stays.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use web_time::{Duration, Instant};

use ddc_io::ifc::ExportStats;
use ddc_model::source::JsonValue;
use ddc_ui::notify::Level;
use ddc_ui::Catalog;
use egui::{self, RichText};

use super::job::{Backups, WritePlan};
use crate::editor::Sel;
use crate::ui_kit as kit;
use crate::shell::EditorApp;

/// The strip folds to the status bar after this long.
const STRIP_LIFE: Duration = Duration::from_secs(10);
/// Rows kept per report; the total stays exact.
const MAX_ROWS: usize = 500;
const REASON_NO_EXPORTER: &str = "save.reason.no_exporter";
const REASON_LINK: &str = ddc_io::ifc::patch::REASON_LINK;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotWritten {
    /// Model table.
    pub kind: String,
    pub id: Option<i64>,
    /// Catalog key.
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SaveReport {
    pub target: String,
    pub backend: String,
    pub secs: f64,
    pub bytes: u64,
    pub elements: u64,
    pub proxies: u64,
    /// Edits since the previous save to this target; none on a first save.
    pub changed: Option<i64>,
    pub by_kind: BTreeMap<String, u64>,
    pub ids_kept: u64,
    pub ids_new: u64,
    pub ids_conflict: u64,
    pub not_written_total: u64,
    pub not_written: Vec<NotWritten>,
    pub warnings: Vec<String>,
    /// Someone else changed the file since our last write; their version is in `prev`.
    pub target_changed: bool,
    /// An existing file we had no record of was replaced; its content is in `prev`.
    pub replaced_unknown: bool,
    pub prev: Option<String>,
    pub original: Option<String>,
}

fn opt_str(p: &Option<PathBuf>) -> Option<String> {
    p.as_ref().map(|p| p.display().to_string())
}

impl SaveReport {
    #[allow(clippy::too_many_arguments)]
    pub fn of(
        target: &Path,
        backend: &str,
        secs: f64,
        changed: Option<i64>,
        plan: &WritePlan,
        stats: &ExportStats,
        target_changed: bool,
        backups: &Backups,
    ) -> Self {
        let mut rows: Vec<NotWritten> = plan
            .will_not
            .iter()
            .map(|(kind, reason)| NotWritten {
                kind: kind.clone(),
                id: None,
                reason: reason.clone(),
            })
            .collect();
        rows.extend(stats.not_written.iter().map(|n| NotWritten {
            kind: n.table.clone(),
            id: Some(n.id),
            reason: REASON_NO_EXPORTER.into(),
        }));
        let not_written_total = rows.len() as u64;
        rows.truncate(MAX_ROWS);
        Self {
            target: target.display().to_string(),
            backend: backend.to_string(),
            secs,
            bytes: stats.bytes,
            elements: stats.elements,
            proxies: stats.proxies,
            changed,
            by_kind: stats.written.clone(),
            ids_kept: stats.ids_kept,
            ids_new: stats.ids_new,
            ids_conflict: stats.ids_conflict,
            not_written_total,
            not_written: rows,
            warnings: stats.warnings.clone(),
            target_changed,
            replaced_unknown: false,
            prev: opt_str(&backups.prev),
            original: opt_str(&backups.original),
        }
    }

    pub fn file_name(&self) -> String {
        Path::new(&self.target)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// `Saved House-edited.ifc in 1.2 s · 5,214 elements · 12 changed · 3 not written`
    pub fn line(&self, c: &Catalog) -> String {
        let name = self.file_name();
        let secs = format!("{:.1}", self.secs);
        let mut parts = vec![
            c.tf(
                "save.line.saved",
                &[("name", name.as_str()), ("secs", secs.as_str())],
            ),
            c.tf(
                "save.line.elements",
                &[("n", self.elements.to_string().as_str())],
            ),
        ];
        if let Some(n) = self.changed {
            parts.push(c.tf("save.line.changed", &[("n", n.to_string().as_str())]));
        }
        if self.not_written_total > 0 {
            parts.push(c.tf(
                "save.line.not",
                &[("n", self.not_written_total.to_string().as_str())],
            ));
        }
        parts.join(" \u{b7} ")
    }

    pub fn to_json(&self) -> String {
        let mut o = JsonValue::Object(Default::default());
        o["target"] = self.target.as_str().into();
        o["backend"] = self.backend.as_str().into();
        o["secs"] = self.secs.into();
        o["bytes"] = self.bytes.into();
        o["elements"] = self.elements.into();
        o["proxies"] = self.proxies.into();
        if let Some(n) = self.changed {
            o["changed"] = n.into();
        }
        let mut kinds = JsonValue::Object(Default::default());
        for (k, n) in &self.by_kind {
            kinds[k.as_str()] = (*n).into();
        }
        o["by_kind"] = kinds;
        o["ids_kept"] = self.ids_kept.into();
        o["ids_new"] = self.ids_new.into();
        o["ids_conflict"] = self.ids_conflict.into();
        o["not_written_total"] = self.not_written_total.into();
        let rows: Vec<JsonValue> = self
            .not_written
            .iter()
            .map(|r| {
                let mut j = JsonValue::Object(Default::default());
                j["kind"] = r.kind.as_str().into();
                if let Some(id) = r.id {
                    j["id"] = id.into();
                }
                j["reason"] = r.reason.as_str().into();
                j
            })
            .collect();
        o["not_written"] = rows.into();
        let warnings: Vec<JsonValue> = self.warnings.iter().map(|w| w.as_str().into()).collect();
        o["warnings"] = warnings.into();
        o["target_changed"] = self.target_changed.into();
        o["replaced_unknown"] = self.replaced_unknown.into();
        if let Some(p) = &self.prev {
            o["prev"] = p.as_str().into();
        }
        if let Some(p) = &self.original {
            o["original"] = p.as_str().into();
        }
        o.to_string()
    }

    pub fn from_json(s: &str) -> Option<Self> {
        let o: JsonValue = s.parse().ok()?;
        let text = |k: &str| o[k].as_str().unwrap_or_default().to_string();
        let num = |k: &str| o[k].as_u64().unwrap_or(0);
        let path = |k: &str| o[k].as_str().map(str::to_string);
        o.get("target")?;
        Some(Self {
            target: text("target"),
            backend: text("backend"),
            secs: o["secs"].as_f64().unwrap_or(0.0),
            bytes: num("bytes"),
            elements: num("elements"),
            proxies: num("proxies"),
            changed: o["changed"].as_i64(),
            by_kind: o["by_kind"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_u64().unwrap_or(0)))
                        .collect()
                })
                .unwrap_or_default(),
            ids_kept: num("ids_kept"),
            ids_new: num("ids_new"),
            ids_conflict: num("ids_conflict"),
            not_written_total: num("not_written_total"),
            not_written: o["not_written"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|r| NotWritten {
                            kind: r["kind"].as_str().unwrap_or_default().to_string(),
                            id: r["id"].as_i64(),
                            reason: r["reason"].as_str().unwrap_or_default().to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            warnings: o["warnings"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|w| w.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            target_changed: o["target_changed"].as_bool().unwrap_or(false),
            replaced_unknown: o["replaced_unknown"].as_bool().unwrap_or(false),
            prev: path("prev"),
            original: path("original"),
        })
    }
}

/// The result line under the ribbon: one per save, the newest replaces it.
pub struct Strip {
    pub text: String,
    pub warn: bool,
    /// Folder to open, when there is a file to look at.
    pub file: Option<PathBuf>,
    pub until: Instant,
    /// Offers "Use the original from now on".
    pub can_use_original: bool,
    pub has_report: bool,
    /// Offers "Choose location..." after a failed write.
    pub can_relocate: bool,
}

impl Strip {
    pub fn new(text: String, warn: bool, file: Option<PathBuf>) -> Self {
        Self {
            text,
            warn,
            file,
            until: Instant::now() + STRIP_LIFE,
            can_use_original: false,
            has_report: false,
            can_relocate: false,
        }
    }
}

/// Puts a line on the strip and in the notification center's history.
pub fn show(a: &mut EditorApp, strip: Strip) {
    let level = if strip.warn {
        Level::Warning
    } else {
        Level::Success
    };
    a.sh.toasts.record(level, strip.text.clone());
    a.save.strip = Some(strip);
}

pub(crate) fn open_folder(file: &Path) {
    let dir = file.parent().unwrap_or(Path::new("."));
    let tool = if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(tool).arg(dir).spawn();
}

/// The selection for a row of a table, where a click can show it.
fn sel_of(kind: &str, id: i64) -> Option<Sel> {
    Some(match kind {
        "wall" => Sel::Wall(id),
        "opening" => Sel::Opening(id),
        "curtain_wall" => Sel::Curtain(id),
        "slab" => Sel::Slab(id),
        "room" => Sel::Room(id),
        "mep_run" => Sel::Mep(id),
        "comp" => Sel::Comp(id),
        "mass" => Sel::Mass(id),
        _ => return None,
    })
}

pub fn strip_ui(a: &mut EditorApp, ctx: &egui::Context) {
    let Some(st) = &a.save.strip else { return };
    let now = Instant::now();
    if now >= st.until {
        a.save.strip = None;
        return;
    }
    ctx.request_repaint_after(st.until - now);
    let (mut folder, mut report, mut original, mut close) = (false, false, false, false);
    let mut relocate = false;
    let th = &a.sh.theme;
    let c = &a.sh.cat;
    egui::Area::new(egui::Id::new("o5.strip"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, 96.0])
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(th.c("bg.panel"))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let color = if st.warn {
                            th.c("warning")
                        } else {
                            th.c("text.secondary")
                        };
                        ui.label(RichText::new(st.text.as_str()).color(color));
                        if st.file.is_some() {
                            folder = kit::secondary_button(ui, c.t("save.btn.open_folder")).clicked();
                        }
                        if st.can_use_original {
                            original = kit::secondary_button(ui, c.t("save.btn.use_original")).clicked();
                        }
                        if st.can_relocate {
                            relocate = kit::secondary_button(ui, c.t("save.btn.choose_location")).clicked();
                        }
                        if st.has_report {
                            report = kit::secondary_button(ui, c.t("save.btn.report")).clicked();
                        }
                        close = ddc_ui::widgets::close_button(ui, &c.t("save_report.close")).clicked();
                    });
                });
        });
    if folder {
        if let Some(f) = &st.file {
            open_folder(f);
        }
    }
    if report {
        a.save.report_open = true;
    }
    if original {
        super::ask_use_original(a);
    }
    if relocate {
        a.save.strip = None;
        super::save_ifc_as(a);
    }
    if close {
        a.save.strip = None;
    }
}

pub fn window(a: &mut EditorApp, ctx: &egui::Context) {
    if !a.save.report_open {
        return;
    }
    let Some(rep) = a.save.report.clone() else {
        a.save.report_open = false;
        return;
    };
    let mut open = true;
    let mut pick = None;
    let mut drop_kind = None;
    let c = &a.sh.cat;
    let num = |ui: &mut egui::Ui, key: &str, n: u64| {
        ui.label(c.tf(key, &[("n", n.to_string().as_str())]));
    };
    crate::ui_kit::Dialog::new("save.report", c.t("save.report.title"), crate::ui_kit::DialogSize::Tool)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label(rep.line(c));
            ui.separator();
            ui.strong(c.t("save.report.written"));
            for (k, n) in &rep.by_kind {
                ui.label(format!("{k}: {n}"));
            }
            num(ui, "save.report.proxies", rep.proxies);
            ui.separator();
            ui.strong(c.t("save.report.ids"));
            num(ui, "save.report.ids_kept", rep.ids_kept);
            num(ui, "save.report.ids_new", rep.ids_new);
            num(ui, "save.report.ids_conflict", rep.ids_conflict);
            if rep.target_changed {
                ui.separator();
                ui.label(c.t("save.report.target_changed"));
            }
            if rep.replaced_unknown {
                ui.separator();
                ui.label(c.t("save.report.replaced_unknown"));
            }
            if let Some(p) = &rep.prev {
                ui.weak(c.tf("save.report.prev", &[("path", p.as_str())]));
            }
            ui.separator();
            ui.strong(c.tf(
                "save.report.not",
                &[("n", rep.not_written_total.to_string().as_str())],
            ));
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    for r in &rep.not_written {
                        let reason = c.t(&r.reason);
                        let id = r.id.map_or(String::new(), |i| format!(" {i}"));
                        let text = format!("{}{id}: {reason}", r.kind);
                        // the button is placed first, at the right: a long reason wraps in what
                        // it leaves instead of pushing the button out of the row
                        let right = egui::Layout::right_to_left(egui::Align::Min);
                        ui.with_layout(right, |ui| {
                            if r.reason == REASON_LINK
                                && kit::secondary_button(ui, c.t("ifc_patch.drop")).clicked()
                            {
                                drop_kind = r.id.map(|i| (r.kind.clone(), i));
                            }
                            let left = egui::Layout::top_down(egui::Align::Min);
                            ui.with_layout(left, |ui| match r.id.and_then(|i| sel_of(&r.kind, i)) {
                                Some(s) => {
                                    if ui.link(text).clicked() {
                                        pick = Some(s);
                                    }
                                }
                                None => {
                                    ui.label(text);
                                }
                            });
                        });
                    }
                    for w in &rep.warnings {
                        ui.weak(w.as_str());
                    }
                });
        });
    if let Some(s) = pick {
        a.ed.selection = vec![s];
    }
    if let Some((kind, id)) = drop_kind {
        drop_links(a, &kind, id);
    }
    a.save.report_open = open;
}

/// Gives up the pending links of one element: the next save no longer retries them and is not
/// dirty because of them. Other pending links, of the same table too, stay. One undo step; the
/// row leaves the report.
fn drop_links(a: &mut EditorApp, kind: &str, row: i64) {
    // the source the report was written over; else the one the source panel shows
    let target = a.save.report.as_ref().map(|r| PathBuf::from(&r.target));
    let over = a.ed.model.sources().ok().and_then(|v| {
        v.into_iter()
            .find(|s| target.as_deref() == Some(Path::new(&s.path)))
            .map(|s| s.source)
    });
    let Some(key) = over.or_else(|| crate::source_ui::current_source(a).map(|s| s.source)) else {
        return;
    };
    let guid = a.ed.model.pending_links(&key).ok().and_then(|v| {
        v.into_iter()
            .find(|l| l.table == kind && l.id == row)
            .map(|l| l.guid)
    });
    // nothing pending here: no claim that anything was dropped
    let Some(guid) = guid else {
        return;
    };
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = dropped.clone();
    let k = key;
    let op = crate::editor::OpFn(std::sync::Arc::new(move |m: &mut ddc_model::Model| {
        seen.store(
            m.drop_pending_link(&k, &guid, None)?,
            std::sync::atomic::Ordering::Relaxed,
        );
        Ok(())
    }));
    if let Err(e) = a.ed.run_op(op) {
        a.ed.status = Some(e.to_string());
        return;
    }
    if let Some(r) = &mut a.save.report {
        let before = r.not_written.len();
        r.not_written
            .retain(|x| !(x.kind == kind && x.id == Some(row) && x.reason == REASON_LINK));
        r.not_written_total = r
            .not_written_total
            .saturating_sub((before - r.not_written.len()) as u64);
    }
    let n = dropped.load(std::sync::atomic::Ordering::Relaxed).to_string();
    a.ed.status = Some(a.sh.cat.tf("ifc_patch.drop_done", &[("n", n.as_str())]));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_io::ifc::NotExported;

    #[test]
    fn esc_closes_the_report_window() {
        let ed = crate::editor::Editor::new_in_memory().unwrap();
        let mut app = EditorApp::new(ed, None, "test".into());
        app.save.report = Some(sample());
        app.save.report_open = true;
        crate::ui_kit::run_esc_frames(|c| window(&mut app, c));
        assert!(!app.save.report_open);
    }

    fn sample() -> SaveReport {
        let mut written = BTreeMap::new();
        written.insert("wall".to_string(), 3);
        let stats = ExportStats {
            bytes: 1000,
            written,
            elements: 3,
            proxies: 1,
            ids_kept: 2,
            ids_new: 1,
            ids_conflict: 1,
            not_written: vec![NotExported {
                table: "stair".into(),
                id: 4,
            }],
            warnings: vec!["w".into()],
            ..Default::default()
        };
        let plan = WritePlan {
            will_not: vec![("wall".into(), "save.reason.no_exporter".into())],
            ..Default::default()
        };
        let backups = Backups {
            prev: Some(PathBuf::from("b/a.prev")),
            original: None,
        };
        SaveReport::of(
            Path::new("d/a.ifc"),
            "rewrite",
            1.21,
            Some(2),
            &plan,
            &stats,
            true,
            &backups,
        )
    }

    #[test]
    fn json_round_trips() {
        let r = sample();
        assert_eq!(SaveReport::from_json(&r.to_json()), Some(r));
        let none = SaveReport::default();
        assert_eq!(SaveReport::from_json(&none.to_json()), Some(none));
        assert_eq!(SaveReport::from_json("{}"), None);
        assert_eq!(SaveReport::from_json("not json"), None);
    }

    #[test]
    fn what_the_plan_and_the_export_left_out_are_both_listed() {
        let r = sample();
        assert_eq!(r.not_written_total, 2);
        assert_eq!(r.not_written[1].kind, "stair");
        assert_eq!(r.not_written[1].id, Some(4));
    }

    #[test]
    fn the_line_says_what_counts_in_both_languages() {
        for lang in [ddc_ui::Lang::Ru, ddc_ui::Lang::En] {
            let c = Catalog::new(lang);
            let l = sample().line(&c);
            assert!(l.contains("a.ifc") && l.contains("1.2"), "{l}");
            assert!(!l.contains('{'), "{l}");
        }
    }

    #[test]
    fn the_total_stays_exact_past_the_row_limit() {
        let stats = ExportStats {
            not_written: (0..MAX_ROWS as i64 + 5)
                .map(|id| NotExported {
                    table: "stair".into(),
                    id,
                })
                .collect(),
            ..Default::default()
        };
        let r = SaveReport::of(
            Path::new("a.ifc"),
            "rewrite",
            0.0,
            None,
            &WritePlan::default(),
            &stats,
            false,
            &Backups::default(),
        );
        assert_eq!(r.not_written.len(), MAX_ROWS);
        assert_eq!(r.not_written_total, MAX_ROWS as u64 + 5);
    }

    #[test]
    fn every_reason_exists_in_both_languages() {
        for lang in [ddc_ui::Lang::Ru, ddc_ui::Lang::En] {
            let c = Catalog::new(lang);
            assert_ne!(c.t(REASON_NO_EXPORTER), REASON_NO_EXPORTER);
        }
    }
}
