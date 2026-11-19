//! Connected runs that follow an edit: ghost lines while a grip is dragged (nothing is written
//! until release) and the status line after the edit when the network has new problems.

use ddc_model::{CompSpec, Error, FollowGhost, MepIssue, Model, RunSpec};
use ddc_render::glam::Vec3;

/// Ghost polylines in scene coordinates (height relative to `z0`).
pub fn lines(g: Vec<FollowGhost>, z0: f64) -> Vec<Vec<Vec3>> {
    g.into_iter()
        .map(|g| {
            g.points
                .iter()
                .map(|q| Vec3::new(q.0 as f32, q.1 as f32, (q.2 - z0) as f32))
                .collect()
        })
        .collect()
}

/// Runs that would follow run `id` taking the path of `s`; the error is the refusal.
pub fn run_ghosts(m: &Model, id: i64, s: &RunSpec, z0: f64) -> Result<Vec<Vec<Vec3>>, Error> {
    Ok(lines(m.mep_follow_ghosts_run(id, s)?, z0))
}

/// Runs that would follow device `id` placed as `s`; the error is the refusal.
pub fn device_ghosts(m: &Model, id: i64, s: &CompSpec, z0: f64) -> Result<Vec<Vec<Vec3>>, Error> {
    Ok(lines(m.mep_follow_ghosts_device(id, s)?, z0))
}

/// Status text for the problems a follow introduced; None when there are none.
pub fn status_text(p: &[(i64, MepIssue)]) -> Option<String> {
    let (_, first) = p.first()?;
    let args = first.args();
    let refs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let first_text = crate::tr(first.key(), &refs);
    Some(crate::tr(
        "mep.follow.problems",
        &[("n", &p.len().to_string()), ("first", &first_text)],
    ))
}

/// After an edit: show new network problems of a follow, if any.
pub fn report(status: &mut Option<String>) {
    if let Some(t) = status_text(&ddc_model::take_follow_problems()) {
        *status = Some(t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_problems_no_text_and_the_first_one_is_named() {
        assert_eq!(status_text(&[]), None);
        let t = status_text(&[(1, MepIssue::OpenEnd { run: 7, end: 0 }), (1, MepIssue::NoFlow)])
            .unwrap();
        assert!(t.contains('2') && t.contains('7'), "{t}");
    }

    #[test]
    fn ghost_lines_are_lifted_by_the_view_height() {
        let g = FollowGhost {
            run: 1,
            points: vec![(1.0, 2.0, 5.0), (3.0, 2.0, 5.0)],
        };
        let l = lines(vec![g], 2.0);
        assert_eq!(l[0][0], Vec3::new(1.0, 2.0, 3.0));
    }
}
