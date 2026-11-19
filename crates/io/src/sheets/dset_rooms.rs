//! Room numbers on a typical floor plan. One plan stands for several floors, and a room's
//! number may change from floor to floor (201, 301, ...). Numbers made of a per-floor prefix
//! and a part every floor shares show only that part (".01") with a note on the sheet that
//! names a real room on the first and the last floor; floors numbered any other way are not
//! merged, so no label on a typical plan is the number of one floor only.

use std::collections::{BTreeMap, HashSet};

use ddc_model::{PlanView, TagCat, ViewPrim};

/// Where a room label stands on a plan, mm, and what it is: 0 a room stamp, 1 a room tag.
pub(super) type Key = (u8, i64, i64);

/// Room numbers of one plan by the place of their labels.
pub(super) type Numbers = BTreeMap<Key, String>;

/// How a typical floor plan shows its room numbers.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Shown {
    /// What the labels whose number changes from floor to floor show instead.
    pub marks: BTreeMap<Key, String>,
    /// One such label: what it shows, its number on the first floor and on the last.
    pub sample: Option<(String, String, String)>,
}

fn key_of(p: &ViewPrim) -> Option<(Key, &str)> {
    let mm = |v: f64| (v * 1000.0).round() as i64;
    match p {
        ViewPrim::RoomTag { pos, number, .. } => Some(((0, mm(pos.0), mm(pos.1)), number.as_str())),
        ViewPrim::Tag { tag } if tag.cat == TagCat::Room => {
            let a = tag.anchor;
            Some(((1, mm(a.0), mm(a.1)), tag.text.mark.as_str()))
        }
        _ => None,
    }
}

/// Room numbers of plan `d`.
pub(super) fn room_numbers(d: &PlanView) -> Numbers {
    d.prims
        .iter()
        .filter_map(key_of)
        .map(|(k, n)| (k, n.trim().to_string()))
        .collect()
}

/// How one plan shows the room numbers of `floors` (in level order), or None when no plan
/// can show them truly: rooms that differ in place, or numbers that do not split into a
/// prefix of each floor and a part every floor shares. Numbers alike on every floor stay.
pub(super) fn typical_rooms(floors: &[&Numbers]) -> Option<Shown> {
    let first = *floors.first()?;
    let last = *floors.last()?;
    if floors.iter().any(|f| !f.keys().eq(first.keys())) {
        return None;
    }
    let varying: Vec<&Key> = first
        .keys()
        .filter(|k| floors.iter().any(|f| f[*k] != first[*k]))
        .collect();
    let Some(&k0) = varying.first() else {
        return Some(Shown {
            marks: BTreeMap::new(),
            sample: None,
        });
    };
    let all: HashSet<&str> = floors
        .iter()
        .flat_map(|f| f.values().map(String::as_str))
        .collect();
    let len = |s: &str| s.chars().count();
    let shortest = varying
        .iter()
        .flat_map(|k| floors.iter().map(move |f| len(&f[*k])))
        .min()?;
    // the longest shared part that leaves every floor a prefix of its own
    for n in (1..shortest).rev() {
        let tail = |s: &str| s.chars().skip(len(s) - n).collect::<String>();
        let head = |s: &str| s.chars().take(len(s) - n).collect::<String>();
        let fits = floors.iter().all(|f| {
            let p = head(&f[k0]);
            varying
                .iter()
                .all(|k| head(&f[*k]) == p && tail(&f[*k]) == tail(&first[*k]))
        });
        let shown = |k: &Key| format!(".{}", tail(&first[k]));
        if fits && varying.iter().all(|k| !all.contains(shown(k).as_str())) {
            return Some(Shown {
                marks: varying.iter().map(|k| (**k, shown(k))).collect(),
                sample: Some((shown(k0), first[k0].clone(), last[k0].clone())),
            });
        }
    }
    None
}

/// Labels of plan `d` show `marks` in place of their numbers.
pub(super) fn show_rooms(d: &mut PlanView, marks: &BTreeMap<Key, String>) {
    for p in &mut d.prims {
        let Some(s) = key_of(p).and_then(|(k, _)| marks.get(&k)).cloned() else {
            continue;
        };
        match p {
            ViewPrim::RoomTag { number, .. } => *number = s,
            ViewPrim::Tag { tag } => tag.text.mark = s,
            _ => {}
        }
    }
}

/// The sheet note that reads a shown number back to every floor's.
pub(super) fn room_note(
    text: &dyn Fn(&str) -> String,
    s: &Shown,
    first_level: &str,
    last_level: &str,
) -> Option<String> {
    let (mark, a, b) = s.sample.as_ref()?;
    Some(
        text("dset.note.room_numbers")
            .replace("{mark}", mark)
            .replace("{first}", a)
            .replace("{last}", b)
            .replace("{level_first}", first_level)
            .replace("{level_last}", last_level),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor(nums: &[&str]) -> Numbers {
        nums.iter()
            .enumerate()
            .map(|(i, n)| ((1, i as i64 * 1000, 0), n.to_string()))
            .collect()
    }

    #[test]
    fn floor_prefixed_numbers_show_their_shared_part() {
        let fs: Vec<Numbers> = (2..=12)
            .map(|f| floor(&[format!("{f}01").as_str(), &format!("{f}02"), "LB"]))
            .collect();
        let refs: Vec<&Numbers> = fs.iter().collect();
        let s = typical_rooms(&refs).unwrap();
        let shown: Vec<&str> = s.marks.values().map(String::as_str).collect();
        assert_eq!(shown, [".01", ".02"]);
        assert_eq!(
            s.sample,
            Some((".01".into(), "201".into(), "1201".into()))
        );
    }

    #[test]
    fn numbers_without_a_floor_prefix_make_no_typical_floor() {
        let a = floor(&["1", "2"]);
        let b = floor(&["3", "4"]);
        assert_eq!(typical_rooms(&[&a, &b]), None);
        let c = floor(&["101", "102"]);
        let d = floor(&["202", "201"]);
        assert_eq!(typical_rooms(&[&c, &d]), None);
        let e = floor(&["101"]);
        assert_eq!(typical_rooms(&[&c, &e]), None);
    }

    #[test]
    fn alike_numbers_stay_as_they_are() {
        let a = floor(&["A", "B"]);
        let s = typical_rooms(&[&a, &a.clone()]).unwrap();
        assert!(s.marks.is_empty() && s.sample.is_none());
    }
}
