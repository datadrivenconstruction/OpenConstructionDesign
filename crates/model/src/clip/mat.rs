// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Materials in the clipboard: a snapshot carries the material rows its elements name (with
//! embedded images) and the material of its imported meshes, by name. A paste into another
//! project creates the names it lacks and keeps rows that exist there; the differing names are
//! reported. A paste inside one project touches none of this.

use std::collections::BTreeSet;

use rusqlite::{params_from_iter, types::Value, Connection};

use super::{columns, has_table, int, select_rows, text, Clip, ClipRows, Paster};
use crate::Error;

const MESH_COLS: [&str; 4] = ["id", "scope", "key", "material"];

fn all_cols(db: &Connection, t: &str) -> Result<Vec<String>, Error> {
    Ok(columns(db, t)?.into_iter().map(|c| c.0).collect())
}

/// Total size of material images one clip may carry, bytes.
pub(super) const MAT_IMAGES_MAX: i64 = 8 * 1024 * 1024;

/// Adds the referenced material rows, their images and the mesh materials to `clip`.
pub(super) fn snapshot(db: &Connection, clip: &mut Clip) -> Result<(), Error> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for (_, r) in clip.tables.iter().chain(clip.dicts.iter()) {
        if let Some(i) = r.cols.iter().position(|c| c == "material") {
            names.extend(
                r.rows
                    .iter()
                    .filter_map(|row| text(&row[i]))
                    .filter(|n| !n.is_empty())
                    .map(str::to_string),
            );
        }
    }
    let mut mesh_rows: Vec<Vec<Value>> = vec![];
    if let (Some(m), true) = (clip.rows("mesh"), has_table(db, "mesh_material")?) {
        let col = |c: &str| m.cols.iter().position(|x| x == c);
        let mut keys: BTreeSet<(&str, String)> = BTreeSet::new();
        for row in &m.rows {
            if let Some(id) = col("id").and_then(|i| int(&row[i])) {
                keys.insert(("element", id.to_string()));
            }
            if let Some(c) = col("category").and_then(|i| text(&row[i])) {
                keys.insert(("category", c.to_string()));
            }
        }
        let sql = "SELECT id,scope,key,material FROM mesh_material WHERE scope=?1 AND key=?2";
        for (scope, key) in keys {
            let got = select_rows(
                db,
                sql,
                MESH_COLS.len(),
                &[Value::Text(scope.to_string()), Value::Text(key)],
            )?;
            for row in got {
                if let Some(n) = text(&row[3]) {
                    names.insert(n.to_string());
                }
                mesh_rows.push(row);
            }
        }
    }
    let mut out: Vec<(String, ClipRows)> = vec![];
    if has_table(db, "material")? && !names.is_empty() {
        let cols = all_cols(db, "material")?;
        let sql = format!("SELECT {} FROM material WHERE name=?1", cols.join(","));
        let mut rows = vec![];
        for n in &names {
            rows.extend(select_rows(db, &sql, cols.len(), &[Value::Text(n.clone())])?);
        }
        let mut images: BTreeSet<String> = BTreeSet::new();
        let tex = cols.iter().position(|c| c == "tex_ref");
        if let Some(i) = tex {
            images.extend(rows.iter().filter_map(|r| text(&r[i])).map(str::to_string));
        }
        let mut img_rows = vec![];
        if has_table(db, "material_image")? && !images.is_empty() {
            // the system clipboard holds the whole text: images past the budget are left out
            let mut budget = MAT_IMAGES_MAX;
            for h in &images {
                let got = select_rows(
                    db,
                    "SELECT hash,bytes FROM material_image WHERE hash=?1 AND length(bytes)<=?2",
                    2,
                    &[Value::Text(h.clone()), Value::Integer(budget)],
                )?;
                for row in got {
                    if let Value::Blob(b) = &row[1] {
                        budget -= b.len() as i64;
                    }
                    img_rows.push(row);
                }
            }
        }
        if let Some(i) = tex {
            // a material whose image was left out pastes without a texture, not with a dangling ref
            let kept: BTreeSet<&str> = img_rows.iter().filter_map(|r| text(&r[0])).collect();
            let kind = cols.iter().position(|c| c == "tex_kind");
            for r in &mut rows {
                // tex_ref of a builtin or file texture is a key or a path, not an image hash
                let embedded = kind.is_none_or(|k| text(&r[k]) == Some("embedded"));
                let dropped = embedded && text(&r[i]).is_some_and(|h| !kept.contains(h));
                if dropped {
                    r[i] = Value::Null;
                    if let Some(k) = kind {
                        r[k] = Value::Null;
                    }
                }
            }
        }
        if !rows.is_empty() {
            out.push(("material".to_string(), ClipRows { cols, rows }));
        }
        if !img_rows.is_empty() {
            let cols = vec!["hash".to_string(), "bytes".to_string()];
            out.push(("material_image".to_string(), ClipRows { cols, rows: img_rows }));
        }
    }
    if !mesh_rows.is_empty() {
        let cols = MESH_COLS.iter().map(|c| (*c).to_string()).collect();
        out.push((
            "mesh_material".to_string(),
            ClipRows {
                cols,
                rows: mesh_rows,
            },
        ));
    }
    clip.materials = out;
    Ok(())
}

