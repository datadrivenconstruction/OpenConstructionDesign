//! One Delete for every selectable element: the whole selection goes through
//! `Model::delete_items` as one model step and one undo step. A panel is a sub-element: it never
//! stands for its curtain wall, so it stays and the user is told why.

use super::{Cmd, Editor, Sel};
use crate::clip_tools::sel_item;

/// (table, id) as `Model::delete_items` takes it.
pub type Item = (String, i64);

/// Model step kind: the "wall" prefix keeps derived data in step, as for Cut.
const KIND: &str = "wall_delete_items";

/// What gets deleted, and what a press refuses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extra {
    pub items: Vec<Item>,
    pub panels: Vec<Sel>,
    /// Selected, but no table takes the element.
    pub refused: usize,
}

impl Extra {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.panels.is_empty() && self.refused == 0
    }

    /// Takes out what the view hides. Returns how many, without those already in `counted`.
    pub(crate) fn drop_hidden(
        &mut self,
        hidden: impl Fn(&str, i64) -> bool,
        counted: &[Sel],
    ) -> usize {
        let counted: Vec<Item> = counted.iter().filter_map(|s| sel_item(*s)).collect();
        let mut n = 0;
        self.items.retain(|i| {
            let drop = hidden(&i.0, i.1);
            n += usize::from(drop && !counted.contains(i));
            !drop
        });
        self.panels.retain(|p| {
            let item = sel_item(*p);
            let drop = item.as_ref().is_some_and(|i| hidden(&i.0, i.1));
            n += usize::from(drop && !item.is_some_and(|i| counted.contains(&i)));
            !drop
        });
        n
    }
}

/// Adds the editor selection to `x`, without repeats. A panel whose wall goes too is not refused.
fn plan(sel: &[Sel], mut x: Extra) -> Extra {
    for &s in sel {
        match s {
            Sel::Panel(..) => x.panels.push(s),
            _ => match sel_item(s) {
                Some(i) => x.items.push(i),
                None => x.refused += 1,
            },
        }
    }
    let mut seen = std::collections::HashSet::new();
    x.items.retain(|i| seen.insert(i.clone()));
    let mut panels: Vec<Sel> = vec![];
    for p in x.panels.drain(..) {
        let Sel::Panel(wall, _) = p else { continue };
        let goes = x.items.iter().any(|(t, i)| t == "curtain_wall" && *i == wall);
        if !goes && !panels.contains(&p) {
            panels.push(p);
        }
    }
    x.panels = panels;
    x
}

fn refs(items: &[Item]) -> Vec<(&str, i64)> {
    items.iter().map(|x| (x.0.as_str(), x.1)).collect()
}

/// Redo re-creates elements under new ids: pending deletes follow them.
pub(super) fn remap(items: &mut [Item], table: &str, map: impl Fn(&mut i64)) {
    for x in items.iter_mut().filter(|x| x.0 == table) {
        map(&mut x.1);
    }
}

impl Editor {
    pub fn delete_selection(&mut self) {
        if crate::elem::fitting::refuse_delete(self) {
            return;
        }
        // picked terrain points first: a selected terrain must not go with them
        if crate::topo::delete(self)
            || crate::elem::delete_selection(self)
            || crate::elem::stairs::delete_selected(self)
        {
            return;
        }
        self.delete_with(Extra::default());
    }

    /// Deletes the selection and `extra` (the 3D selection) in one undo step; true when they went.
    /// The step is `Cmd::Items`, not `run_op`: its redo has to follow re-created ids.
    pub(crate) fn delete_with(&mut self, extra: Extra) -> bool {
        self.delete_checked(extra, false)
    }

