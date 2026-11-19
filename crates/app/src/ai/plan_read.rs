// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `plan_from_image` (plan g9): walls, doors, windows and room labels drafted from an attached
//! plan picture or PDF page. The geometry is the deterministic read of `ddc_io::plan_image`
//! (a picture) or of the page's vector lines (a PDF); a model that sees may add only room
//! labels and the scale. Reads below `min_confidence` are not drafted but listed to check.
//! Everything goes into plan items: nothing reaches the model before Apply.

use super::fmt::Out;
use super::session::{Core, Done};
use super::tools_write::{abbreviate_refs, create_raw};
use ddc_ai::CallCx;
use ddc_io::attach::Kind;
use ddc_io::drawing_model::{propose, wall_types, Memory, Proposals};
use ddc_io::plan_image::{read_plan, vector_options, Gray, ImageScale};
use serde_json::{json, Value as J};

/// How a read point becomes a model point: metres of the read, then the offset.
struct Read {
    proposals: Proposals,
    /// A `rooms[].at` point of the person's file (pixels or sheet millimetres) in read metres.
    at: Box<dyn Fn((f64, f64)) -> (f64, f64)>,
    info: J,
}

fn num(a: &J, k: &str) -> Result<Option<f64>, String> {
    match &a[k] {
        J::Null => Ok(None),
        v => v
            .as_f64()
            .filter(|x| x.is_finite() && *x > 0.0)
            .map(Some)
            .ok_or_else(|| format!("{k} must be a positive number")),
    }
}

/// The picture scale from the arguments: exactly one way of giving it.
fn image_scale(a: &J) -> Result<ImageScale, String> {
    let mut found = vec![];
    if let Some(k) = num(a, "px_per_m")? {
        found.push(ImageScale::PxPerM(k));
    }
    if let Some(dpi) = num(a, "dpi")? {
        let ratio = num(a, "scale")?.ok_or("dpi needs scale (N of 1:N)")?;
        found.push(ImageScale::Dpi { dpi, ratio });
    }
    if let Some(c) = a["calibrate"].as_array() {
        let v: Vec<f64> = c.iter().filter_map(J::as_f64).collect();
        if v.len() != 4 {
            return Err("calibrate must be [x1, y1, x2, y2] in pixels".into());
        }
        let length = num(a, "length_m")?.ok_or("calibrate needs length_m")?;
        found.push(ImageScale::Calibrate { a: (v[0], v[1]), b: (v[2], v[3]), length });
    }
    if let Some(w) = num(a, "width_m")? {
        found.push(ImageScale::Width(w));
    }
    if let Some(h) = num(a, "height_m")? {
        found.push(ImageScale::Height(h));
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err("the picture scale is unknown: give px_per_m, dpi with scale, calibrate with length_m, width_m or height_m".into()),
        _ => Err("give the picture scale one way only".into()),
    }
}

fn read_picture(bytes: &[u8], a: &J, types: &[ddc_io::drawing_model::WallTypeInfo]) -> Result<Read, String> {
    let (w, h, rgba) = crate::mat_tex::decode_image(bytes).ok_or("not a PNG or JPEG picture")?;
    let g = Gray::from_rgba(w, h, &rgba).ok_or("the picture is empty")?;
    let r = read_plan(&g, &image_scale(a)?, types, None)?;
    let (k, hp) = (r.px_per_m, r.height_px as f64);
    let info = json!({
        "source": "picture",
        "size_px": [w, h],
        "px_per_m": (k * 1000.0).round() / 1000.0,
        "angle_deg": (r.stats.angle_deg * 100.0).round() / 100.0,
        "lines": r.stats.segments,
        "swings": r.stats.arcs,
    });
    Ok(Read { proposals: r.proposals, at: Box::new(move |p| (p.0 / k, (hp - p.1) / k)), info })
}

