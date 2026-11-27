// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Textures of materials in the realistic 3D style. Reads the texture slots from the model,
//! hands the images to the renderer (`ddc_render::texture::publish`) and tells the scene builders
//! which texture layer a face takes. The renderer reads the layer from the face tag; styles other
//! than realistic ignore it. Images are PNG or JPEG. A material without a slot takes its library
//! look (`ddc_model::material_look`): a built-in texture tinted to the look colour, one layer per
//! look, so many material names share few layers. Images are made only once a realistic view is
//! on screen; the element query runs once per model revision.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use ddc_model::{MaterialTextureRow, TextureSource};
use ddc_render::texture::{self, TexLayer, TextureSet, MAX_LAYERS};

use crate::editor::Editor;

/// Largest image side taken from a file; bigger ones are refused, not scaled.
const MAX_SIDE: u32 = 8192;

/// Texture layer per element and per wall layer.
#[derive(Default, Debug)]
pub struct Layers {
    elem: HashMap<(&'static str, i64), u8>,
    /// Typed parts (solids) whose material is a finish: the look colour in every style.
    elem_rgb: HashMap<(&'static str, i64), [u8; 3]>,
    wall: HashMap<(i64, usize), u8>,
    /// Wall type layers drawn with their library look (finishes keep their shaded colour).
    wall_look: HashMap<(i64, usize), u8>,
    /// Layer stacks of slabs, ceilings and roofs, top first: thickness and texture layer.
    stack: HashMap<(&'static str, i64), Vec<(f32, Option<u8>)>>,
    /// Wall types with at least one textured layer.
    wall_types: HashSet<i64>,
}

impl Layers {
    pub fn is_empty(&self) -> bool {
        self.elem.is_empty() && self.elem_rgb.is_empty() && self.wall.is_empty() && self.wall_look.is_empty() && self.stack.is_empty()
    }

    /// Tag of a wall type layer drawn with its library look; the plain layer number without one.
    pub fn wall_look(&self, wall_type: i64, layer: usize) -> u32 {
        match self.wall_look.get(&(wall_type, layer)) {
            Some(&l) => texture::with_layer(layer as u32, l),
            None => layer as u32,
        }
    }

    /// Whether any layer of walls of this type has a texture (such walls get an anchor).
    pub fn wall_textured(&self, wall_type: i64) -> bool {
        self.wall_types.contains(&wall_type)
    }

    /// The faces of a slab, ceiling or roof split by layer: the top face takes the top layer,
    /// the bottom face the bottom layer and, on a flat element, the edge bands their own layers.
    /// `None` when the type has one layer or no layer has a texture. `tag` is the plain tag of the
    /// element; the thickest layer keeps serving edges that cannot be banded.
    pub fn split(
        &self,
        table: &'static str,
        id: i64,
        m: &ddc_render::SceneMesh,
        tag: u32,
    ) -> Option<Vec<ddc_render::SceneMesh>> {
        let st = self.stack.get(&(table, id))?;
        let tags: Vec<(f32, u32)> = st
            .iter()
            .map(|&(t, l)| (t, l.map_or(tag, |l| texture::with_layer(tag, l))))
            .collect();
        let thickest = tags
            .iter()
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map_or(tag, |t| t.1);
        let out = crate::layer_split::split(m, &tags, thickest);
        (!out.is_empty()).then_some(out)
    }

    /// `tag` of an element's faces with its material texture, unchanged without one.
    pub fn elem(&self, table: &'static str, id: i64, tag: u32) -> u32 {
        if let Some(c) = self.elem_rgb.get(&(table, id)) {
            return ddc_render::tag_rgb(*c, false);
        }
        match self.elem.get(&(table, id)) {
            Some(&l) => texture::with_layer(tag, l),
            None => tag,
        }
    }

    /// Tag of layer `layer` (position in the type) of a wall of type `wall_type`; the plain layer
    /// number when its material has no texture.
    pub fn wall(&self, wall_type: i64, layer: usize) -> u32 {
        match self.wall.get(&(wall_type, layer)) {
            Some(&l) => texture::with_layer(layer as u32, l),
            None => layer as u32,
        }
    }
}

/// What the renderer was last given.
#[derive(Default, PartialEq)]
struct Published {
    keys: Vec<String>,
    /// Images are decoded (a realistic view has been on screen); otherwise only placeholders.
    decoded: bool,
    /// Image files behind the layers and their stamps when read.
    files: Vec<(String, String)>,
    checked: Option<web_time::Instant>,
}

struct Cached {
    rev: u64,
    /// Keys of the slots: a slot edit does not bump the revision of the cache key alone.
    keys: Vec<String>,
    layers: Rc<Layers>,
    srcs: Rc<Vec<Src>>,
    all_keys: Rc<Vec<String>>,
}

/// A library look as a texture layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LookTex {
    pub texture: &'static str,
    pub rgb: [u8; 3],
    pub tile_m: f32,
    pub roughness: f32,
    pub metallic: f32,
}

impl LookTex {
    pub fn of(r: &ddc_model::material_look::Resolved) -> LookTex {
        let c = r.rgb;
        LookTex {
            texture: r.look.texture,
            rgb: [(c >> 16) as u8, (c >> 8) as u8, c as u8],
            tile_m: r.look.tile_m,
            roughness: r.roughness,
            metallic: r.metallic,
        }
    }

    fn key(&self) -> String {
        let [r, g, b] = self.rgb;
        format!("look|{}|{r:02x}{g:02x}{b:02x}|{}|{}|{}", self.texture, self.tile_m, self.roughness, self.metallic)
    }

    fn layer(&self) -> Option<TexLayer> {
        TexLayer::look(self.texture, self.rgb, self.tile_m, self.roughness, self.metallic)
    }

    fn placeholder(&self) -> TexLayer {
        TexLayer {
            rgba: vec![],
            scale_m: texture::clamp_scale(self.tile_m),
            rotation_deg: 0.0,
            roughness: self.roughness,
            metallic: self.metallic,
        }
    }
}

/// Where the image of a layer comes from.
#[derive(Clone)]
enum Src {
    Slot(MaterialTextureRow),
    Look(LookTex),
}

thread_local! {
    static CACHE: RefCell<Option<Cached>> = const { RefCell::new(None) };
    static IMAGES: RefCell<HashMap<String, Option<TexLayer>>> = RefCell::new(HashMap::new());
    static PUBLISHED: RefCell<Published> = RefCell::new(Published::default());
}

/// `image` stamps an embedded image (`Model::material_texture_slots`).
/// Size and modification time of an image file; empty when it cannot be read.
fn file_stamp(p: &str) -> String {
    std::fs::metadata(p)
        .ok()
        .map(|m| {
            let t = ddc_model::wtime::modified(&m)
                .ok()
                .and_then(|t| t.duration_since(web_time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{}:{t}", m.len())
        })
        .unwrap_or_default()
}

fn row_key(r: &MaterialTextureRow, image: &str) -> String {
    let stamp = match &r.tex.source {
        TextureSource::File(p) => file_stamp(p),
        TextureSource::Embedded => image.to_string(),
        TextureSource::Builtin(_) => String::new(),
    };
    format!(
        "{}|{:?}|{}|{}|{stamp}",
        r.material, r.tex.source, r.tex.scale_m, r.tex.rotation_deg
    )
}

/// 8-bit RGBA from a PNG or a JPEG, chosen by the first bytes; None when it is neither, is
/// damaged or is too big.
pub fn decode_image(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        decode_jpeg(bytes)
    } else {
        decode_png(bytes)
    }
}

/// 8-bit RGBA from a baseline or progressive JPEG (grey, YCbCr; other colour spaces are refused
/// by the decoder).
pub fn decode_jpeg(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    dec.decode_headers().ok()?;
    let info = dec.info()?;
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return None;
    }
    let mut out = vec![0u8; dec.output_buffer_size()?];
    dec.decode_into(&mut out).ok()?;
    ((w as usize) * (h as usize) * 4 == out.len()).then_some((w, h, out))
}

/// 8-bit RGBA from a PNG; None when it is not a PNG or is too big.
pub fn decode_png(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let (w, h) = {
        let i = reader.info();
        (i.width, i.height)
    };
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return None;
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let fr = reader.next_frame(&mut buf).ok()?;
    let data = &buf[..fr.buffer_size()];
    let px = (w as usize) * (h as usize);
    let rgba = match fr.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => data
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Indexed => return None,
    };
    (rgba.len() == px * 4).then_some((w, h, rgba))
}

/// The image of one slot, cached by what it depends on.
fn layer_of(ed: &Editor, r: &MaterialTextureRow, key: &str) -> Option<TexLayer> {
    if let Some(hit) = IMAGES.with(|c| c.borrow().get(key).cloned()) {
        return hit;
    }
    let (scale, rot) = (r.tex.scale_m as f32, r.tex.rotation_deg as f32);
    let layer = match &r.tex.source {
        TextureSource::Builtin(k) => TexLayer::builtin(k, Some(scale), rot),
        TextureSource::File(p) => std::fs::read(p)
            .ok()
            .and_then(|b| decode_image(&b))
            .and_then(|(w, h, d)| TexLayer::from_image(w, h, &d, scale, rot, 0.7)),
        TextureSource::Embedded => ed
            .model
            .material_texture_image(&r.material)
            .ok()
            .flatten()
            .as_deref()
            .and_then(decode_image)
            .and_then(|(w, h, d)| TexLayer::from_image(w, h, &d, scale, rot, 0.7)),
    };
    IMAGES.with(|c| {
        let mut c = c.borrow_mut();
        // a long session editing scales must not keep every old image
        if c.len() > 4 * MAX_LAYERS {
            c.clear();
        }
        c.insert(key.to_string(), layer.clone());
    });
    layer
}

/// Layer without pixels: keeps its index in the set until the images are wanted.
fn placeholder(r: &MaterialTextureRow) -> TexLayer {
    TexLayer {
        rgba: vec![],
        scale_m: texture::clamp_scale(r.tex.scale_m as f32),
        rotation_deg: r.tex.rotation_deg as f32,
        roughness: 0.7,
        metallic: 0.0,
    }
}

/// The tinted image of a look, cached by its key.
fn look_layer(t: &LookTex, key: &str) -> Option<TexLayer> {
    if let Some(hit) = IMAGES.with(|c| c.borrow().get(key).cloned()) {
        return hit;
    }
    let layer = t.layer();
    IMAGES.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 4 * MAX_LAYERS {
            c.clear();
        }
        c.insert(key.to_string(), layer.clone());
    });
    layer
}