    /// `confirmed`: the user already saw the dependents, so no dialog comes up.
    pub(super) fn delete_checked(&mut self, mut extra: Extra, confirmed: bool) -> bool {
        let asked = extra.clone();
        if crate::topo::delete(self) {
            return false;
        }
        let visible = crate::annot_text::deletable(self);
        self.selection
            .retain(|s| !matches!(s, Sel::Anno(id) if !visible.contains(id)));
        let solid = if self.selection.is_empty() {
            crate::solid_tools::selected()
        } else {
            None
        };
        if let Some(id) = solid {
            extra.items.push(("solid".to_string(), id));
        }
        let Extra {
            items,
            panels,
            refused,
        } = plan(&self.selection, extra);
        if items.is_empty() {
            if !panels.is_empty() {
                crate::elem::panel_rule::offer(&self.selection);
                self.say(crate::status::Severity::Refusal, crate::tr("elem.panel_refused", &[]));
            } else if refused > 0 {
                self.say(crate::status::Severity::Refusal, crate::tr("elem.not_deletable", &[]));
            }
            return false;
        }
        let deps = super::delete_deps::collect_exact(&self.model, &items, self.level);
        if !confirmed && deps.needs_confirm() {
            self.pending_delete = Some(super::delete_deps::Pending {
                extra: asked,
                deps,
                sel: self.selection.clone(),
                revision: self.revision,
                via_elem: false,
            });
            return false;
        }
        let head = self.model.change_head();
        if let Err(e) = self.model.delete_items(KIND, &refs(&items)) {
            self.status = Some(crate::tr("elem.not_deleted", &[("e", &e.to_string())]));
            return false;
        }
        // nothing written: no undo step, it would undo an earlier edit
        if self.model.change_head() == head {
            self.status = Some(crate::tr("elem.not_deletable", &[]));
            return false;
        }
        let n = items.len();
        // the step is in the model journal: the undo entry goes first, a failed reload keeps it
        self.push(Cmd::Items(items));
        self.selection.retain(|s| match s {
            Sel::Panel(..) => panels.contains(s),
            _ => sel_item(*s).is_none(),
        });
        if solid.is_some() {
            crate::solid_tools::select(None);
        }
        let reloaded = if crate::mep3d::only_mep(&self.model, head) {
            self.revision += 1;
            // fittings follow their runs
            self.struct_reload();
            Ok(())
        } else {
            self.reload()
        };
        let kept = panels.len() + refused;
        crate::elem::panel_rule::offer(&self.selection);
        let also = deps.also();
        let sev = if reloaded.is_err() {
            crate::status::Severity::Error
        } else if deps.other_level > 0 || deps.rooms > 0 {
            crate::status::Severity::Warning
        } else {
            crate::status::Severity::Info
        };
        let text = match reloaded {
            Err(e) => crate::tr("elem.reload_failed", &[("e", &e.to_string())]),
            Ok(()) if kept > 0 => {
                let t = crate::tr(
                    "elem.deleted_kept",
                    &[("n", &n.to_string()), ("k", &kept.to_string())],
                );
                format!("{t}{also}")
            }
            Ok(()) => crate::tr("elem.deleted", &[("n", &n.to_string())]) + &also,
        };
        self.say(sev, text);
        if sev != crate::status::Severity::Error {
            self.offer_undo();
        }
        true
    }

