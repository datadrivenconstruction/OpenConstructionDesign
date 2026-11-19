//! Frame counters over the 3D view (command `view.perf_stats`): the renderer publishes them in
//! `ddc_render::perf`; the overlay only displays them.

use std::time::Duration;

use ddc_render::perf::{published, FrameStats};

use super::index_job::IndexJob;
use crate::viewport::Viewport;

#[derive(Default)]
pub struct PerfUi {
    pub shown: bool,
    pub index: IndexJob,
    last_vp: Option<[[f32; 4]; 4]>,
}

impl PerfUi {
    /// The camera changed since the previous frame.
    pub fn camera_jumped(&mut self, vp: [[f32; 4]; 4]) -> bool {
        let jumped = self.last_vp.is_some_and(|p| p != vp);
        self.last_vp = Some(vp);
        jumped
    }
}

/// What the frame shows incompletely and why: string key and number (`{v}`). Goes to the status bar:
/// the overlay is hidden by default, yet an incomplete picture must not go unreported. Important items come first.
pub fn status_notes(
    s: &FrameStats,
    index_pending: bool,
    loading: bool,
) -> Vec<(&'static str, usize)> {
    let mut v = vec![];
    let (dropped, denied) = s.held_back();
    if denied > 0 {
        v.push(("v3d.status.held_memory", denied));
    }
    if dropped > 0 {
        v.push(("v3d.status.held_budget", dropped));
    }
    // the device buffer limit is a hard cap: the frame stays up with fewer boxes, said aloud
    if s.impostors_dropped > 0 {
        v.push(("v3d.status.gpu_limit", s.impostors_dropped));
    }
    if loading {
        v.push(("v3d.status.loading", 0));
    }
    if s.coarse > 0 && !s.moving {
        v.push(("v3d.status.coarse", s.coarse));
    }
    if index_pending {
        v.push(("v3d.status.index", 0));
    }
    if s.pending > 0 && !s.moving {
        v.push(("v3d.status.refining", s.pending));
    }
    v
}

/// Status-bar lines (translated).
pub fn status_lines(app: &crate::shell::EditorApp, cat: &ddc_ui::i18n::Catalog) -> Vec<String> {
    let notes = status_notes(
        &published(),
        crate::spatial::index_pending(app.view.index.as_deref(), &app.scene),
        app.view.e3.cache.inst.loading(),
    );
    notes
        .into_iter()
        .map(|(k, n)| cat.tf(k, &[("v", &n.to_string())]))
        .collect()
}

const MB: f64 = 1024.0 * 1024.0;
const M: f64 = 1.0e6;

/// Table rows: label key and value. Units are part of the labels (translated by the catalog).
pub fn rows(s: &FrameStats, dt_s: f32) -> Vec<(&'static str, String)> {
    let ms = dt_s * 1000.0;
    let fps = if dt_s > 0.0 { 1.0 / dt_s } else { 0.0 };
    vec![
        ("v3d.perf.frame", format!("{ms:.1} / {fps:.0}")),
        ("v3d.perf.cpu", format!("{:.1}", s.cpu_ms)),
        ("v3d.perf.draws", s.draws.to_string()),
        (
            "v3d.perf.tris",
            format!(
                "{:.2} / {:.1}",
                s.triangles as f64 / M,
                s.total_triangles as f64 / M
            ),
        ),
        ("v3d.perf.chunks", format!("{} / {}", s.drawn, s.chunks)),
        ("v3d.perf.culled", s.culled.to_string()),
        ("v3d.perf.occluded", s.occluded.to_string()),
        ("v3d.perf.small", s.lod_skipped.to_string()),
        ("v3d.perf.boxes", s.boxes.to_string()),
        ("v3d.perf.dropped", s.dropped.to_string()),
        (
            "v3d.perf.gpu",
            format!(
                "{:.0} / {:.0}",
                s.gpu_bytes as f64 / MB,
                s.gpu_budget as f64 / MB
            ),
        ),
        (
            "v3d.perf.upload",
            format!("{:.1}", s.uploaded_bytes as f64 / MB),
        ),
        ("v3d.perf.queue", s.pending.to_string()),
        ("v3d.perf.evicted", format!("{} / {}", s.evicted, s.denied)),
    ]
}

pub fn state_key(s: &FrameStats) -> &'static str {
    if s.moving {
        "v3d.perf.moving"
    } else if s.refining {
        "v3d.perf.refining"
    } else if s.held_back() != (0, 0) || s.coarse > 0 || s.impostors_dropped > 0 {
        "v3d.perf.partial"
    } else {
        "v3d.perf.still"
    }
}