fn read_pdf(bytes: &[u8], a: &J, sheet_ratio: Option<f64>, types: &[ddc_io::drawing_model::WallTypeInfo]) -> Result<Read, String> {
    use ddc_io::drawing_model::Underlay;
    use ddc_io::pdf::import::{ImportOptions, PdfFile, Scale};
    let page = a["page"].as_u64().unwrap_or(1).max(1) as usize - 1;
    let ratio = match (num(a, "scale")?, sheet_ratio) {
        (Some(r), _) => r,
        (None, Some(r)) => r,
        _ => return Err("the sheet scale is unknown: pass scale as N of 1:N".into()),
    };
    let doc = PdfFile::parse(bytes).map_err(|e| format!("{e:?}"))?;
    let pg = doc.page(page).map_err(|e| format!("{e:?}"))?;
    let opt = ImportOptions { scale: Scale::Ratio(ratio), ..Default::default() };
    let u = Underlay::from_pdf(&pg, &opt).map_err(|e| format!("{e:?}"))?;
    if u.lines.is_empty() {
        return Err("this PDF page has no vector lines (a scan?): attach the page as a PNG or JPEG picture".into());
    }
    let proposals = propose(&u, types, &vector_options(), &Memory::default());
    let info = json!({"source": "pdf", "page": page + 1, "scale": ratio, "lines": u.lines.len(), "swings": u.arcs.len()});
    let k = ratio / 1000.0;
    Ok(Read { proposals, at: Box::new(move |p| (p.0 * k, p.1 * k)), info })
}

fn r3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Draft items of `cat` in chunks the create tool takes; returns the refs of all items in order.
fn create_all(c: &mut Core, cx: &CallCx<'_>, title: &str, why: &J, cat: &str, items: Vec<J>, out: &mut Done2) -> Result<Vec<String>, String> {
    let mut refs = vec![];
    let n = items.len().div_ceil(ddc_ai::catalog::MAX_CREATE_ITEMS).max(1);
    for (k, chunk) in items.chunks(ddc_ai::catalog::MAX_CREATE_ITEMS).enumerate() {
        let t = if n > 1 { format!("{title} ({}/{n})", k + 1) } else { title.to_string() };
        let mut args = json!({"title": t, "cat": cat, "items": chunk});
        if why.is_string() {
            args["why"] = why.clone();
        }
        let (slot0, added) = create_raw(c, &args, cx)?;
        refs.extend((0..chunk.len()).map(|i| format!("{cat}:n{}", slot0 + i as u32)));
        out.items.push(added.id);
        out.counts.0 += added.counts.0;
        out.counts.1 += added.counts.1;
        out.counts.2 += added.counts.2;
        out.created.extend(added.created);
    }
    Ok(refs)
}

#[derive(Default)]
struct Done2 {
    items: Vec<u32>,
    counts: (usize, usize, usize),
    created: Vec<String>,
}