/// Gives the renderer the layers of the slots and looks, made only once a realistic view asked
/// for them. A slot whose image cannot be read stays a grey layer, so the layer numbers in face
/// tags hold.
fn publish_set(ed: &Editor, srcs: &[Src], keys: &[String]) -> bool {
    let decoded = texture::wanted();
    let same = PUBLISHED.with(|p| {
        let p = p.borrow();
        p.keys == keys && p.decoded == decoded
    });
    if same {
        return false;
    }
    let mut set = TextureSet::default();
    let mut files = vec![];
    for (src, key) in srcs.iter().zip(keys) {
        let r = match src {
            Src::Slot(r) => r,
            Src::Look(t) => {
                set.layers.push(if decoded {
                    look_layer(t, key).unwrap_or_else(|| t.placeholder())
                } else {
                    t.placeholder()
                });
                continue;
            }
        };
        if let TextureSource::File(path) = &r.tex.source {
            files.push((path.clone(), file_stamp(path)));
        }
        set.layers.push(if decoded {
            layer_of(ed, r, key)
                .or_else(|| {
                    TexLayer::from_image(1, 1, &[128, 128, 128, 255], r.tex.scale_m as f32, 0.0, 0.7)
                })
                .unwrap_or_else(|| placeholder(r))
        } else {
            placeholder(r)
        });
    }
    texture::publish(set);
    PUBLISHED.with(|p| {
        *p.borrow_mut() = Published {
            keys: keys.to_vec(),
            decoded,
            files,
            checked: Some(web_time::Instant::now()),
        }
    });
    true
}

