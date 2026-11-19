//! Column widths of the schedule grid: drag the header edge to resize, double-click the edge to
//! fit the content, text that does not fit is elided with a tooltip. The decisions are pure
//! functions (clamp, fit, edge hit, scroll to a column) and are tested without a frame.

use super::*;
use std::collections::HashMap;

/// Narrowest a column can be dragged.
pub const MIN_W: f32 = 48.0;
/// Widest a column can be dragged or fitted.
pub const MAX_W: f32 = 640.0;
/// Pointer distance from a column edge that still grabs it.
pub const SLOP: f32 = 4.0;
/// Text inset on each side of a cell.
pub const PAD: f32 = 6.0;
/// Rows measured by "fit to content".
pub const FIT_ROWS: usize = 500;

pub fn clamp_width(w: f32) -> f32 {
    if w.is_finite() {
        w.clamp(MIN_W, MAX_W)
    } else {
        MIN_W
    }
}

/// Width that shows the widest of `text_widths` whole, clamped to `min..=max`.
pub fn autofit(text_widths: &[f32], min: f32, max: f32) -> f32 {
    let widest = text_widths
        .iter()
        .copied()
        .filter(|w| w.is_finite())
        .fold(0.0f32, f32::max);
    (widest + 2.0 * PAD).clamp(min, max.max(min))
}

/// Which column's right edge is under `x` (grid coordinates): index into the visible columns.
/// `xs` are the cumulative edges, `xs[0] == 0`; the nearest edge within `slop` wins.
pub fn hit_edge(x: f32, xs: &[f32], slop: f32) -> Option<usize> {
    (1..xs.len())
        .map(|e| (e - 1, (xs[e] - x).abs()))
        .filter(|&(_, d)| d <= slop)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(k, _)| k)
}

/// Room for text in a column of width `w`.
#[cfg(test)]
pub fn text_room(w: f32) -> f32 {
    (w - 2.0 * PAD).max(1.0)
}

/// True when text of width `text_w` does not fit a column of width `col_w`
/// (the painted decision is the galley's own `elided`; this one pins the rule).
#[cfg(test)]
pub fn is_cut(text_w: f32, col_w: f32) -> bool {
    text_w > text_room(col_w) + 0.5
}

/// Width of every table column: the stored width (by column key) or the built default.
pub fn widths(cols: &[Col], stored: &HashMap<String, f32>) -> Vec<f32> {
    cols.iter()
        .map(|c| clamp_width(stored.get(&c.key).copied().unwrap_or(c.width)))
        .collect()
}

/// Cumulative left edges of the visible columns `vis` (table indices), `xs[0] == 0`.
pub fn edges(ws: &[f32], vis: &[usize]) -> Vec<f32> {
    let mut xs = vec![0.0f32];
    for &j in vis {
        xs.push(xs.last().copied().unwrap_or(0.0) + ws.get(j).copied().unwrap_or(MIN_W));
    }
    xs
}

/// Horizontal scroll offset that brings visible column `k` fully into the viewport
/// `[offset, offset + view_w]`; `None` when it already is. A column wider than the viewport
/// aligns its left edge.
pub fn scroll_x_for(xs: &[f32], k: usize, offset: f32, view_w: f32) -> Option<f32> {
    let (l, r) = (*xs.get(k)?, *xs.get(k + 1)?);
    if l < offset || r - l > view_w {
        Some(l.max(0.0))
    } else if r > offset + view_w {
        Some((r - view_w).max(0.0))
    } else {
        None
    }
}

/// One line of text in a cell (`indent` extra on the left), elided when it does not fit.
/// Returns true when it was cut, so the caller can offer the full text in a tooltip.
pub fn paint_cell(
    ui: &egui::Ui,
    cell: Rect,
    indent: f32,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    right: bool,
) -> bool {
    let l = cell.left() + PAD + indent;
    let inner = Rect::from_x_y_ranges(l..=(cell.right() - PAD).max(l), cell.y_range());
    let al = if right {
        egui::Align::Max
    } else {
        egui::Align::Min
    };
    ddc_ui::widgets::text::paint_in(ui, inner, text, font, color, al)
}

fn drag_id() -> egui::Id {
    egui::Id::new("ddc.table.col_drag")
}

/// Edge grab zone of column `j` whose right edge is at `x`: returns a new width while dragging
/// and `fit` on a double-click.
pub(super) struct EdgeOut {
    pub width: Option<(usize, f32)>,
    pub fit: Option<usize>,
}

