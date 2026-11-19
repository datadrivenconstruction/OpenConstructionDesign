//! Steel connection texts: type and part names, the analysis status and the bolt, weld and
//! plate schedules as titled tables of text, in an explicit language.

use crate::schedule::tl;
use ddc_geom::steel::forces::Verdict;
use ddc_geom::steel::schedule::Schedule;
use ddc_geom::steel::{ConnKind, Role};

pub fn type_name(ru: bool, k: ConnKind) -> String {
    let key = k.key();
    tl(ru, &format!("steel.type.{key}"))
}

pub fn role_name(ru: bool, r: Role) -> String {
    let key = r.key();
    tl(ru, &format!("steel.role.{key}"))
}

/// "Not checked by analysis": connections carry no strength calculation.
pub fn status(ru: bool, v: &Verdict) -> String {
    tl(ru, &format!("steel.status.{}", v.key()))
}

/// One schedule table: title, column heads, rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub title: String,
    pub head: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

fn mm(x: f64) -> String {
    format!("{:.0}", x * 1000.0)
}

/// Bolts, welds, plates and (when present) balls and sleeves; empty tables are left out.
pub fn tables(ru: bool, s: &Schedule) -> Vec<Table> {
    let h = |keys: &[&str]| keys.iter().map(|k| tl(ru, k)).collect::<Vec<_>>();
    let mut out = vec![];
    let mut push = |title: &str, head: Vec<String>, rows: Vec<Vec<String>>| {
        if !rows.is_empty() {
            out.push(Table {
                title: tl(ru, title),
                head,
                rows,
            });
        }
    };
    let bolts = s
        .bolts
        .iter()
        .map(|b| {
            let role = role_name(ru, if b.anchor { Role::Anchor } else { Role::Bolt });
            vec![
                role,
                b.size.clone(),
                b.class.clone(),
                mm(b.length),
                b.count.to_string(),
            ]
        })
        .collect();
    push(
        "steel.sched.bolts",
        h(&[
            "steel.col.part",
            "steel.col.size",
            "steel.col.class",
            "steel.col.length_mm",
            "steel.col.count",
        ]),
        bolts,
    );
    let welds = s
        .welds
        .iter()
        .map(|w| {
            vec![
                mm(w.leg),
                w.runs.to_string(),
                format!("{:.2}", w.length),
                format!("{:.2}", w.mass),
            ]
        })
        .collect();
    push(
        "steel.sched.welds",
        h(&[
            "steel.col.leg_mm",
            "steel.col.runs",
            "steel.col.length_m",
            "steel.col.mass_kg",
        ]),
        welds,
    );
    let plates = s
        .plates
        .iter()
        .map(|p| {
            vec![
                role_name(ru, p.role),
                p.mark.clone(),
                mm(p.t),
                p.count.to_string(),
                format!("{:.3}", p.area),
                format!("{:.1}", p.mass),
            ]
        })
        .collect();
    push(
        "steel.sched.plates",
        h(&[
            "steel.col.part",
            "steel.col.mark",
            "steel.col.thickness_mm",
            "steel.col.count",
            "steel.col.area_m2",
            "steel.col.mass_kg",
        ]),
        plates,
    );
    let other = s
        .other
        .iter()
        .map(|(r, mark, n, kg)| {
            vec![
                role_name(ru, *r),
                mark.clone(),
                n.to_string(),
                format!("{kg:.1}"),
            ]
        })
        .collect();
    push(
        "steel.sched.other",
        h(&[
            "steel.col.part",
            "steel.col.mark",
            "steel.col.count",
            "steel.col.mass_kg",
        ]),
        other,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::math::v3;
    use ddc_geom::member::{Just, Member, Path, Shape};
    use ddc_geom::steel::{build, forces, schedule, Params};

    fn column() -> Member {
        Member {
            shape: Shape::I {
                h: 0.2,
                b: 0.2,
                tw: 0.009,
                tf: 0.015,
            },
            path: Path::Line {
                a: v3(0.0, 0.0, 0.0),
                b: v3(0.0, 0.0, 3.0),
            },
            rotation: 0.0,
            yjust: Just::Center,
            zjust: Just::Center,
            column: true,
            start: None,
            end: None,
        }
    }

    fn is_ru(s: &str) -> bool {
        s.chars().any(|c| ('\u{0400}'..='\u{04ff}').contains(&c))
    }

    #[test]
    fn every_type_and_part_has_a_name_in_both_languages() {
        for k in ConnKind::ALL {
            let (en, ru) = (type_name(false, k), type_name(true, k));
            assert!(!en.contains("steel.") && !en.is_empty(), "{en}");
            assert!(is_ru(&ru), "{ru}");
        }
        for r in [
            Role::Plate,
            Role::Stiffener,
            Role::Anchor,
            Role::Bolt,
            Role::Ball,
        ] {
            assert!(is_ru(&role_name(true, r)) && !role_name(false, r).contains("steel."));
        }
    }

    #[test]
    fn status_says_not_checked_until_forces_exist() {
        assert_eq!(
            status(false, &Verdict::NotChecked),
            "Not checked by analysis"
        );
        assert!(is_ru(&status(true, &Verdict::NotChecked)));
    }

    #[test]
    fn schedule_tables_from_a_base_plate() {
        let c = build(ConnKind::BasePlate, &[column()], &Params::default()).unwrap();
        assert_eq!(forces::verdict(&c), Verdict::NotChecked);
        let s = schedule::schedule([&c]);
        for ru in [false, true] {
            let t = tables(ru, &s);
            assert!(t.len() >= 3, "{t:?}");
            for tab in &t {
                assert!(
                    tab.rows.iter().all(|r| r.len() == tab.head.len()),
                    "{}",
                    tab.title
                );
                assert!(!tab.title.contains("steel.") && !tab.head.iter().any(|x| x.is_empty()));
            }
            assert_eq!(is_ru(&t[0].title), ru);
        }
    }
}
