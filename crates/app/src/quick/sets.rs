//! Selection sets: named element lists with a level, stored per project (`ux.json`);
//! recalling one switches to the set's level and drops deleted elements.
use crate::editor::{Editor, Sel};
use ddc_ui::uxstate::SavedSet;

pub const MAX: usize = 20;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Set {
    pub name: String,
    pub level: i64,
    pub items: Vec<Sel>,
}

#[derive(Clone, Debug, Default)]
pub struct Sets {
    pub list: Vec<Set>,
}

/// Selection element -> (kind, id, panel number) for the file.
pub fn sel_key(s: &Sel) -> (String, i64, u32) {
    let (k, id, n) = match *s {
        Sel::Wall(i) => ("wall", i, 0),
        Sel::Opening(i) => ("opening", i, 0),
        Sel::Curtain(i) => ("curtain", i, 0),
        Sel::Panel(i, n) => ("panel", i, n),
        Sel::Slab(i) => ("slab", i, 0),
        Sel::Room(i) => ("room", i, 0),
        Sel::Mass(i) => ("mass", i, 0),
        Sel::Mep(i) => ("mep", i, 0),
        Sel::Comp(i) => ("comp", i, 0),
        Sel::Anno(i) => ("anno", i, 0),
        Sel::Ext(c, i) => ("ext", i, u32::from(c)),
        Sel::Body(k, i) => (k.table(), i, 0),
    };
    (k.to_string(), id, n)
}

pub fn sel_of(k: &str, id: i64, n: u32) -> Option<Sel> {
    Some(match k {
        "wall" => Sel::Wall(id),
        "opening" => Sel::Opening(id),
        "curtain" => Sel::Curtain(id),
        "panel" => Sel::Panel(id, n),
        "slab" => Sel::Slab(id),
        "room" => Sel::Room(id),
        "mass" => Sel::Mass(id),
        "mep" => Sel::Mep(id),
        "comp" => Sel::Comp(id),
        "anno" => Sel::Anno(id),
        "ext" => Sel::Ext(u16::try_from(n).ok()?, id),
        k => Sel::Body(crate::editor::bodies::BodyKind::from_table(k)?, id),
    })
}

impl Sets {
    /// Save the selection of level `level` as "<base> N" (new number); the oldest is dropped after
    /// [`MAX`].
    pub fn save(&mut self, base: &str, level: i64, sel: &[Sel]) -> usize {
        let n = (1..)
            .find(|k| !self.list.iter().any(|s| s.name == format!("{base} {k}")))
            .unwrap_or(1);
        self.list.push(Set {
            name: format!("{base} {n}"),
            level,
            items: sel.to_vec(),
        });
        if self.list.len() > MAX {
            self.list.remove(0);
        }
        self.list.len() - 1
    }
    /// Set elements that still exist in the view (the level is already current).
    pub fn recall(&self, i: usize, ed: &Editor) -> Vec<Sel> {
        let all = crate::selection::all(ed);
        self.list
            .get(i)
            .map(|s| {
                s.items
                    .iter()
                    .copied()
                    .filter(|x| all.contains(x))
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn remove(&mut self, i: usize) {
        if i < self.list.len() {
            self.list.remove(i);
        }
    }
    pub fn to_saved(&self) -> Vec<SavedSet> {
        self.list
            .iter()
            .map(|s| SavedSet {
                name: s.name.clone(),
                level: s.level,
                items: s.items.iter().map(sel_key).collect(),
            })
            .collect()
    }
    pub fn from_saved(v: &[SavedSet]) -> Sets {
        Sets {
            list: v
                .iter()
                .map(|s| Set {
                    name: s.name.clone(),
                    level: s.level,
                    items: s
                        .items
                        .iter()
                        .filter_map(|(k, i, n)| sel_of(k, *i, *n))
                        .collect(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_cap_and_missing_elements() {
        let ed = Editor::new_in_memory().unwrap();
        let mut s = Sets::default();
        assert_eq!(s.save("Set", 1, &[Sel::Wall(1)]), 0);
        assert_eq!(s.save("Set", 1, &[Sel::Wall(2)]), 1);
        assert_eq!(s.list[1].name, "Set 2");
        s.remove(0);
        assert_eq!(s.save("Set", 1, &[]), 1);
        assert_eq!(s.list[1].name, "Set 1");
        // elements 1 and 2 do not exist in the empty model
        assert!(s.recall(0, &ed).is_empty());
        assert!(s.recall(9, &ed).is_empty());
        for _ in 0..MAX + 5 {
            s.save("S", 1, &[]);
        }
        assert_eq!(s.list.len(), MAX);
    }

    #[test]
    fn saved_round_trip_every_kind() {
        let all = [
            Sel::Wall(1),
            Sel::Opening(2),
            Sel::Curtain(3),
            Sel::Panel(3, 7),
            Sel::Slab(4),
            Sel::Room(5),
            Sel::Mass(6),
            Sel::Mep(7),
            Sel::Comp(8),
        ];
        let mut s = Sets::default();
        s.save("A", 42, &all);
        let back = Sets::from_saved(&s.to_saved());
        assert_eq!(back.list, s.list);
        assert_eq!(sel_of("nope", 1, 0), None);
    }
}