pub(super) fn edge_widget(
    ui: &mut egui::Ui,
    hdr: Rect,
    x: f32,
    j: usize,
    width: f32,
    accent: egui::Color32,
    out: &mut EdgeOut,
) {
    let zone = Rect::from_center_size(
        egui::pos2(x, hdr.center().y),
        Vec2::new(SLOP * 2.0, hdr.height()),
    );
    let r = ui.interact(
        zone,
        ui.id().with(("hdr_edge", j)),
        Sense::click_and_drag(),
    );
    if r.hovered() || r.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
        ui.painter()
            .line_segment([zone.center_top(), zone.center_bottom()], (1.5, accent));
    }
    if r.hovered() && !r.dragged() {
        r.clone().on_hover_text(crate::tr("sch.ui.col_edge", &[]));
    }
    if r.double_clicked() {
        out.fit = Some(j);
        return;
    }
    if r.drag_started() {
        ui.ctx().data_mut(|d| d.insert_temp(drag_id(), width));
    }
    if r.dragged() {
        let base: Option<f32> = ui.ctx().data(|d| d.get_temp(drag_id()));
        let (now, origin) = ui.input(|i| (i.pointer.interact_pos(), i.pointer.press_origin()));
        if let (Some(b), Some(n), Some(o)) = (base, now, origin) {
            out.width = Some((j, clamp_width(b + n.x - o.x)));
        }
    }
}

impl View {
    /// Widths of all table columns on the current tab.
    pub(super) fn col_widths(&self) -> Vec<f32> {
        let Some(tb) = self.table.as_ref() else {
            return vec![];
        };
        widths(&tb.cols, &self.tabs[self.cat.index()].widths)
    }

    /// Edges of the visible columns.
    pub(super) fn col_xs(&self) -> Vec<f32> {
        edges(&self.col_widths(), &self.visible_cols())
    }

    pub(super) fn set_col_width(&mut self, j: usize, w: f32) {
        let Some(key) = self
            .table
            .as_ref()
            .and_then(|t| t.cols.get(j))
            .map(|c| c.key.clone())
        else {
            return;
        };
        self.tabs[self.cat.index()]
            .widths
            .insert(key, clamp_width(w));
    }

    /// Width that shows the header and the first `FIT_ROWS` shown values of column `j` whole.
    pub(super) fn fit_width(&self, ui: &egui::Ui, j: usize, u: Units, font: &egui::FontId) -> f32 {
        let Some(tb) = self.table.as_ref() else {
            return MIN_W;
        };
        let measure = |s: String| {
            ui.painter()
                .layout_no_wrap(s, font.clone(), egui::Color32::PLACEHOLDER)
                .size()
                .x
        };
        let c = &tb.cols[j];
        let mut ws = vec![measure(format!("{} ▲", header(c, u)))];
        ws.extend(
            self.ord
                .iter()
                .take(FIT_ROWS)
                .map(|&i| measure(fmt(&tb.rows[i as usize].vals[j], c.kind, u))),
        );
        autofit(&ws, MIN_W, MAX_W)
    }

