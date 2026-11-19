// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Phase dates for calendar display: `prop(kind='phase', ref=id, key='dates')` =
//! `{"start":"YYYY-MM-DD","end":"YYYY-MM-DD"}`. The schema is unchanged; a write is a single
//! reversible operation. Dates are parsed here (ISO, Gregorian calendar), with no new dependencies.

use rusqlite::types::Value;

use crate::phases::ElemPhase;
use crate::{Error, Model, PhaseIndex};

pub const DATES_KEY: &str = "dates";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub y: i32,
    pub m: u8,
    pub d: u8,
}

fn leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn month_days(y: i32, m: u8) -> u8 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if leap(y) => 29,
        _ => 28,
    }
}

impl Date {
    pub fn new(y: i32, m: u8, d: u8) -> Option<Date> {
        ((1..=12).contains(&m) && (1..=9999).contains(&y) && d >= 1 && d <= month_days(y, m))
            .then_some(Date { y, m, d })
    }

    /// Strictly `YYYY-MM-DD`.
    pub fn parse(s: &str) -> Option<Date> {
        let b = s.as_bytes();
        if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let num = |r: std::ops::Range<usize>| -> Option<u32> {
            s.get(r)
                .filter(|t| t.bytes().all(|c| c.is_ascii_digit()))?
                .parse()
                .ok()
        };
        Date::new(num(0..4)? as i32, num(5..7)? as u8, num(8..10)? as u8)
    }

    /// Days since 1970-01-01 (civil_from_days, Hinnant's public-domain algorithm).
    pub fn days(self) -> i64 {
        let y = i64::from(self.y) - i64::from(self.m <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (i64::from(self.m) + 9) % 12;
        let doy = (153 * mp + 2) / 5 + i64::from(self.d) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    pub fn from_days(z: i64) -> Date {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
        let y = (yoe + era * 400) as i32 + i32::from(m <= 2);
        Date { y, m, d }
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.y, self.m, self.d)
    }
}

/// Value of the `dates` property -> (start, end); wrong shape or non-calendar dates give `None`.
pub fn parse_dates(text: &str) -> Option<(Date, Date)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let (a, b) = (Date::parse(v.get("start")?.as_str()?)?, Date::parse(v.get("end")?.as_str()?)?);
    (a <= b).then_some((a, b))
}

impl Model {
    /// Phase dates; `None` if there is no record or it is corrupt.
    pub fn phase_dates(&self, id: i64) -> Result<Option<(Date, Date)>, Error> {
        Ok(self
            .props("phase", id)?
            .into_iter()
            .find(|(k, _, _)| k == DATES_KEY)
            .and_then(|(_, v, _)| match v {
                Value::Text(t) => parse_dates(&t),
                _ => None,
            }))
    }

    /// Sets (or clears with `None`) the phase dates in one journal operation; an end before the
    /// start is rejected.
    pub fn set_phase_dates(&mut self, id: i64, dates: Option<(Date, Date)>) -> Result<(), Error> {
        if !self.phases()?.iter().any(|p| p.id == id) {
            return Err(Error::Invalid(format!("no phase {id}")));
        }
        let value = match dates {
            Some((a, b)) if a > b => {
                return Err(Error::Invalid("phase end is before its start".into()));
            }
            Some((a, b)) => Some((
                Value::Text(format!("{{\"start\":\"{a}\",\"end\":\"{b}\"}}")),
                "",
            )),
            None => None,
        };
        self.set_prop("phase", id, DATES_KEY, value)
    }
}

impl PhaseIndex {
    /// Phases of all elements: (kind, number, phases), ordered by kind and number.
    pub fn elements(&self) -> Vec<(&'static str, i64, ElemPhase)> {
        let mut v: Vec<_> = self
            .rows()
            .flat_map(|(k, m)| m.iter().map(move |(id, e)| (k, *id, *e)))
            .collect();
        v.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u8, dd: u8) -> Date {
        Date::new(y, m, dd).unwrap()
    }

    #[test]
    fn dates_parse_strictly_and_count_days() {
        assert_eq!(Date::parse("2025-03-09"), Some(d(2025, 3, 9)));
        for bad in ["2025-02-30", "2025-13-01", "2025-3-9", "25-03-09", "2025/03/09", "", "2025-03-09x", "2025-00-10"] {
            assert_eq!(Date::parse(bad), None, "{bad}");
        }
        assert!(Date::parse("2024-02-29").is_some());
        assert!(Date::parse("2100-02-29").is_none());
        assert_eq!(d(1970, 1, 1).days(), 0);
        assert_eq!(d(2000, 3, 1).days() - d(2000, 2, 28).days(), 2);
        for z in [-800_000, -1, 0, 1, 11_016, 20_000, 2_000_000] {
            let x = Date::from_days(z);
            assert_eq!(x.days(), z, "{x}");
            assert_eq!(Date::parse(&x.to_string()), Some(x));
        }
        assert!(d(2025, 1, 31) < d(2025, 2, 1));
    }

    #[test]
    fn phase_dates_roundtrip_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let ph = m.phases().unwrap();
        let id = ph[1].id;
        assert_eq!(m.phase_dates(id).unwrap(), None);
        let span = (d(2025, 3, 1), d(2025, 4, 15));
        m.set_phase_dates(id, Some(span)).unwrap();
        assert_eq!(m.phase_dates(id).unwrap(), Some(span));
        // another phase is untouched
        assert_eq!(m.phase_dates(ph[0].id).unwrap(), None);
        let span2 = (d(2025, 5, 1), d(2025, 5, 1));
        m.set_phase_dates(id, Some(span2)).unwrap();
        assert_eq!(m.phase_dates(id).unwrap(), Some(span2));
        // one operation per write: undo restores the previous dates, then their absence
        m.undo().unwrap();
        assert_eq!(m.phase_dates(id).unwrap(), Some(span));
        m.undo().unwrap();
        assert_eq!(m.phase_dates(id).unwrap(), None);
        // an end before the start and a foreign phase are rejected, nothing is written
        assert!(m.set_phase_dates(id, Some((span.1, span.0))).is_err());
        assert!(m.set_phase_dates(9999, Some(span)).is_err());
        assert_eq!(m.phase_dates(id).unwrap(), None);
        // clear
        m.set_phase_dates(id, Some(span)).unwrap();
        m.set_phase_dates(id, None).unwrap();
        assert_eq!(m.phase_dates(id).unwrap(), None);
    }

    #[test]
    fn spoiled_value_reads_as_none() {
        assert_eq!(parse_dates(r#"{"start":"2025-01-02","end":"2025-01-01"}"#), None);
        assert_eq!(parse_dates(r#"{"start":"2025-01-02"}"#), None);
        assert_eq!(parse_dates("nope"), None);
        assert_eq!(
            parse_dates(r#"{"start":"2025-01-02","end":"2025-01-09"}"#),
            Some((d(2025, 1, 2), d(2025, 1, 9)))
        );
    }
}