/// Per-frame upkeep: decodes the images when a realistic view has just appeared and notices an
/// image file changed on disk (checked every two seconds). True when new layers were published
/// and a repaint is due.
pub fn sync(ed: &Editor) -> bool {
    let due = PUBLISHED.with(|p| {
        let mut p = p.borrow_mut();
        if p.keys.is_empty() {
            return false;
        }
        if texture::wanted() && !p.decoded {
            return true;
        }
        let now = web_time::Instant::now();
        if p.files.is_empty() || p.checked.is_some_and(|t| now.duration_since(t).as_secs() < 2) {
            return false;
        }
        p.checked = Some(now);
        p.files.iter().any(|(f, st)| file_stamp(f) != *st)
    });
    if !due {
        return false;
    }
    // the keys or the decoded flag differ now, so this publishes
    let _ = layers(ed);
    true
}

/// Texture layers for the current model: one query per model revision, then cached.
pub fn layers(ed: &Editor) -> Rc<Layers> {
    // no blobs here: this runs several times per scene build
    let slots: Vec<_> = ed
        .model
        .material_texture_slots()
        .unwrap_or_default()
        .into_iter()
        .take(MAX_LAYERS)
        .collect();
    let keys: Vec<String> = slots.iter().map(|(r, s)| row_key(r, s)).collect();
    let hit = CACHE.with(|c| {
        c.borrow()
            .as_ref()
            .filter(|c| c.rev == ed.revision && c.keys == keys)
            .map(|c| (c.layers.clone(), c.srcs.clone(), c.all_keys.clone()))
    });
    if let Some((l, srcs, all)) = hit {
        publish(ed, &srcs, &all);
        return l;
    }
    // slot layers first, by slot position, whether or not the image is decoded yet
    let by_name: HashMap<&str, u8> = slots
        .iter()
        .enumerate()
        .map(|(i, (r, _))| (r.material.as_str(), i as u8))
        .collect();
    let elems = ed.model.element_materials().unwrap_or_default();
    let walls = ed.model.all_wall_layer_materials().unwrap_or_default();
    // then one layer per look, the most used first, while the table has room
    let mut resolved: HashMap<String, Option<LookTex>> = HashMap::new();
    let mut look = |name: &str| -> Option<LookTex> {
        if let Some(t) = resolved.get(name) {
            return *t;
        }
        let t = ed.model.material_look(name).ok().flatten().map(|r| LookTex::of(&r));
        resolved.insert(name.to_string(), t);
        t
    };
    let mut uses: HashMap<String, (LookTex, usize)> = HashMap::new();
    let mut elem_look: Vec<((&'static str, i64), String)> = vec![];
    let mut elem_rgb: HashMap<(&'static str, i64), [u8; 3]> = HashMap::new();
    let mut finish: HashMap<&str, Option<[u8; 3]>> = HashMap::new();
    for (table, id, material) in &elems {
        let Some(table) = static_table(table) else { continue };
        if let Some(&l) = by_name.get(material.as_str()) {
            elem_look.push(((table, *id), format!("#{l}")));
        } else if let Some(c) = (table == "solid").then(|| finish.entry(material.as_str()).or_insert_with(|| {
            let r = ed.model.material_look(material).ok().flatten().filter(|r| r.look.finish)?;
            Some([(r.rgb >> 16) as u8, (r.rgb >> 8) as u8, r.rgb as u8])
        })).and_then(|c| *c) {
            elem_rgb.insert((table, *id), c);
        } else if let Some(t) = look(material) {
            let k = t.key();
            uses.entry(k.clone()).or_insert((t, 0)).1 += 1;
            elem_look.push(((table, *id), k));
        }
    }
    let mut wall_look: Vec<((i64, usize), String, bool)> = vec![];
    let mut rank: HashMap<i64, usize> = HashMap::new();
    for (wtype, material) in &walls {
        let n = rank.entry(*wtype).or_insert(0);
        if let Some(&l) = by_name.get(material.as_str()) {
            wall_look.push(((*wtype, *n), format!("#{l}"), true));
        } else if crate::editor::opening_fill::finish_rgb(material).is_none() {
            // finishes keep their colour in every style; structural layers take the look
            if let Some(t) = look(material) {
                let k = t.key();
                uses.entry(k.clone()).or_insert((t, 0)).1 += 1;
                wall_look.push(((*wtype, *n), k, false));
            }
        }
        *n += 1;
    }
    let mut order: Vec<(String, LookTex, usize)> = uses.into_iter().map(|(k, (t, n))| (k, t, n)).collect();
    order.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    order.truncate(MAX_LAYERS - slots.len());
    let mut srcs: Vec<Src> = slots.iter().map(|(r, _)| Src::Slot(r.clone())).collect();
    let mut all = keys.clone();
    let mut index: HashMap<String, u8> = (0..slots.len()).map(|i| (format!("#{i}"), i as u8)).collect();
    for (k, t, _) in order {
        index.insert(k.clone(), srcs.len() as u8);
        srcs.push(Src::Look(t));
        all.push(k);
    }
    let mut out = Layers { elem_rgb, ..Layers::default() };
    for (at, k) in elem_look {
        if let Some(&l) = index.get(&k) {
            out.elem.insert(at, l);
        }
    }
    for (at, k, slot) in wall_look {
        if let Some(&l) = index.get(&k) {
            if slot {
                out.wall.insert(at, l);
                out.wall_types.insert(at.0);
            } else {
                out.wall_look.insert(at, l);
            }
        }
    }
    // stacks split by slot textures only; a look textures the element whole
    let mut stacks: HashMap<(&'static str, i64), Vec<(f32, Option<u8>)>> = HashMap::new();
    let typed = if slots.is_empty() { vec![] } else { ed.model.typed_layers().unwrap_or_default() };
    for (table, id, material, thickness) in typed {
        if let Some(table) = static_table(&table) {
            stacks
                .entry((table, id))
                .or_default()
                .push((thickness as f32, by_name.get(material.as_str()).copied()));
        }
    }
    // one layer is the plain element texture; untextured stacks need no splitting
    out.stack = stacks
        .into_iter()
        .filter(|(_, v)| v.len() > 1 && v.iter().any(|l| l.1.is_some()))
        .collect();
    let out = Rc::new(out);
    let (srcs, all) = (Rc::new(srcs), Rc::new(all));
    publish(ed, &srcs, &all);
    CACHE.with(|c| {
        *c.borrow_mut() = Some(Cached {
            rev: ed.revision,
            keys,
            layers: out.clone(),
            srcs,
            all_keys: all,
        })
    });
    out
}

/// Publishes the layers, or an empty set once none is left.
fn publish(ed: &Editor, srcs: &[Src], keys: &[String]) {
    if srcs.is_empty() {
        if PUBLISHED.with(|p| !p.borrow().keys.is_empty()) {
            texture::publish(TextureSet::default());
            PUBLISHED.with(|p| *p.borrow_mut() = Published::default());
        }
        return;
    }
    publish_set(ed, srcs, keys);
}

/// Table of an element row that can carry a material: the drawn tables and every table of the
/// material check.
pub(crate) fn static_table(t: &str) -> Option<&'static str> {
    ["wall", "slab", "ceiling", "roof", "col", "beam", "stair", "railing", "mep_run", "mesh"]
        .into_iter()
        .chain(ddc_model::material_look::CHECKED.iter().map(|c| c.0))
        .find(|k| *k == t)
}

#[cfg(test)]
#[path = "mat_look_tests.rs"]
mod look_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(w: u32, h: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = vec![];
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(color);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(data).unwrap();
        }
        out
    }

    #[test]
    fn png_variants_become_rgba() {
        let rgb = png_bytes(2, 1, png::ColorType::Rgb, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(
            decode_png(&rgb),
            Some((2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]))
        );
        let gray = png_bytes(1, 2, png::ColorType::Grayscale, &[7, 9]);
        assert_eq!(
            decode_png(&gray),
            Some((1, 2, vec![7, 7, 7, 255, 9, 9, 9, 255]))
        );
        let ga = png_bytes(1, 1, png::ColorType::GrayscaleAlpha, &[5, 128]);
        assert_eq!(decode_png(&ga), Some((1, 1, vec![5, 5, 5, 128])));
        assert!(decode_png(b"not a png").is_none());
        assert!(decode_png(&[]).is_none());
    }

    #[test]
    fn tags_get_a_layer_only_for_known_elements() {
        let mut l = Layers::default();
        l.elem.insert(("slab", 4), 2);
        l.wall.insert((1, 1), 0);
        assert_eq!(texture::layer_of(l.elem("slab", 4, 11)), Some(2));
        assert_eq!(l.elem("slab", 5, 11), 11);
        assert_eq!(texture::layer_of(l.wall(1, 1)), Some(0));
        assert_eq!(l.wall(1, 0), 0);
        assert_eq!(l.wall(2, 1), 1);
        assert!(!l.is_empty() && Layers::default().is_empty());
    }

    #[test]
    fn slot_keys_change_with_scale_and_source() {
        let row = |scale| MaterialTextureRow {
            material: "Brick".into(),
            tex: ddc_model::MaterialTexture::builtin("brick", scale),
            image: None,
        };
        assert_ne!(row_key(&row(0.5), ""), row_key(&row(0.6), ""));
        assert_eq!(row_key(&row(0.5), ""), row_key(&row(0.5), ""));
    }
}