    /// Keep the active column inside the horizontal viewport (uses the current widths).
    pub(super) fn reveal_col(&mut self, j: usize) {
        let vis = self.visible_cols();
        let Some(k) = vis.iter().position(|&c| c == j) else {
            return;
        };
        let (off, w) = self.hview;
        if let Some(x) = scroll_x_for(&self.col_xs(), k, off, w) {
            self.scroll_x = Some(x);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col_of(key: &str, w: f32) -> Col {
        let mut c = col(key, key, Kind::Text, Edit::No);
        c.width = w;
        c
    }

    #[test]
    fn width_clamps_to_min_and_max() {
        assert_eq!(clamp_width(10.0), MIN_W);
        assert_eq!(clamp_width(-5.0), MIN_W);
        assert_eq!(clamp_width(100.0), 100.0);
        assert_eq!(clamp_width(5000.0), MAX_W);
        assert_eq!(clamp_width(f32::NAN), MIN_W);
    }

    #[test]
    fn autofit_takes_widest_plus_padding_and_clamps() {
        assert_eq!(autofit(&[40.0, 90.0, 70.0], MIN_W, MAX_W), 90.0 + 2.0 * PAD);
        assert_eq!(autofit(&[], MIN_W, MAX_W), MIN_W);
        assert_eq!(autofit(&[5.0], MIN_W, MAX_W), MIN_W);
        assert_eq!(autofit(&[9000.0], MIN_W, MAX_W), MAX_W);
        assert_eq!(autofit(&[f32::NAN, 60.0], MIN_W, MAX_W), 60.0 + 2.0 * PAD);
    }

    #[test]
    fn edge_hit_uses_slop_and_picks_nearest() {
        let xs = [0.0, 100.0, 180.0, 300.0];
        assert_eq!(hit_edge(100.0, &xs, SLOP), Some(0));
        assert_eq!(hit_edge(104.0, &xs, SLOP), Some(0));
        assert_eq!(hit_edge(96.0, &xs, SLOP), Some(0));
        assert_eq!(hit_edge(104.5, &xs, SLOP), None);
        assert_eq!(hit_edge(50.0, &xs, SLOP), None);
        assert_eq!(hit_edge(300.0, &xs, SLOP), Some(2));
        // the left border of the first column is not an edge
        assert_eq!(hit_edge(0.0, &xs, SLOP), None);
        // two edges inside the slop: the nearer one
        let tight = [0.0, 10.0, 16.0];
        assert_eq!(hit_edge(14.0, &tight, SLOP), Some(1));
        assert_eq!(hit_edge(5.0, &[0.0], SLOP), None);
    }

    #[test]
    fn hidden_columns_have_no_edge() {
        let cols = [col_of("a", 100.0), col_of("b", 80.0), col_of("c", 60.0)];
        let ws = widths(&cols, &HashMap::new());
        let vis = [0usize, 2];
        let xs = edges(&ws, &vis);
        assert_eq!(xs, vec![0.0, 100.0, 160.0]);
        // the edge at x=180 (end of hidden "b" when shown) grabs nothing
        assert_eq!(hit_edge(180.0, &xs, SLOP), None);
        let k = hit_edge(160.0, &xs, SLOP).unwrap();
        assert_eq!(vis[k], 2);
    }

    #[test]
    fn cut_decision_follows_room() {
        assert_eq!(text_room(100.0), 88.0);
        assert!(!is_cut(88.0, 100.0));
        assert!(is_cut(90.0, 100.0));
        assert!(!is_cut(0.0, 10.0));
        assert!(text_room(0.0) >= 1.0);
    }

    #[test]
    fn stored_widths_override_defaults_by_key_and_clamp() {
        let cols = [col_of("a", 100.0), col_of("b", 80.0)];
        let mut st = HashMap::new();
        st.insert("b".to_string(), 250.0);
        st.insert("zzz".to_string(), 10.0);
        assert_eq!(widths(&cols, &st), vec![100.0, 250.0]);
        st.insert("a".to_string(), 3.0);
        assert_eq!(widths(&cols, &st), vec![MIN_W, 250.0]);
        // other column order: the width follows the key
        let swapped = [cols[1].clone(), cols[0].clone()];
        assert_eq!(widths(&swapped, &st), vec![250.0, MIN_W]);
    }

    #[test]
    fn scroll_x_reveals_column_with_custom_widths() {
        let xs = edges(&[100.0, 300.0, 100.0], &[0, 1, 2]);
        assert_eq!(xs, vec![0.0, 100.0, 400.0, 500.0]);
        // viewport 0..250: column 1 (100..400) is wider than the view -> left edge aligns
        assert_eq!(scroll_x_for(&xs, 1, 0.0, 250.0), Some(100.0));
        // viewport wide enough: align the right edge
        assert_eq!(scroll_x_for(&xs, 1, 0.0, 450.0), None);
        assert_eq!(scroll_x_for(&xs, 2, 0.0, 450.0), Some(50.0));
        // already inside
        assert_eq!(scroll_x_for(&xs, 0, 0.0, 250.0), None);
        // scrolled past it to the left
        assert_eq!(scroll_x_for(&xs, 0, 120.0, 250.0), Some(0.0));
        assert_eq!(scroll_x_for(&xs, 9, 0.0, 250.0), None);
    }

    #[test]
    fn view_widths_persist_per_tab_and_drive_the_cursor_scroll() {
        let (_e, mut v, jp, jl) = super::super::tests::sheet();
        let k0 = v.col_xs();
        // two visible columns, defaults
        assert_eq!(k0.len(), 3);
        let (a, b) = (jp.min(jl), jp.max(jl));
        v.set_col_width(a, 300.0);
        v.set_col_width(b, 2.0);
        let xs = v.col_xs();
        assert_eq!(xs, vec![0.0, 300.0, 300.0 + MIN_W]);
        // survives a table rebuild of the same tab
        let tb = v.table.clone().unwrap();
        v.set_table(tb, Units::Metric);
        assert_eq!(v.col_xs(), xs);
        // a viewport of 200 px: revealing the second column scrolls to its right edge
        v.hview = (0.0, 200.0);
        v.reveal_col(b);
        assert_eq!(v.scroll_x, Some(300.0 + MIN_W - 200.0));
        // other tab keeps its own widths
        let other = (v.cat.index() + 1) % Cat::ALL.len();
        assert!(v.tabs[other].widths.is_empty());
    }
}