pub fn plan_from_image(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let f = super::attach::shared(c, super::attach::id_of(a)?)?;
    let bytes = f.bytes.clone().ok_or("only a picture (png, jpg) or a PDF page can be read as a plan")?;
    let types = wall_types(&c.ed.model).map_err(|e| e.to_string())?;
    let read = match f.kind {
        Kind::Image => read_picture(&bytes, a, &types)?,
        Kind::Pdf => read_pdf(&bytes, a, f.ex.scale.as_ref().map(|s| s.ratio), &types)?,
        _ => return Err("only a picture (png, jpg) or a PDF page can be read as a plan".into()),
    };
    let min = a["min_confidence"].as_f64().unwrap_or(0.5).clamp(0.0, 1.0);
    let off = a["offset"].as_array().map_or([0.0, 0.0], |v| {
        [v.first().and_then(J::as_f64).unwrap_or(0.0), v.get(1).and_then(J::as_f64).unwrap_or(0.0)]
    });
    let mv = |p: (f64, f64)| json!([r3(p.0 + off[0]), r3(p.1 + off[1])]);
    let level = a["level"].as_i64();
    let p = &read.proposals;
    let mut check: Vec<J> = vec![];

    let mut wall_items = vec![];
    let mut wall_slot = std::collections::HashMap::new();
    for w in &p.walls {
        if w.confidence < min {
            check.push(json!({"what": "wall", "a": mv(w.a), "b": mv(w.b), "thickness": r3(w.thickness), "confidence": r3(w.confidence)}));
            continue;
        }
        let mut o = json!({"a": mv(w.a), "b": mv(w.b)});
        if w.height > 0.0 {
            o["height"] = json!(w.height);
        }
        if let Some(t) = w.wall_type {
            o["type"] = json!(format!("wall_type:{t}"));
        }
        if let Some(l) = level {
            o["level"] = json!(l);
        }
        wall_slot.insert(w.id, wall_items.len());
        wall_items.push(o);
    }
    if wall_items.is_empty() {
        return Err(format!(
            "no walls read: {} candidates, all below confidence {min}; check the scale and the picture",
            p.walls.len()
        ));
    }
    let title = a["title"].as_str().filter(|t| !t.trim().is_empty()).map_or_else(|| format!("Plan from {}", f.name), str::to_string);
    let why = a["why"].clone();
    let mut done = Done2::default();
    let wall_refs = create_all(c, cx, &format!("{title}: walls"), &why, "wall", wall_items, &mut done)?;

    let (mut doors, mut windows) = (vec![], vec![]);
    for o in &p.openings {
        let host = wall_slot.get(&o.wall).map(|&i| wall_refs[i].clone());
        let Some(host) = host.filter(|_| o.confidence >= min) else {
            check.push(json!({"what": o.kind, "width": r3(o.width), "confidence": r3(o.confidence), "reason": "low confidence or its wall is not drafted"}));
            continue;
        };
        let item = json!({"wall": host, "along": r3(o.along), "width": r3(o.width), "height": r3(o.height), "sill": r3(o.sill)});
        if o.kind == "door" {
            doors.push(item);
        } else {
            windows.push(item);
        }
    }
    if !doors.is_empty() {
        create_all(c, cx, &format!("{title}: doors"), &why, "door", doors, &mut done)?;
    }
    if !windows.is_empty() {
        create_all(c, cx, &format!("{title}: windows"), &why, "window", windows, &mut done)?;
    }

    // room labels: those read from the page's text, then those the caller read on the picture
    let mut rooms: Vec<J> = p
        .rooms
        .iter()
        .filter(|r| r.confidence >= min && !r.name.trim().is_empty())
        .map(|r| json!({"name": r.name, "number": r.number, "point": mv(r.point)}))
        .collect();
    for r in a["rooms"].as_array().into_iter().flatten() {
        let name = r["name"].as_str().unwrap_or_default().trim();
        let at = r["at"].as_array().map(|v| (v.first().and_then(J::as_f64), v.get(1).and_then(J::as_f64)));
        let (Some((Some(x), Some(y))), false) = (at, name.is_empty()) else {
            return Err("each room needs name and at: [x, y]".into());
        };
        let mut o = json!({"name": name, "number": r["number"].as_str().unwrap_or_default(), "point": mv((read.at)((x, y)))});
        if let Some(l) = level {
            o["level"] = json!(l);
        }
        rooms.push(o);
    }
    if !rooms.is_empty() {
        create_all(c, cx, &format!("{title}: rooms"), &why, "room", rooms, &mut done)?;
    }

    let mut o = Out::new(c.max_chars);
    o.put("item", &json!(done.items[0]));
    o.put("items", &json!(done.items));
    o.put("changes", &json!({"created": done.counts.0, "changed": done.counts.1, "deleted": done.counts.2}));
    o.put("created", &J::from(abbreviate_refs(&done.created)));
    o.put("read", &read.info);
    o.put("found", &json!({"walls": p.walls.len(), "doors": p.report.doors, "windows": p.report.windows, "rooms": p.rooms.len()}));
    if !check.is_empty() {
        o.list("check", &check);
    }
    o.put("note", &json!("Draft only: the geometry is the deterministic read of the drawing; nothing reaches the model before Apply."));
    Ok(o.finish().into())
}
