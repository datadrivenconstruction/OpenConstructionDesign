//! One-click fix: a single journal entry and a single undo step.

use super::Fix;
use crate::{Edit, ElKey, Error, Model};

fn delete_keys(m: &mut Model, keys: &[ElKey]) -> Result<(), Error> {
    // openings first: deleting a wall takes them with it
    let mut edits = vec![];
    for (_, id) in keys.iter().filter(|k| k.0 == "opening") {
        edits.push(Edit::Delete { table: "opening", id: *id });
    }
    for (table, id) in keys.iter().filter(|k| k.0 != "opening") {
        let id = *id;
        match table.as_str() {
            "wall" => edits.push(Edit::Delete { table: "wall", id }),
            "room" => edits.push(Edit::DeleteRoom(id)),
            "col" => edits.push(Edit::DeleteColumn(id)),
            "beam" => edits.push(Edit::DeleteBeam(id)),
            "slab" => edits.push(Edit::Delete { table: "slab", id }),
            "curtain_wall" => edits.push(Edit::DeleteCurtain(id)),
            "ceiling" => m.delete_ceiling(id)?,
            "comp" => m.delete_component(id)?,
            "roof" => m.delete_roof(id)?,
            "stair" => m.delete_stair(id)?,
            "railing" => m.delete_railing(id)?,
            "mep_run" => m.delete_mep_run(id)?,
            "shaft" => m.delete_shaft(id)?,
            other => return Err(Error::Invalid(format!("cannot delete from {other}"))),
        }
    }
    if edits.is_empty() {
        return Ok(());
    }
    m.apply_edits(&edits)
}

impl Model {
    /// Applies a finding's fix; undoing it restores the model exactly to its previous state.
    pub fn apply_check_fix(&mut self, fix: &Fix) -> Result<(), Error> {
        self.atomic("wall_check_fix", |m| match fix {
            Fix::Edits(e) if !e.is_empty() => {
                // findings were computed before this edit: a neighbouring one may already have created the same parameter
                let mut edits = e.clone();
                let names: Vec<String> = m.params()?.into_iter().map(|d| d.spec.name).collect();
                edits.retain(|x| !matches!(x, Edit::NewParam(s) if names.contains(&s.name)));
                m.apply_edits(&edits)
            }
            Fix::Clash(c) => m.apply_clash_fix(c),
            Fix::Delete(keys) if !keys.is_empty() => delete_keys(m, keys),
            Fix::Edits(_) | Fix::Delete(_) | Fix::None => {
                Err(Error::Invalid("this finding has no automatic fix".into()))
            }
        })
    }
}
