//! Project information: name, number, address, client, organization, author, checker, approver,
//! stage, status, issue date. Stored as properties of `project_info` row 0 (no schema change):
//! the one store title blocks read, by these keys. The site (north, origin, latitude, longitude)
//! is `site_info` (topo).

use rusqlite::types::Value;
use rusqlite::Connection;

use crate::ops::Inv;
use crate::{Error, Model, SiteProps};

const KIND: &str = "project_info";
const MAX_LEN: usize = 500;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectInfo {
    pub name: String,
    pub number: String,
    pub address: String,
    pub client: String,
    pub organization: String,
    pub author: String,
    pub checker: String,
    pub approver: String,
    pub stage: String,
    pub status: String,
    pub issue_date: String,
}

impl ProjectInfo {
    /// (key, value) in display order; keys are the property names title blocks look up.
    pub fn fields(&self) -> [(&'static str, &str); 11] {
        [
            ("name", &self.name),
            ("number", &self.number),
            ("address", &self.address),
            ("client", &self.client),
            ("organization", &self.organization),
            ("author", &self.author),
            ("checker", &self.checker),
            ("approver", &self.approver),
            ("stage", &self.stage),
            ("status", &self.status),
            ("issue_date", &self.issue_date),
        ]
    }

    /// Mutable field by key (window editing, loading).
    pub fn slot(&mut self, key: &str) -> Option<&mut String> {
        Some(match key {
            "name" => &mut self.name,
            "number" => &mut self.number,
            "address" => &mut self.address,
            "client" => &mut self.client,
            "organization" => &mut self.organization,
            "author" => &mut self.author,
            "checker" => &mut self.checker,
            "approver" => &mut self.approver,
            "stage" => &mut self.stage,
            "status" => &mut self.status,
            "issue_date" => &mut self.issue_date,
            _ => return None,
        })
    }

    /// The same fields without surrounding spaces: the form they are stored in.
    pub fn trimmed(&self) -> ProjectInfo {
        let mut p = self.clone();
        for k in self.fields().map(|f| f.0) {
            if let Some(s) = p.slot(k) {
                *s = s.trim().to_string();
            }
        }
        p
    }

    fn check(&self) -> Result<(), Error> {
        for (k, v) in self.fields() {
            if v.chars().count() > MAX_LEN || v.chars().any(|c| c.is_control() && c != '\n') {
                return Err(Error::Invalid(format!(
                    "project {k}: too long or has control characters"
                )));
            }
        }
        Ok(())
    }
}

/// Writes the fields inside an operation (empty removes the property); returns the inverse.
fn write_fields(db: &Connection, info: &ProjectInfo) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    for (k, v) in info.fields() {
        inv = crate::imported::prop_restore(db, KIND, 0, k)?.then(inv);
        db.execute(
            "DELETE FROM prop WHERE kind=?1 AND ref=0 AND key=?2",
            rusqlite::params![KIND, k],
        )?;
        if !v.is_empty() {
            db.execute(
                "INSERT INTO prop(kind,ref,key,value,unit) VALUES (?1,0,?2,?3,'')",
                rusqlite::params![KIND, k, v],
            )?;
        }
    }
    Ok(inv)
}

/// Project info as stored, read through an open connection (inside a batch).
pub fn read_info(db: &Connection) -> Result<ProjectInfo, Error> {
    let mut p = ProjectInfo::default();
    let mut st = db.prepare("SELECT key, value FROM prop WHERE kind=?1 AND ref=0")?;
    let rows = st.query_map([KIND], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Value>(1)?)))?;
    for row in rows {
        let (k, v) = row?;
        if let (Some(slot), Value::Text(t)) = (p.slot(&k), v) {
            *slot = t;
        }
    }
    Ok(p)
}

/// Writes `info` inside a running batch: the previous values come back with the batch's undo.
/// `false` — nothing differs.
pub fn write_in_batch(
    db: &Connection,
    snaps: &mut crate::Snapshots,
    info: &ProjectInfo,
) -> Result<bool, Error> {
    let info = info.trimmed();
    info.check()?;
    if info == read_info(db)? {
        return Ok(false);
    }
    snaps.push(write_fields(db, &info)?);
    Ok(true)
}

impl Model {
    pub fn project_info(&self) -> Result<ProjectInfo, Error> {
        let mut p = ProjectInfo::default();
        for (key, v, _) in self.props(KIND, 0)? {
            if let (Some(slot), Value::Text(t)) = (p.slot(&key), v) {
                *slot = t;
            }
        }
        Ok(p)
    }

