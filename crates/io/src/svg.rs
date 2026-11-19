//! SVG export of a drawing or sheet (docs/IO4.md): millimetre page, one group per layer
//! (hidden layers stay in the file with display none), real line weights, even-odd hatches
//! and fills, text with rotation, logos as embedded PNG.

use std::fmt::Write as _;

use base64::Engine;

use crate::pdf::{Item, PdfImage, Sheet};

#[derive(Clone, Debug)]
pub struct Options {
    /// Thinnest stroke written, mm (hairlines stay visible when printed).
    pub min_weight_mm: f64,
    /// Decimals of coordinates.
    pub decimals: usize,
    /// White page rectangle behind the drawing.
    pub background: bool,
    pub title: String,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            min_weight_mm: 0.05,
            decimals: 3,
            background: true,
            title: String::new(),
        }
    }
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => o.push(' '),
            c => o.push(c),
        }
    }
    o
}

/// Layer name as an XML id: letters, digits, '-', '_' and '.', starting with a letter.
fn id_of(k: usize, name: &str) -> String {
    let body: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect();
    format!("layer{k}_{body}")
}

fn rgb(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn png(img: &PdfImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        w.write_image_data(&img.rgba).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

struct Out<'a> {
    s: String,
    h: f64,
    opt: &'a Options,
}

impl Out<'_> {
    fn n(&self, x: f64) -> String {
        let p = 10f64.powi(self.opt.decimals as i32);
        let r = (x * p).round() / p;
        let r = if r == 0.0 { 0.0 } else { r };
        format!("{r}")
    }
    /// Sheet point (Y up) as SVG coordinates (Y down).
    fn pt(&self, p: [f64; 2]) -> String {
        format!("{},{}", self.n(p[0]), self.n(self.h - p[1]))
    }
    fn item(&mut self, it: &Item) -> Result<(), String> {
        match it {
            Item::Polyline {
                pts,
                closed,
                weight_mm,
                rgb: c,
                ..
            } => {
                let tag = if *closed { "polygon" } else { "polyline" };
                let points: Vec<String> = pts.iter().map(|p| self.pt(*p)).collect();
                let w = weight_mm.max(self.opt.min_weight_mm);
                let _ = writeln!(
                    self.s,
                    "<{tag} points=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"{}\"/>",
                    points.join(" "),
                    rgb(*c),
                    self.n(w)
                );
            }
            Item::Fill { rings, rgb: c, .. } => {
                let mut d = String::new();
                for r in rings.iter().filter(|r| r.len() >= 3) {
                    let _ = write!(d, "M{}", self.pt(r[0]));
                    for p in &r[1..] {
                        let _ = write!(d, " L{}", self.pt(*p));
                    }
                    d.push_str(" Z ");
                }
                if !d.is_empty() {
                    let _ = writeln!(self.s, "<path d=\"{}\" fill=\"{}\" fill-rule=\"evenodd\" stroke=\"none\"/>", d.trim_end(), rgb(*c));
                }
            }
            Item::Text {
                pos,
                height_mm,
                rotation,
                string,
                rgb: c,
                ..
            } => {
                let (x, y) = (self.n(pos[0]), self.n(self.h - pos[1]));
                let rot = if rotation.abs() > 1e-9 {
                    format!(" transform=\"rotate({} {x} {y})\"", self.n(-rotation))
                } else {
                    String::new()
                };
                let _ = writeln!(
                    self.s,
                    "<text x=\"{x}\" y=\"{y}\" font-size=\"{}\" fill=\"{}\"{rot} xml:space=\"preserve\">{}</text>",
                    self.n(*height_mm),
                    rgb(*c),
                    esc(string)
                );
            }
            Item::Image { rect, image, .. } => {
                if image.width == 0 || image.height == 0 || image.rgba.len() != (image.width * image.height * 4) as usize {
                    return Err(format!("image {}x{} with {} bytes", image.width, image.height, image.rgba.len()));
                }
                let b64 = base64::engine::general_purpose::STANDARD.encode(png(image)?);
                let _ = writeln!(
                    self.s,
                    "<image x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" preserveAspectRatio=\"none\" href=\"data:image/png;base64,{b64}\"/>",
                    self.n(rect[0]),
                    self.n(self.h - rect[1] - rect[3]),
                    self.n(rect[2]),
                    self.n(rect[3])
                );
            }
        }
        Ok(())
    }
}

