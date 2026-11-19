//! One-off: render the kit app icon and favicons to PNG (16..256) into `data/brand/png`.
//! `cargo run -p ddc-app --example brand_icons`; `tools/brand_ico.py` packs the .ico.

fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/brand/png");
    std::fs::create_dir_all(&dir).unwrap();
    let svg = std::fs::read_to_string(dir.join("../design-appicon-512.svg")).unwrap();
    for px in [16u32, 24, 32, 48, 64, 128, 256] {
        let img = ddc_ui::icons::rasterize_svg(&svg, px).unwrap();
        let rgba: Vec<u8> = img
            .pixels
            .iter()
            .flat_map(|p| {
                let a = p.a() as u32;
                let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 };
                [un(p.r()), un(p.g()), un(p.b()), p.a()]
            })
            .collect();
        let f = std::fs::File::create(dir.join(format!("design-appicon-{px}.png"))).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(f), px, px);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgba).unwrap();
    }
}
