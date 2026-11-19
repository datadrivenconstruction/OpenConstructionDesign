//! Prints every storey of a project brief as rooms with their boxes and areas, plus the gate
//! findings: `cargo run -p ddc-gen --example plan_dump -- brief.json`.

use ddc_gen::project::{build_project, check_project, parse_spec};

fn main() {
    let path = std::env::args().nth(1).expect("brief.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = match build_project(&s) {
        Ok(p) => p,
        Err(e) => {
            println!("REFUSED {e}");
            return;
        }
    };
    println!("footprint {:.2} x {:.2}", p.footprint.0, p.footprint.1);
    for st in &p.storeys {
        println!("-- {} ({} openings, {} columns)", st.name, st.layout.openings.len(), st.layout.columns.len());
        for (g, r) in st.layout.rooms.iter().zip(&st.rooms) {
            let (xs, ys): (Vec<f64>, Vec<f64>) = g.polygon.iter().copied().unzip();
            let f = |v: &[f64], max: bool| {
                v.iter().copied().fold(if max { f64::MIN } else { f64::MAX }, |a, b| if max { a.max(b) } else { a.min(b) })
            };
            let (x0, x1, y0, y1) = (f(&xs, false), f(&xs, true), f(&ys, false), f(&ys, true));
            println!(
                "  {:6} {:?} {:<18} x {:5.2}..{:5.2} y {:5.2}..{:5.2} = {:6.2} m2 asked {:?}",
                r.number,
                r.kind,
                g.name,
                x0,
                x1,
                y0,
                y1,
                (x1 - x0) * (y1 - y0),
                r.asked
            );
        }
    }
    let g = check_project(&s, &p);
    println!("failures {:?}\nwarnings {:?}", g.failures, g.warnings);
}