fn layer_of(it: &Item) -> usize {
    match it {
        Item::Polyline { layer, .. } | Item::Text { layer, .. } | Item::Fill { layer, .. } | Item::Image { layer, .. } => *layer,
    }
}

/// The SVG document of a sheet. Items keep their order inside a layer; layers are written
/// in sheet order, and items with an unknown layer go to a last group.
pub fn write(sheet: &Sheet, opt: &Options) -> Result<String, String> {
    let (w, h) = (sheet.width_mm, sheet.height_mm);
    if !(w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite()) {
        return Err(format!("sheet size {w} x {h} mm"));
    }
    let mut o = Out {
        s: String::new(),
        h,
        opt,
    };
    let (ws, hs) = (o.n(w), o.n(h));
    let _ = writeln!(o.s, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    let _ = writeln!(
        o.s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" version=\"1.1\" width=\"{ws}mm\" height=\"{hs}mm\" viewBox=\"0 0 {ws} {hs}\">"
    );
    if !opt.title.is_empty() {
        let _ = writeln!(o.s, "<title>{}</title>", esc(&opt.title));
    }
    if opt.background {
        let _ = writeln!(o.s, "<rect x=\"0\" y=\"0\" width=\"{ws}\" height=\"{hs}\" fill=\"#ffffff\"/>");
    }
    let n = sheet.layers.len();
    for k in 0..=n {
        let items: Vec<&Item> = sheet
            .items
            .iter()
            .filter(|it| {
                let l = layer_of(it);
                if k < n { l == k } else { l >= n }
            })
            .collect();
        if k == n && items.is_empty() {
            break;
        }
        let (name, visible) = match sheet.layers.get(k) {
            Some(l) => (l.name.as_str(), l.visible),
            None => ("unassigned", true),
        };
        let hide = if visible { "" } else { " display=\"none\"" };
        let _ = writeln!(
            o.s,
            "<g id=\"{}\" data-layer=\"{}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"{hide}>",
            id_of(k, name),
            esc(name)
        );
        for it in items {
            o.item(it)?;
        }
        o.s.push_str("</g>\n");
    }
    o.s.push_str("</svg>\n");
    Ok(o.s)
}

pub fn write_file(sheet: &Sheet, opt: &Options, path: &std::path::Path) -> Result<(), String> {
    let s = write(sheet, opt)?;
    crate::fsutil::replace_durably_bytes(path, s.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::pdf::SheetLayer;

    fn sheet() -> Sheet {
        let layer = |name: &str, visible| SheetLayer {
            name: name.into(),
            visible,
        };
        let mut items = vec![];
        // a 10 x 10 grid of lines on layer 0, frame on layer 1, a hatched hole on layer 2
        for i in 0..10 {
            let x = 20.0 + i as f64 * 5.0;
            items.push(Item::Polyline {
                pts: vec![[x, 20.0], [x, 70.0]],
                closed: false,
                weight_mm: 0.0,
                rgb: [0, 0, 0],
                layer: 0,
            });
        }
        items.push(Item::Polyline {
            pts: vec![[5.0, 5.0], [415.0, 5.0], [415.0, 292.0], [5.0, 292.0]],
            closed: true,
            weight_mm: 0.7,
            rgb: [10, 20, 30],
            layer: 1,
        });
        items.push(Item::Fill {
            rings: vec![
                vec![[100.0, 100.0], [200.0, 100.0], [200.0, 200.0], [100.0, 200.0]],
                vec![[140.0, 140.0], [160.0, 140.0], [160.0, 160.0]],
            ],
            rgb: [200, 200, 200],
            layer: 2,
        });
        items.push(Item::Text {
            pos: [30.0, 10.0],
            height_mm: 3.5,
            rotation: 90.0,
            string: "A<1> & \"B\"".into(),
            rgb: [0, 0, 0],
            layer: 1,
        });
        items.push(Item::Image {
            rect: [300.0, 10.0, 20.0, 10.0],
            image: Arc::new(PdfImage {
                width: 2,
                height: 1,
                rgba: vec![255, 0, 0, 255, 0, 0, 255, 128],
            }),
            layer: 1,
        });
        items.push(Item::Polyline {
            pts: vec![[1.0, 1.0], [2.0, 2.0]],
            closed: false,
            weight_mm: 0.25,
            rgb: [255, 0, 0],
            layer: 9,
        });
        Sheet {
            width_mm: 420.0,
            height_mm: 297.0,
            layers: vec![layer("Grid", true), layer("Frame & title", true), layer("Hatch", false)],
            items,
        }
    }

    #[test]
    fn valid_xml_with_layers_and_line_count() {
        let sh = sheet();
        let s = write(&sh, &Options::default()).unwrap();
        let doc = roxmltree::Document::parse(&s).unwrap();
        let root = doc.root_element();
        assert_eq!(root.tag_name().name(), "svg");
        assert_eq!(root.tag_name().namespace(), Some("http://www.w3.org/2000/svg"));
        assert_eq!(root.attribute("width"), Some("420mm"));
        assert_eq!(root.attribute("viewBox"), Some("0 0 420 297"));
        let lines = sh.items.iter().filter(|i| matches!(i, Item::Polyline { .. })).count();
        let strokes = doc
            .descendants()
            .filter(|n| matches!(n.tag_name().name(), "polyline" | "polygon"))
            .count();
        assert_eq!(strokes, lines);
        let groups: Vec<_> = doc.descendants().filter(|n| n.has_tag_name("g")).collect();
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[1].attribute("data-layer"), Some("Frame & title"));
        assert_eq!(groups[2].attribute("display"), Some("none"));
        assert_eq!(groups[3].attribute("data-layer"), Some("unassigned"));
        // weights: hairline raised to the minimum, frame kept
        assert_eq!(groups[0].first_element_child().unwrap().attribute("stroke-width"), Some("0.05"));
        let frame = groups[1].first_element_child().unwrap();
        assert_eq!(frame.tag_name().name(), "polygon");
        assert_eq!(frame.attribute("stroke-width"), Some("0.7"));
        assert_eq!(frame.attribute("points").unwrap().split(' ').next(), Some("5,292"));
        // hatch: one even-odd path with two sub-paths
        let path = groups[2].first_element_child().unwrap();
        assert_eq!(path.attribute("fill-rule"), Some("evenodd"));
        assert_eq!(path.attribute("d").unwrap().matches('M').count(), 2);
        let text = doc.descendants().find(|n| n.has_tag_name("text")).unwrap();
        assert_eq!(text.text(), Some("A<1> & \"B\""));
        assert_eq!(text.attribute("transform"), Some("rotate(-90 30 287)"));
        let img = doc.descendants().find(|n| n.has_tag_name("image")).unwrap();
        assert_eq!(img.attribute("y"), Some("277"));
        assert!(img.attribute("href").unwrap().starts_with("data:image/png;base64,iVBORw0KGgo"));
    }

    #[test]
    fn refuses_bad_size_and_image() {
        let mut sh = sheet();
        sh.width_mm = 0.0;
        assert!(write(&sh, &Options::default()).is_err());
        let mut sh = sheet();
        sh.items.push(Item::Image {
            rect: [0.0, 0.0, 1.0, 1.0],
            image: Arc::new(PdfImage {
                width: 2,
                height: 2,
                rgba: vec![0; 4],
            }),
            layer: 0,
        });
        assert!(write(&sh, &Options::default()).is_err());
    }
}