    /// Ok once the delete is in the model journal; a failed reload comes back as `Some`, so the
    /// step stays on the undo stack, in step with the journal.
    pub(super) fn redo_items(
        &mut self,
        items: &[Item],
    ) -> Result<Option<ddc_model::Error>, ddc_model::Error> {
        let head = self.model.change_head();
        self.model.delete_items(KIND, &refs(items))?;
        if self.model.change_head() == head {
            return Err(ddc_model::Error::Invalid("nothing left to delete".into()));
        }
        Ok(self.reload().err())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::bodies::BodyKind;
    use super::*;
    use crate::modify_tools::tests::walls;
    use ddc_geom::v2;
    use ddc_model::{LayerSpec, OpeningSpec, RoomSpec, SlabSpec, ZoneSpec};

    const TABLES: [&str; 10] = [
        "wall",
        "opening",
        "curtain_wall",
        "curtain_panel",
        "slab",
        "room",
        "zone",
        "zone_room",
        "col",
        "mep_run",
    ];

    /// Every row of a table as text.
    pub fn dump(e: &Editor, t: &str) -> Vec<String> {
        let db = e.model.sql();
        let cols: Vec<String> = db
            .prepare(&format!("SELECT name FROM pragma_table_info('{t}')"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|x| x.unwrap())
            .collect();
        let all = cols
            .iter()
            .map(|c| format!("quote(\"{c}\")"))
            .collect::<Vec<_>>()
            .join("||'|'||");
        let mut st = db
            .prepare(&format!("SELECT {all} FROM {t} ORDER BY rowid"))
            .unwrap();
        let rows = st.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.map(|x| x.unwrap()).collect()
    }

    fn all(e: &Editor) -> Vec<Vec<String>> {
        TABLES.iter().map(|t| dump(e, t)).collect()
    }

    fn facade() -> (Editor, i64) {
        let mut e = Editor::new_in_memory().unwrap();
        e.add_curtains(&[(v2(0.0, 20.0), v2(6.0, 20.0), 0.0)]);
        let id = e.curtains[0].id;
        (e, id)
    }

    fn told_why(e: &Editor) -> bool {
        e.status.as_deref().is_some_and(|s| !s.starts_with("elem."))
    }

    fn door(along: f64) -> OpeningSpec {
        OpeningSpec {
            along,
            width: 0.9,
            sill: 0.0,
            height: 2.1,
            kind: "door".into(),
        }
    }

    #[test]
    fn panel_alone_refuses_and_the_wall_stays() {
        let (mut e, cw) = facade();
        let before = all(&e);
        let head = e.model.change_head();
        let steps = e.undo_len();
        e.selection = vec![Sel::Panel(cw, 2)];
        e.delete_selection();
        assert_eq!(all(&e), before);
        assert_eq!(e.model.change_head(), head);
        assert_eq!(e.undo_len(), steps);
        assert_eq!(e.selection, vec![Sel::Panel(cw, 2)]);
        assert!(told_why(&e), "{:?}", e.status);
    }

    #[test]
    fn panel_mixed_with_a_wall_keeps_its_curtain_wall() {
        let (mut e, cw) = facade();
        walls(&mut e, &[((0.0, 0.0), (4.0, 0.0))]);
        let wall = e.walls[0].id;
        let before = all(&e);
        let curtain = dump(&e, "curtain_wall");
        let steps = e.undo_len();
        e.selection = vec![Sel::Panel(cw, 2), Sel::Wall(wall)];
        e.delete_selection();
        assert!(e.walls.is_empty());
        assert_eq!(dump(&e, "curtain_wall"), curtain);
        assert_eq!(e.undo_len(), steps + 1);
        assert_eq!(e.selection, vec![Sel::Panel(cw, 2)]);
        assert!(told_why(&e), "{:?}", e.status);
        e.undo();
        assert_eq!(all(&e), before);
    }

    /// The route that used to take the wall with a run in the selection.
    #[test]
    fn panel_mixed_with_a_run_keeps_its_curtain_wall() {
        let (mut e, cw) = facade();
        let l = e.level;
        e.model
            .sql()
            .execute_batch(&format!(
                "INSERT INTO mep_system(name,kind,color) VALUES ('S','cold_water',255);
                 INSERT INTO mep_run(system,level,domain,shape,w,h,wall,insulation,lining,slope,
                     points,cap0,cap1)
                     VALUES (1,{l},'pipe','round',0.05,0.05,0,0,0,0,'20 2 2.5;24 2 2.5',0,0);"
            ))
            .unwrap();
        e.reload().unwrap();
        let run: i64 = e
            .model
            .sql()
            .query_row("SELECT id FROM mep_run", [], |r| r.get(0))
            .unwrap();
        let curtain = dump(&e, "curtain_wall");
        e.selection = vec![Sel::Panel(cw, 2), Sel::Mep(run)];
        e.delete_selection();
        assert!(e.model.mep_run(run).is_err());
        assert_eq!(dump(&e, "curtain_wall"), curtain);
    }

    #[test]
    fn panel_with_its_own_wall_goes_with_the_wall() {
        let (mut e, cw) = facade();
        let before = all(&e);
        e.selection = vec![Sel::Panel(cw, 2), Sel::Curtain(cw)];
        e.delete_selection();
        assert!(e.curtains.is_empty());
        assert!(e.selection.is_empty());
        e.undo();
        assert_eq!(all(&e), before);
    }

    /// The 3D selection takes the same route.
    #[test]
    fn panel_picked_in_3d_keeps_its_curtain_wall() {
        let (mut e, cw) = facade();
        walls(&mut e, &[((0.0, 0.0), (4.0, 0.0))]);
        let wall = e.walls[0].id;
        // adding the facade selected it; only the 3D selection is under test
        e.selection.clear();
        let curtain = dump(&e, "curtain_wall");
        let extra = Extra {
            items: vec![("wall".to_string(), wall)],
            panels: vec![Sel::Panel(cw, 1)],
            refused: 0,
        };
        assert!(e.delete_with(extra));
        assert!(e.walls.is_empty());
        assert_eq!(dump(&e, "curtain_wall"), curtain);
    }

    #[test]
    fn mixed_selection_goes_in_one_press_and_one_undo_step() {
        let mut e = Editor::new_in_memory().unwrap();
        e.add_curtains(&[
            (v2(0.0, 20.0), v2(6.0, 20.0), 0.0),
            (v2(0.0, 25.0), v2(6.0, 25.0), 0.0),
        ]);
        let (c1, c2) = (e.curtains[0].id, e.curtains[1].id);
        walls(
            &mut e,
            &[((0.0, 0.0), (8.0, 0.0)), ((0.0, 5.0), (8.0, 5.0))],
        );
        let (host, other) = (e.walls[0].id, e.walls[1].id);
        let o1 = e.model.add_opening(host, &door(1.0)).unwrap();
        let o2 = e.model.add_opening(host, &door(4.0)).unwrap();
        let st = e
            .model
            .add_slab_type(
                "S",
                &[LayerSpec {
                    material: "c".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let l = e.level;
        let slab = e
            .model
            .add_slab(&SlabSpec {
                level: l,
                slab_type: st,
                offset: 3.0,
                outer: vec![(0.0, 40.0), (6.0, 40.0), (6.0, 44.0), (0.0, 44.0)],
                holes: vec![],
            })
            .unwrap();
        let room = e
            .model
            .add_room(&RoomSpec {
                level: l,
                name: "R".into(),
                number: "1".into(),
                point: (2.0, 2.0),
                height: 3.0,
            })
            .unwrap();
        let zone = e
            .model
            .add_zone(&ZoneSpec {
                name: "Z".into(),
                rooms: vec![room],
            })
            .unwrap();
        e.model
            .sql()
            .execute_batch(&format!(
                "INSERT INTO member_type(name,shape,material) VALUES ('R','rect 0.3 0.5','x');
                 INSERT INTO col(level,mtype,structural,x,y,base_offset,top_offset,rotation)
                     VALUES ({l},1,1,10.0,1.0,0.0,3.0,0.0);
                 INSERT INTO mep_system(name,kind,color) VALUES ('S','cold_water',255);
                 INSERT INTO mep_run(system,level,domain,shape,w,h,wall,insulation,lining,slope,
                     points,cap0,cap1)
                     VALUES (1,{l},'pipe','round',0.05,0.05,0,0,0,0,'20 2 2.5;24 2 2.5',0,0);"
            ))
            .unwrap();
        e.reload().unwrap();
        let one = |e: &Editor, t: &str| -> i64 {
            e.model
                .sql()
                .query_row(&format!("SELECT id FROM {t}"), [], |r| r.get(0))
                .unwrap()
        };
        let (col, run) = (one(&e, "col"), one(&e, "mep_run"));
        let before = all(&e);
        let steps = e.undo_len();
        e.selection = vec![
            Sel::Wall(other),
            Sel::Opening(o1),
            Sel::Opening(o2),
            Sel::Curtain(c1),
            Sel::Curtain(c2),
            Sel::Slab(slab),
            Sel::Room(room),
            super::super::structx::sel("zone", zone).unwrap(),
            Sel::Body(BodyKind::Column, col),
            Sel::Mep(run),
        ];
        e.delete_selection();
        assert_eq!(e.undo_len(), steps + 1);
        assert!(e.selection.is_empty());
        assert_eq!(dump(&e, "wall").len(), 1, "the unselected host stays");
        for t in TABLES.iter().filter(|t| **t != "wall") {
            assert!(dump(&e, t).is_empty(), "{t} still has rows");
        }
        e.undo();
        assert_eq!(all(&e), before);
        e.redo();
        assert_eq!(dump(&e, "wall").len(), 1);
        assert!(dump(&e, "opening").is_empty());
        e.undo();
        assert_eq!(all(&e), before);
    }

    #[test]
    fn wall_with_its_own_opening_goes_once() {
        let mut e = Editor::new_in_memory().unwrap();
        walls(&mut e, &[((0.0, 0.0), (8.0, 0.0))]);
        let wall = e.walls[0].id;
        let o = e.model.add_opening(wall, &door(1.0)).unwrap();
        e.reload().unwrap();
        let before = all(&e);
        e.selection = vec![Sel::Opening(o), Sel::Wall(wall)];
        e.delete_selection();
        assert!(dump(&e, "wall").is_empty());
        assert!(dump(&e, "opening").is_empty());
        e.undo();
        assert_eq!(all(&e), before);
    }

    #[test]
    fn redo_follows_a_recreated_curtain_wall() {
        let (mut e, cw) = facade();
        e.selection = vec![Sel::Curtain(cw)];
        e.delete_selection();
        e.undo();
        e.undo();
        assert!(e.curtains.is_empty());
        e.redo();
        assert_eq!(e.curtains.len(), 1);
        e.redo();
        assert!(e.curtains.is_empty(), "{:?}", e.status);
    }

    #[test]
    fn redo_with_a_failed_reload_stays_on_the_undo_stack() {
        let (mut e, cw) = facade();
        e.selection = vec![Sel::Curtain(cw)];
        e.delete_selection();
        e.undo();
        let (u, r) = (e.undo_len(), e.redo_len());
        // reload reads this table, deleting a curtain wall does not
        e.model
            .sql()
            .execute_batch("DROP TABLE wall_type_layer")
            .unwrap();
        e.redo();
        assert!(dump(&e, "curtain_wall").is_empty(), "{:?}", e.status);
        assert_eq!((e.undo_len(), e.redo_len()), (u + 1, r - 1));
    }

    #[test]
    fn drop_hidden_takes_out_hidden_and_counts_each_once() {
        let mut x = Extra {
            items: vec![
                ("wall".to_string(), 1),
                ("wall".to_string(), 2),
                ("roof".to_string(), 3),
            ],
            panels: vec![Sel::Panel(4, 0), Sel::Panel(5, 0)],
            refused: 1,
        };
        let hidden = |t: &str, id: i64| (t == "wall" && id == 1) || t == "roof" || id == 4;
        // wall 1 is already counted with the editor selection
        assert_eq!(x.drop_hidden(hidden, &[Sel::Wall(1)]), 2);
        assert_eq!(x.items, vec![("wall".to_string(), 2)]);
        assert_eq!(x.panels, vec![Sel::Panel(5, 0)]);
        assert_eq!(x.refused, 1);
    }

    #[test]
    fn stale_selection_writes_no_undo_step() {
        let mut e = Editor::new_in_memory().unwrap();
        walls(&mut e, &[((0.0, 0.0), (4.0, 0.0))]);
        let steps = e.undo_len();
        let head = e.model.change_head();
        e.selection = vec![Sel::Wall(99_999)];
        e.delete_selection();
        assert_eq!(e.undo_len(), steps);
        assert_eq!(e.model.change_head(), head);
        assert_eq!(e.walls.len(), 1);
    }

    #[test]
    fn empty_selection_does_nothing() {
        let mut e = Editor::new_in_memory().unwrap();
        walls(&mut e, &[((0.0, 0.0), (4.0, 0.0))]);
        let steps = e.undo_len();
        e.delete_selection();
        assert_eq!(e.undo_len(), steps);
        assert_eq!(e.walls.len(), 1);
    }

    #[test]
    fn plan_drops_repeats_and_panels_of_a_wall_that_goes() {
        let x = plan(
            &[
                Sel::Panel(1, 0),
                Sel::Panel(2, 0),
                Sel::Panel(2, 0),
                Sel::Curtain(1),
                Sel::Curtain(1),
            ],
            Extra {
                items: vec![("curtain_wall".to_string(), 1)],
                ..Extra::default()
            },
        );
        assert_eq!(x.items, vec![("curtain_wall".to_string(), 1)]);
        assert_eq!(x.panels, vec![Sel::Panel(2, 0)]);
    }

    #[test]
    fn strings_exist_in_both_languages() {
        use ddc_ui::i18n::{Catalog, Lang};
        let c = Catalog::new(Lang::En);
        for k in [
            "elem.deleted",
            "elem.deleted_kept",
            "elem.not_deleted",
            "elem.not_deletable",
            "elem.panel",
            "elem.panel_refused",
            "elem.reload_failed",
        ] {
            assert_ne!(c.t_in(Lang::Ru, k), k);
            let en = c.t_in(Lang::En, k);
            assert_ne!(en, k);
            assert!(!en.chars().any(|ch| ('\u{400}'..='\u{4ff}').contains(&ch)));
        }
    }
}