    /// Saves the fields (trimmed; empty removes the property) in one undo step.
    /// `false` — nothing differs, no journal entry.
    pub fn set_project_info(&mut self, info: &ProjectInfo) -> Result<bool, Error> {
        let info = info.trimmed();
        info.check()?;
        if info == self.project_info()? {
            return Ok(false);
        }
        self.op("project_info", |db| Ok((true, 0, write_fields(db, &info)?)))
    }

    /// Fields and site in one operation: one undo step, nothing written on a refusal.
    /// `false` — nothing differs, no journal entry.
    pub fn set_project_data(
        &mut self,
        info: &ProjectInfo,
        site: &SiteProps,
    ) -> Result<bool, Error> {
        let info = info.trimmed();
        info.check()?;
        crate::topo::check_site(site)?;
        let (new_info, new_site) = (info != self.project_info()?, *site != self.site()?);
        if !new_info && !new_site {
            return Ok(false);
        }
        self.op("project_info", |db| {
            let mut inv = Inv::default();
            if new_info {
                inv = write_fields(db, &info)?;
            }
            if new_site {
                inv = inv.then(crate::topo::write_site(db, site)?);
            }
            Ok((true, 0, inv))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> ProjectInfo {
        ProjectInfo {
            name: "Haus 7".into(),
            address: "Hauptstr. 1".into(),
            author: "A. B.".into(),
            ..Default::default()
        }
    }

    fn ops(m: &Model) -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get::<_, i64>(0))
            .unwrap()
    }

    #[test]
    fn saves_trims_and_undoes() {
        let mut m = Model::open(":memory:").unwrap();
        assert_eq!(m.project_info().unwrap(), ProjectInfo::default());
        let mut i = info();
        i.name = "  Haus 7  ".into();
        assert!(m.set_project_info(&i).unwrap());
        assert_eq!(m.project_info().unwrap().name, "Haus 7");
        // same data again: no journal entry
        let n = ops(&m);
        assert!(!m.set_project_info(&info()).unwrap());
        assert_eq!(ops(&m), n);
        // change one field, clear another
        let mut j = info();
        j.name = "Haus 8".into();
        j.address.clear();
        assert!(m.set_project_info(&j).unwrap());
        assert_eq!(m.project_info().unwrap(), j);
        m.undo().unwrap();
        assert_eq!(m.project_info().unwrap(), info());
        m.undo().unwrap();
        assert_eq!(m.project_info().unwrap(), ProjectInfo::default());
    }

    #[test]
    fn rejects_control_characters_and_long_text() {
        let mut m = Model::open(":memory:").unwrap();
        let mut i = info();
        i.name = "a\u{7}b".into();
        assert!(m.set_project_info(&i).is_err());
        i.name = "x".repeat(MAX_LEN + 1);
        assert!(m.set_project_info(&i).is_err());
        assert_eq!(m.project_info().unwrap(), ProjectInfo::default());
    }

    #[test]
    fn project_data_is_one_step_and_refuses_as_a_whole() {
        let mut m = Model::open(":memory:").unwrap();
        let site = SiteProps {
            north_deg: 20.0,
            ..Default::default()
        };
        let n = ops(&m);
        assert!(m.set_project_data(&info(), &site).unwrap());
        assert_eq!(ops(&m), n + 1);
        assert!(!m.set_project_data(&info(), &site).unwrap());
        assert_eq!(ops(&m), n + 1);
        // a bad site leaves the valid fields unwritten
        let mut j = info();
        j.name = "Haus 8".into();
        let bad = SiteProps {
            latitude: 91.0,
            ..Default::default()
        };
        assert!(m.set_project_data(&j, &bad).is_err());
        assert_eq!(m.project_info().unwrap(), info());
        m.undo().unwrap();
        assert_eq!(m.project_info().unwrap(), ProjectInfo::default());
        assert_eq!(m.site().unwrap(), SiteProps::default());
    }

    #[test]
    fn signature_fields_are_stored_under_the_keys_title_blocks_read() {
        let mut m = Model::open(":memory:").unwrap();
        let mut i = info();
        for (k, v) in [
            ("organization", "O"),
            ("checker", "C"),
            ("approver", "A"),
            ("stage", "P"),
        ] {
            *i.slot(k).unwrap() = v.into();
        }
        m.set_project_info(&i).unwrap();
        assert_eq!(m.project_info().unwrap(), i);
        let stored = m.props(KIND, 0).unwrap();
        for (k, v) in [
            ("organization", "O"),
            ("checker", "C"),
            ("approver", "A"),
            ("stage", "P"),
        ] {
            let row = stored.iter().find(|p| p.0 == k).unwrap();
            assert_eq!(row.1, Value::Text(v.into()));
        }
        m.undo().unwrap();
        assert_eq!(m.project_info().unwrap(), ProjectInfo::default());
    }
}
