//! Components as drawing blocks: one block per family type (one more per instance-value variant
//! that changes the plan symbol) with the type's plan lines in component coordinates and hidden
//! attribute definitions (element id, family, type, category, IFC class, every parameter,
//! classification codes); one INSERT per instance on the layer of its category, rotated, mirrored
//! by a negative x scale, its attributes filled with the instance's values.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use ddc_geom::component::{cache_key, Family, Lod, Value};
use ddc_model::{value_text, CompRecord, Model};

use super::{units_meters, AttDef, Attrib, Block, Common, DxfDocument, Entity, Insert, Layer, Line};

/// Layer of a component category: `COMP-FURNITURE`, `COMP-AIR_TERMINAL`, ...
pub fn category_layer(category: &str) -> String {
    format!("COMP-{}", category.to_ascii_uppercase())
}

/// Attribute tag: upper case, letters, digits and `_` only.
pub fn attr_tag(name: &str) -> String {
    let t: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
        .collect();
    if t.is_empty() { "_".into() } else { t }
}

/// Characters a block name may not hold.
fn block_name(s: &str) -> String {
    s.chars()
        .map(|c| if "<>/\\\":;?*|=`,".contains(c) { '_' } else { c })
        .collect()
}

/// What `append_components` added.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompBlocksReport {
    pub blocks: usize,
    pub inserts: usize,
}

const FIXED: [&str; 6] = ["ID", "FAMILY", "TYPE", "CATEGORY", "IFC_CLASS", "IFC_TYPE"];

/// (tag, value) of every attribute of an instance, in block order.
fn attributes(r: &CompRecord, systems: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![
        ("ID".into(), r.id.to_string()),
        ("FAMILY".into(), r.family.clone()),
        ("TYPE".into(), r.type_name.clone()),
        ("CATEGORY".into(), r.category.into()),
        ("IFC_CLASS".into(), r.ifc_class.clone()),
        ("IFC_TYPE".into(), r.ifc_predefined.clone()),
    ];
    let mut taken: BTreeSet<String> = FIXED.iter().map(|s| s.to_string()).collect();
    let mut unique = |base: String| {
        let mut t = base.clone();
        let mut n = 2;
        while !taken.insert(t.clone()) {
            t = format!("{base}_{n}");
            n += 1;
        }
        t
    };
    for p in &r.params {
        out.push((unique(attr_tag(&p.name)), value_text(&p.value)));
    }
    for s in systems {
        let code = r.classification.iter().find(|c| &c.0 == s).map(|c| c.1.clone());
        out.push((unique(format!("CLASS_{}", attr_tag(s))), code.unwrap_or_default()));
    }
    out
}

/// Adds the components of `m` (all levels) to `doc`: blocks, category layers, model-space
/// INSERTs. Coordinates go in the document's units (`$INSUNITS`; unset: metres). Block names
/// already in `doc` are not reused: a clash gets a `~n` suffix.
pub fn append_components(doc: &mut DxfDocument, m: &Model) -> Result<CompBlocksReport, String> {
    let k = match doc.insunits() {
        0 => 1.0,
        u => 1.0 / units_meters(u).ok_or(format!("unknown drawing units {u}"))?,
    };
    let recs = m.component_records(None).map_err(|e| e.to_string())?;
    let systems: Vec<String> = recs
        .iter()
        .flat_map(|r| r.classification.iter().map(|c| c.0.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut taken: BTreeSet<String> = doc.blocks.iter().map(|b| b.name.to_uppercase()).collect();
    let mut fams: HashMap<i64, Arc<Family>> = HashMap::new();
    let mut tvals: HashMap<i64, Vec<(String, Value)>> = HashMap::new();
    // (type, cache key of the resolved values) -> block name
    let mut variants: HashMap<(i64, String), String> = HashMap::new();
    let mut per_type: HashMap<i64, usize> = HashMap::new();
    let mut rep = CompBlocksReport::default();
    for r in &recs {
        if !fams.contains_key(&r.family_id) {
            fams.insert(r.family_id, Arc::new(m.family(r.family_id).map_err(|e| e.to_string())?));
        }
        let f = fams[&r.family_id].clone();
        if !tvals.contains_key(&r.type_id) {
            tvals.insert(r.type_id, m.comp_type_values(r.type_id).map_err(|e| e.to_string())?);
        }
        let at = format!("comp:{}", r.id);
        let iv = f.assignments(&r.vals).map_err(|e| format!("{at}: {e}"))?;
        let res = f.resolve(&tvals[&r.type_id], &iv).map_err(|e| format!("{at}: {e}"))?;
        let key = (r.type_id, cache_key(&res));
        let attrs = attributes(r, &systems);
        let name = match variants.get(&key) {
            Some(n) => n.clone(),
            None => {
                let n = per_type.entry(r.type_id).or_insert(0);
                *n += 1;
                let base = match *n {
                    1 => block_name(&format!("{} - {}", r.family, r.type_name)),
                    v => block_name(&format!("{} - {} v{v}", r.family, r.type_name)),
                };
                let mut name = base.clone();
                let mut i = 2;
                while !taken.insert(name.to_uppercase()) {
                    name = format!("{base}~{i}");
                    i += 1;
                }
                let built = f.build(&res, Lod::Medium).map_err(|e| format!("{at}: {e}"))?;
                let mut b = Block::new(name.clone());
                b.entities = built
                    .plan
                    .iter()
                    .map(|(a, q)| {
                        Entity::Line(Line {
                            common: Common::default(),
                            start: [a.x * k, a.y * k, 0.0],
                            end: [q.x * k, q.y * k, 0.0],
                        })
                    })
                    .collect();
                b.attdefs = attrs
                    .iter()
                    .map(|(tag, _)| AttDef {
                        common: Common::default(),
                        tag: tag.clone(),
                        prompt: tag.clone(),
                        default: String::new(),
                        pos: [0.0; 3],
                        height: 0.1 * k,
                        // hidden: the plan shows the symbol, the data rides along
                        flags: 1,
                    })
                    .collect();
                doc.blocks.push(b);
                rep.blocks += 1;
                variants.insert(key, name.clone());
                name
            }
        };
        let layer = category_layer(r.category);
        if !doc.layers.iter().any(|l| l.name.eq_ignore_ascii_case(&layer)) {
            doc.layers.push(Layer {
                name: layer.clone(),
                flags: 0,
                color: 7,
                linetype: "Continuous".into(),
                extra: vec![],
            });
        }
        let o = r.placement.origin;
        let pos = [o.0 * k, o.1 * k, o.2 * k];
        doc.entities.push(Entity::Insert(Insert {
            common: Common::on_layer(&layer),
            block: name,
            pos,
            scale: [if r.placement.mirror { -1.0 } else { 1.0 }, 1.0, 1.0],
            rotation: r.placement.rotation.to_degrees(),
            attribs: attrs
                .into_iter()
                .map(|(tag, value)| Attrib {
                    common: Common::on_layer(&layer),
                    tag,
                    value,
                    pos,
                    height: 0.1 * k,
                    flags: 1,
                })
                .collect(),
        }));
        rep.inserts += 1;
    }
    Ok(rep)
}