/// Draws the table in the view's bottom-left corner; its rectangle is used for the "cursor over overlay" check.
pub fn show(v: &Viewport, ui: &mut egui::Ui, rect: egui::Rect) -> Option<egui::Rect> {
    if !v.nav.perf.shown {
        return None;
    }
    let ctx = ui.ctx().clone();
    let stats = published();
    let table = rows(&stats, ctx.input(|i| i.stable_dt));
    let id = ui.id().with("v3d_perf");
    let r = egui::Area::new(id)
        .order(ddc_ui::z::Z::Canvas.order())
        .pivot(egui::Align2::LEFT_BOTTOM)
        .fixed_pos(rect.left_bottom() + egui::vec2(8.0, -8.0))
        .show(&ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.strong(v.nav.t("v3d.perf.title"));
                egui::Grid::new(id.with("grid"))
                    .num_columns(2)
                    .spacing([12.0, 1.0])
                    .show(ui, |ui| {
                        for (k, value) in &table {
                            ui.label(v.nav.t(k));
                            ui.monospace(value);
                            ui.end_row();
                        }
                        ui.label(v.nav.t("v3d.perf.state"));
                        ui.label(v.nav.t(state_key(&stats)));
                        ui.end_row();
                    });
            });
        });
    ctx.request_repaint_after(Duration::from_millis(250));
    Some(r.response.rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_key_has_a_label_key() {
        let s = FrameStats::default();
        for (k, _) in rows(&s, 0.016) {
            assert!(super::super::LABEL_KEYS.contains(&k), "{k}");
        }
        for k in [
            "v3d.perf.title",
            "v3d.perf.state",
            "v3d.perf.moving",
            "v3d.perf.refining",
            "v3d.perf.partial",
            "v3d.perf.still",
        ] {
            assert!(super::super::LABEL_KEYS.contains(&k), "{k}");
        }
    }

    #[test]
    fn values_are_in_the_units_of_the_labels() {
        let s = FrameStats {
            triangles: 2_500_000,
            total_triangles: 100_000_000,
            gpu_bytes: 512 << 20,
            gpu_budget: 1024 << 20,
            ..FrameStats::default()
        };
        let t = rows(&s, 0.02);
        let get = |k: &str| t.iter().find(|r| r.0 == k).unwrap().1.clone();
        assert_eq!(get("v3d.perf.frame"), "20.0 / 50");
        assert_eq!(get("v3d.perf.tris"), "2.50 / 100.0");
        assert_eq!(get("v3d.perf.gpu"), "512 / 1024");
    }

    #[test]
    fn gpu_buffer_limit_is_named_and_frame_is_partial() {
        let s = FrameStats {
            impostors_dropped: 7,
            ..FrameStats::default()
        };
        assert_eq!(status_notes(&s, false, false), [("v3d.status.gpu_limit", 7)]);
        assert_eq!(state_key(&s), "v3d.perf.partial");
    }

    #[test]
    fn a_complete_frame_at_rest_has_no_notes() {
        assert!(status_notes(&FrameStats::default(), false, false).is_empty());
    }

    #[test]
    fn everything_held_back_at_rest_is_named_in_the_status_bar() {
        let s = FrameStats {
            dropped: 3,
            denied: 2,
            coarse: 4,
            ..FrameStats::default()
        };
        let keys = status_notes(&s, true, true);
        let want = [
            ("v3d.status.held_memory", 2),
            ("v3d.status.held_budget", 3),
            ("v3d.status.loading", 0),
            ("v3d.status.coarse", 4),
            ("v3d.status.index", 0),
        ];
        assert_eq!(keys, want);
        assert_eq!(
            state_key(&s),
            "v3d.perf.partial",
            "not reported as complete"
        );
    }

    #[test]
    fn a_moving_camera_is_not_a_held_back_model() {
        let s = FrameStats {
            moving: true,
            dropped: 3,
            denied: 2,
            coarse: 4,
            pending: 5,
            ..FrameStats::default()
        };
        assert!(status_notes(&s, false, false).is_empty());
        // while an unfinished refinement at rest is visible
        let s = FrameStats {
            refining: true,
            pending: 5,
            ..FrameStats::default()
        };
        assert_eq!(status_notes(&s, false, false), [("v3d.status.refining", 5)]);
    }

    #[test]
    fn status_notes_have_both_translations() {
        let s = FrameStats {
            dropped: 1,
            denied: 1,
            coarse: 1,
            pending: 1,
            ..FrameStats::default()
        };
        let mut keys = status_notes(&s, true, true);
        keys.push(("v3d.status.refining", 1));
        for file in [
            include_str!("../../../ui/data/i18n/ru/perf_overlay.json"),
            include_str!("../../../ui/data/i18n/en/perf_overlay.json"),
        ] {
            for (k, _) in &keys {
                assert!(file.contains(&format!("\"{k}\"")), "{k}");
            }
            assert!(file.contains("\"v3d.perf.partial\""));
        }
    }

    #[test]
    fn only_a_changed_camera_is_a_jump() {
        let mut p = PerfUi::default();
        let (a, mut b) = ([[1.0; 4]; 4], [[1.0; 4]; 4]);
        b[3][0] = 2.0;
        assert!(!p.camera_jumped(a), "first frame");
        assert!(!p.camera_jumped(a));
        assert!(p.camera_jumped(b));
        assert!(!p.camera_jumped(b));
    }

    #[test]
    fn motion_wins_over_refinement_in_the_state() {
        let mut s = FrameStats {
            refining: true,
            ..FrameStats::default()
        };
        assert_eq!(state_key(&s), "v3d.perf.refining");
        s.moving = true;
        assert_eq!(state_key(&s), "v3d.perf.moving");
        assert_eq!(state_key(&FrameStats::default()), "v3d.perf.still");
    }
}
