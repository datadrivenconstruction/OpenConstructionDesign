// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The glulam post-and-beam frame of a timber project in the model: one beam between each
//! pair of neighbouring columns, its type found or made by section name. Kept as it is while
//! nothing it depends on changed, like the data centre; an edited beam is left alone.

use super::*;
use ddc_gen::project::timber::GTimber;
use ddc_model::MemberShape;

/// The slots the frame asks for.
/// `cut`: the storey slabs are cut into panels (not when something else cuts them, the site
/// split); only then do the panels past the first ask for slots, so the old ones are deleted.
pub(super) fn asked(t: &GTimber, cut: bool, out: &mut BTreeSet<u32>) {
    out.extend((0..t.beams.len() as u32).map(|i| slots::TIMBER + i));
    if !cut {
        return;
    }
    let first = t.first_panels();
    out.extend((0..t.panels.len()).filter(|i| !first.contains(i)).map(|i| slots::TIMBER_PANELS + i as u32));
}

/// The storey's slab as its panels: the slab slot takes the first, the others their own slots.
/// A storey whose slab is already cut by something else stays whole.
pub(super) fn cut_slab(t: &GTimber, storey: usize, g: &mut Vec<Want>) {
    let mine: Vec<usize> = (0..t.panels.len()).filter(|i| t.panels[*i].storey == storey).collect();
    let (Some(&head), true) = (mine.first(), g.len() == 1 && g[0].table == "slab") else { return };
    let NewElem::Slab { level, stype, offset, .. } = g[0].elem.clone() else { return };
    let panel = |i: usize| NewElem::Slab {
        level,
        stype: stype.clone(),
        offset,
        outer: t.panels[i].outer.clone(),
        holes: t.panels[i].holes.clone(),
    };
    g[0].elem = panel(head);
    for &i in &mine[1..] {
        g.push(Want { rel: slots::TIMBER_PANELS + i as u32, table: "slab", elem: panel(i), role: None, wall_rel: None });
    }
}

/// The member type of a section, made when the model lacks it.
fn beam_type(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    known: &mut BTreeMap<String, i64>,
    t: &GTimber,
    section: (f64, f64),
) -> Result<i64, PlanError> {
    let name = t.type_name(section);
    if let Some(id) = known.get(&name) {
        return Ok(*id);
    }
    let found: Result<i64, rusqlite::Error> =
        cx.ed.model.sql().query_row("SELECT id FROM member_type WHERE name=?1", [&name], |r| r.get(0));
    let id = match found {
        Ok(id) => id,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            // the beam material gets its row (colour, density) when the model has none
            if !known.contains_key("\0material") && cx.ed.model.material(&t.material)?.id == 0 {
                known.insert("\0material".into(), 0);
                let spec = ddc_model::material_defaults(&t.material);
                push(cx, out, ModelStep::Edits(vec![Edit::Material { name: t.material.clone(), spec }]))?;
            }
            let elem = NewResolved::MemberType {
                name: name.clone(),
                shape: MemberShape::Rect { b: section.0, h: section.1 },
                material: t.material.clone(),
            };
            new_type(cx, out, "member_type", elem)?
        }
        Err(e) => return Err(e.into()),
    };
    known.insert(name, id);
    Ok(id)
}

/// The frame must fit its slot blocks: beams and panels.
fn check_size(t: &GTimber) -> Result<(), PlanError> {
    if t.beams.len() > slots::TIMBER_MAX as usize {
        return Err("the timber frame is larger than its slot block".into());
    }
    if t.panels.len() > slots::TIMBER_PANELS_MAX as usize {
        return Err("the timber floor has more panels than its slot block".into());
    }
    Ok(())
}

pub(super) fn sync(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    sy: &mut Sync,
    t: &GTimber,
    levels: &[i64],
) -> Result<(), PlanError> {
    check_size(t)?;
    let mut known = BTreeMap::new();
    let mut wants = vec![];
    for (i, b) in t.beams.iter().enumerate() {
        let level = levels.get(b.storey).copied().ok_or_else(|| PlanError(format!("the frame names storey {}", b.storey)))?;
        let id = beam_type(cx, out, &mut known, t, b.section)?;
        wants.push(Want {
            rel: slots::TIMBER + i as u32,
            table: "beam",
            elem: NewElem::Beam {
                level,
                mtype: Some(type_ref("member_type", id)),
                a: (b.a.0, b.a.1, b.z),
                b: (b.b.0, b.b.1, b.z),
            },
            role: None,
            wall_rel: None,
        });
    }
    sync_group(cx, out, sy, wants)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_gen::project::timber::TPanel;

    fn panel(storey: usize) -> TPanel {
        TPanel { storey, rect: [0.0, 0.0, 1.0, 1.0], outer: vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)], holes: vec![] }
    }

    #[test]
    fn more_panels_than_the_slot_block_are_refused() {
        let mut t = GTimber::default();
        t.panels = (0..slots::TIMBER_PANELS_MAX as usize).map(|_| panel(0)).collect();
        assert!(check_size(&t).is_ok());
        t.panels.push(panel(0));
        assert!(check_size(&t).unwrap_err().0.contains("panels"));
    }

    #[test]
    fn a_slab_cut_by_something_else_asks_no_panel_slots() {
        let t = GTimber { panels: vec![panel(0), panel(0), panel(1), panel(1)], ..GTimber::default() };
        let (mut cut, mut whole) = (BTreeSet::new(), BTreeSet::new());
        asked(&t, true, &mut cut);
        asked(&t, false, &mut whole);
        assert_eq!(cut, [slots::TIMBER_PANELS + 1, slots::TIMBER_PANELS + 3].into());
        assert!(whole.is_empty(), "{whole:?}");
    }
}