impl Paster<'_> {
    fn mat_rows(&self, t: &str) -> Option<&ClipRows> {
        self.clip
            .materials
            .iter()
            .find(|(n, _)| n == t)
            .map(|(_, r)| r)
    }

    /// Material rows and images before the elements: a missing name is created with the source
    /// properties, an existing row stays and its name goes to the report when it differs.
    pub(super) fn paste_materials(&mut self) -> Result<(), Error> {
        if self.same_project || !has_table(self.db, "material")? {
            return Ok(());
        }
        let db = self.db;
        if let (Some(im), true) = (self.mat_rows("material_image"), has_table(db, "material_image")?)
        {
            for row in &im.rows {
                db.execute(
                    "INSERT OR IGNORE INTO material_image(hash,bytes) VALUES (?1,?2)",
                    params_from_iter(row.iter()),
                )?;
            }
        }
        let Some(src) = self.mat_rows("material") else {
            return Ok(());
        };
        let target = all_cols(db, "material")?;
        let keep: Vec<usize> = (0..src.cols.len())
            .filter(|i| src.cols[*i] != "id" && target.contains(&src.cols[*i]))
            .collect();
        let Some(name_at) = keep.iter().position(|i| src.cols[*i] == "name") else {
            return Ok(());
        };
        let names: Vec<&str> = keep.iter().map(|i| src.cols[*i].as_str()).collect();
        let select = format!("SELECT {} FROM material WHERE name=?1", names.join(","));
        let insert = format!(
            "INSERT INTO material({}) VALUES ({})",
            names.join(","),
            vec!["?"; names.len()].join(",")
        );
        let (mut created, mut differ) = (vec![], vec![]);
        for row in &src.rows {
            let vals: Vec<&Value> = keep.iter().map(|i| &row[*i]).collect();
            let Some(name) = text(vals[name_at]) else {
                continue;
            };
            let cur = select_rows(db, &select, keep.len(), &[Value::Text(name.to_string())])?;
            match cur.first() {
                None => {
                    db.execute(&insert, params_from_iter(vals.iter().copied()))?;
                    created.push(name.to_string());
                }
                Some(c) => {
                    if c.iter().zip(&vals).any(|(a, b)| a != *b) {
                        differ.push(name.to_string());
                    }
                }
            }
        }
        self.report.materials_created.extend(created);
        self.report.materials_differ.extend(differ);
        Ok(())
    }

    /// Materials of the pasted meshes, after the meshes got their new ids; rows already there stay.
    pub(super) fn paste_mesh_materials(&mut self) -> Result<(), Error> {
        if self.same_project || !has_table(self.db, "mesh_material")? {
            return Ok(());
        }
        let Some(src) = self.mat_rows("mesh_material") else {
            return Ok(());
        };
        let at = |c: &str| src.cols.iter().position(|x| x == c);
        let (Some(si), Some(ki), Some(mi)) = (at("scope"), at("key"), at("material")) else {
            return Ok(());
        };
        let fresh = self.map.get("mesh");
        for row in &src.rows {
            let (Some(scope), Some(key), Some(mat)) =
                (text(&row[si]), text(&row[ki]), text(&row[mi]))
            else {
                continue;
            };
            let key = if scope == "element" {
                let new = key
                    .parse::<i64>()
                    .ok()
                    .and_then(|old| fresh.and_then(|m| m.get(&old)));
                match new {
                    Some(n) => n.to_string(),
                    None => continue,
                }
            } else {
                key.to_string()
            };
            self.db.execute(
                "INSERT OR IGNORE INTO mesh_material(scope,key,material) VALUES (?1,?2,?3)",
                rusqlite::params![scope, key, mat],
            )?;
        }
        Ok(())
    }
}
