//! Railing (RL): path sketch (Enter) or "Pick host" — click a stair.

use super::{Active, Rec};
use crate::editor::Editor;
use ddc_geom::V2;
use ddc_model::{RailingHost, RailingSpec};

pub fn rec(a: &Active, pts: &[V2]) -> Result<Rec, String> {
    let path = pts.iter().map(|p| (p.x, p.y, 0.0)).collect();
    let mut s = RailingSpec::new(a.level, RailingHost::Path(path));
    s.height = a.opts.height;
    Ok(Rec::new("railing", move |m| {
        m.add_railing(&s)?;
        Ok(1)
    }))
}

pub fn pick(ed: &Editor, a: &Active, p: V2) -> Result<Rec, String> {
    let stair = ed
        .tool
        .layer
        .stair_at(p)
        .ok_or_else(|| a.t("tool.err.no_host"))?;
    let mut tpl = RailingSpec::new(a.level, RailingHost::Stair { stair, side: 0 });
    tpl.height = a.opts.height;
    Ok(Rec::new("railing", move |m| {
        m.add_stair_railings(stair, &tpl)?;
        Ok(1)
    }))
}
