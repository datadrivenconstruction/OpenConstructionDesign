//! Element counts, levels and warnings of a loaded model.

use ddc_model::Model;
use serde_json::{json, Value};

use crate::load::Loaded;

pub const INFO_SCHEMA: &str = "ddc-cli-info/1";

pub use ddc_model::{category_counts as counts, COUNTED_CATEGORIES as CATEGORIES};

pub fn levels(m: &Model) -> Vec<(String, f64)> {
    let Ok(mut st) = m
        .sql()
        .prepare("SELECT name, elevation FROM level ORDER BY elevation, id")
    else {
        return vec![];
    };
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)));
    rows.map(|it| it.flatten().collect()).unwrap_or_default()
}

pub fn document(file: &str, l: &Loaded) -> Value {
    let c = counts(&l.model);
    json!({
        "schema": INFO_SCHEMA,
        "file": file,
        "kind": l.kind,
        "counts": c
            .iter()
            .map(|(k, n)| (k.to_string(), json!(n)))
            .collect::<serde_json::Map<_, _>>(),
        "levels": levels(&l.model)
            .iter()
            .map(|(n, e)| json!({"name": n, "elevation": e}))
            .collect::<Vec<_>>(),
        "warnings": l.warnings,
        "warnings_total": l.warnings.len(),
    })
}

pub fn text(d: &Value) -> String {
    let mut s = format!(
        "{} ({})\n",
        d["file"].as_str().unwrap_or(""),
        d["kind"].as_str().unwrap_or("")
    );
    if let Some(c) = d["counts"].as_object() {
        for (k, n) in c.iter().filter(|(_, n)| n.as_i64().unwrap_or(0) > 0) {
            s += &format!("  {k}: {n}\n");
        }
    }
    for l in d["levels"].as_array().into_iter().flatten() {
        s += &format!(
            "  level {} @ {}\n",
            l["name"].as_str().unwrap_or(""),
            l["elevation"]
        );
    }
    s += &format!("  warnings: {}\n", d["warnings_total"]);
    s
}
