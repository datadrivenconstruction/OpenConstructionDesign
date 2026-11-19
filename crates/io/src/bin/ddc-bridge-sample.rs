// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Reference bridge: ignores the input content and streams a synthetic two-storey project
//! (millimetres) through protocol v1. `--fault <mode>` produces broken streams for host tests;
//! `--walls N` adds N free-standing walls for streaming tests; `--fault edit` changes the source
//! for re-import tests.

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static DONE: AtomicBool = AtomicBool::new(false);

use ddc_bridge_proto::*;

fn s(x: &str) -> String {
    x.to_string()
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<P2> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

fn wall(guid: &str, level: &str, t: &str, a: P2, b: P2, top: Top) -> Element {
    Element::Wall {
        guid: s(guid),
        level: s(level),
        wall_type: s(t),
        axis: Curve::Line { a, b },
        base_offset: 0.0,
        top,
        offset0: None,
    }
}

fn layer(m: &str, t: f64) -> Vec<Layer> {
    vec![Layer {
        material: s(m),
        thickness: t,
    }]
}

fn project() -> (Vec<Frame>, Vec<Vec<Element>>, Vec<Frame>) {
    let head = vec![
        Frame::Units(Units { length_to_m: 0.001 }),
        Frame::Levels(vec![
            Level {
                guid: s("lvl-1"),
                name: s("Level 1"),
                elevation: 0.0,
            },
            Level {
                guid: s("lvl-2"),
                name: s("Level 2"),
                elevation: 3000.0,
            },
        ]),
        Frame::Grids(vec![Grid {
            guid: s("grid-a"),
            name: s("A"),
            a: (0.0, -1000.0),
            b: (0.0, 7000.0),
        }]),
        Frame::Materials(vec![
            Material {
                guid: s("mat-masonry"),
                name: s("masonry"),
                color: 0xb0603aff,
            },
            Material {
                guid: s("mat-concrete"),
                name: s("concrete"),
                color: 0x999999ff,
            },
        ]),
        Frame::Types(vec![
            Type::Wall {
                guid: s("wt-200"),
                name: s("Masonry 200"),
                layers: layer("mat-masonry", 200.0),
            },
            Type::Wall {
                guid: s("wt-150"),
                name: s("Concrete 150"),
                layers: layer("mat-concrete", 150.0),
            },
            Type::Slab {
                guid: s("st-250"),
                name: s("Slab 250"),
                layers: layer("mat-concrete", 250.0),
            },
            Type::Family {
                guid: s("ft-door"),
                category: s("door"),
                name: s("Single 900"),
                params: vec![(s("fire_rating"), Value::Text(s("EI30")))],
            },
            Type::Family {
                guid: s("ft-window"),
                category: s("window"),
                name: s("Casement 1200"),
                params: vec![(s("u_value"), Value::Real(1.1))],
            },
        ]),
    ];
    let to2 = || Top::Level {
        level: s("lvl-2"),
        offset: 0.0,
    };
    let h3 = || Top::Unconnected { height: 3000.0 };
    let r = rect(0.0, 0.0, 10000.0, 6000.0);
    let mut l1: Vec<Element> = (0..4)
        .map(|i| {
            wall(
                &format!("w1-{i}"),
                "lvl-1",
                "wt-200",
                r[i],
                r[(i + 1) % 4],
                to2(),
            )
        })
        .collect();
    l1.push(wall(
        "w1-part",
        "lvl-1",
        "wt-200",
        (4000.0, 0.0),
        (4000.0, 6000.0),
        to2(),
    ));
    let mut l2: Vec<Element> = (0..4)
        .map(|i| {
            wall(
                &format!("w2-{i}"),
                "lvl-2",
                "wt-200",
                r[i],
                r[(i + 1) % 4],
                h3(),
            )
        })
        .collect();
    l2.push(Element::Wall {
        guid: s("w2-arc"),
        level: s("lvl-2"),
        wall_type: s("wt-150"),
        axis: Curve::Arc {
            a: (2000.0, 2000.0),
            mid: (5000.0, 3500.0),
            b: (8000.0, 2000.0),
        },
        base_offset: 0.0,
        top: h3(),
        offset0: None,
    });
    let rest = vec![
        Element::Opening {
            guid: s("door-1"),
            host: s("w1-part"),
            kind: OpeningKind::Door,
            family_type: Some(s("ft-door")),
            along: 2000.0,
            width: 900.0,
            height: 2100.0,
            sill: 0.0,
            swing_side: true,
            swing_hinge: false,
        },
        Element::Opening {
            guid: s("win-1"),
            host: s("w1-0"),
            kind: OpeningKind::Window,
            family_type: Some(s("ft-window")),
            along: 6000.0,
            width: 1200.0,
            height: 1500.0,
            sill: 900.0,
            swing_side: false,
            swing_hinge: false,
        },
        Element::Slab {
            guid: s("slab-2"),
            level: s("lvl-2"),
            slab_type: s("st-250"),
            offset: 0.0,
            outer: r.clone(),
            holes: vec![rect(1000.0, 1000.0, 2000.0, 2000.0)],
        },
        Element::Room {
            guid: s("room-101"),
            level: s("lvl-1"),
            name: s("Office"),
            number: s("101"),
            height: 2800.0,
            boundary: rect(100.0, 100.0, 3900.0, 5900.0),
            point: None,
        },
        Element::Room {
            guid: s("room-102"),
            level: s("lvl-1"),
            name: s("Hall"),
            number: s("102"),
            height: 2800.0,
            boundary: rect(4100.0, 100.0, 9900.0, 5900.0),
            point: None,
        },
        Element::Generic {
            guid: s("mesh-1"),
            level: Some(s("lvl-1")),
            category: s("furniture"),
            name: s("Cabinet"),
            positions: (0..8)
                .map(|i| {
                    [
                        1000.0 + 600.0 * (i & 1) as f64,
                        1000.0 + 400.0 * (i >> 1 & 1) as f64,
                        900.0 * (i >> 2 & 1) as f64,
                    ]
                })
                .collect(),
            indices: vec![
                0, 2, 1, 1, 2, 3, 4, 5, 6, 5, 7, 6, 0, 1, 4, 1, 5, 4, 2, 6, 3, 3, 6, 7, 0, 4, 2, 2,
                4, 6, 1, 3, 5, 3, 7, 5,
            ],
        },
    ];
    let tail = vec![
        Frame::Properties(vec![
            Property {
                owner: s("w1-0"),
                key: s("fire_rating"),
                value: Value::Text(s("REI60")),
                unit: s(""),
            },
            Property {
                owner: s("mesh-1"),
                key: s("mass"),
                value: Value::Real(45.0),
                unit: s("kg"),
            },
        ]),
        Frame::Warnings(vec![Warning {
            code: s("sample"),
            message: s("synthetic project"),
            element: None,
        }]),
    ];
    (head, vec![l1, l2, rest], tail)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    if arg("--protocol").as_deref() != Some("1") {
        eprintln!("unsupported protocol");
        std::process::exit(2);
    }
    let Some(input) = arg("--input") else {
        eprintln!("--input required");
        std::process::exit(2);
    };
    if std::fs::metadata(&input).is_err() {
        eprintln!("cannot read input");
        std::process::exit(2);
    }
    let fault = arg("--fault").unwrap_or_default();
    let extra: usize = arg("--walls").and_then(|n| n.parse().ok()).unwrap_or(0);
    if fault != "hang" {
        let crash_end = fault == "crash-after-end";
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::stdin().lock().read_line(&mut line);
            // cancel line, or stdin closed before End; the crash fault must not lose the race
            let done = DONE.load(Ordering::SeqCst);
            std::process::exit(if done && crash_end { 4 } else if done { 0 } else { 130 });
        });
    }
    let out = std::io::stdout().lock();
    let mut w = FrameWriter::new(std::io::BufWriter::new(out)).expect("stdout");
    let res = emit(&mut w, &fault, extra);
    if res.is_err() {
        std::process::exit(1);
    }
    if fault == "crash-after-end" {
        std::process::exit(4);
    }
}

fn emit<W: Write>(w: &mut FrameWriter<W>, fault: &str, extra: usize) -> std::io::Result<()> {
    w.write(&Frame::Hello(Hello {
        protocol: PROTOCOL,
        bridge: s("ddc-bridge-sample"),
        version: s(env!("CARGO_PKG_VERSION")),
        formats: vec![s("rvt"), s("rfa")],
        capabilities: vec![s("progress"), s("cancel")],
    }))?;
    let (head, mut batches, tail) = project();
    if fault == "edit" {
        // re-import of a changed source: arc reshaped; extra walls: x-0 moved, x-1 removed
        for e in batches.iter_mut().flatten() {
            if let Element::Wall {
                guid,
                axis: Curve::Arc { mid, .. },
                ..
            } = e
            {
                if guid == "w2-arc" {
                    mid.1 = 4000.0;
                }
            }
        }
    }
    for f in &head {
        w.write(f)?;
    }
    if fault == "bad-host" {
        if let Some(Element::Opening { host, .. }) = batches[2].first_mut() {
            *host = s("no-such-wall");
        }
    }
    if extra > 0 {
        let n = extra.div_ceil(4096);
        for b in 0..n {
            let batch = (b * 4096..((b + 1) * 4096).min(extra))
                .filter(|&i| !(fault == "edit" && i == 1))
                .map(|i| {
                    let (x, y) = (
                        (i % 1000) as f64 * 2000.0,
                        20000.0 + (i / 1000) as f64 * 2000.0,
                    );
                    let x = if fault == "edit" && i == 0 {
                        x + 500.0
                    } else {
                        x
                    };
                    wall(
                        &format!("x-{i}"),
                        "lvl-1",
                        "wt-200",
                        (x, y),
                        (x + 1000.0, y),
                        Top::Unconnected { height: 3000.0 },
                    )
                })
                .collect();
            batches.push(batch);
        }
    }
    let total: u64 = batches.iter().map(|b| b.len() as u64).sum();
    let mut done = 0u64;
    for (k, b) in batches.into_iter().enumerate() {
        if fault == "slow" {
            std::thread::sleep(Duration::from_millis(300));
        }
        done += b.len() as u64;
        let f = Frame::Elements(b);
        if k == 1 {
            match fault {
                "truncate" => {
                    let body = encode(&f);
                    w.inner().write_all(&(body.len() as u32).to_le_bytes())?;
                    w.inner().write_all(&body[..body.len() / 2])?;
                    return w.inner().flush();
                }
                "oversize" => {
                    w.inner().write_all(&0xffff_fff0u32.to_le_bytes())?;
                    return w.inner().flush();
                }
                "malformed" => {
                    let mut body = encode(&f);
                    body[5] = 0x7f; // first element tag
                    return w.write_raw(&body);
                }
                "crash" => {
                    w.inner().flush()?;
                    std::process::exit(3);
                }
                "hang" => {
                    w.inner().flush()?;
                    std::thread::sleep(Duration::from_secs(3600));
                }
                _ => {}
            }
        }
        w.write(&f)?;
        w.write(&Frame::Progress(Progress {
            done,
            total,
            phase: s("elements"),
        }))?;
    }
    for f in &tail {
        w.write(f)?;
    }
    let elements = if fault == "count" { total + 1 } else { total };
    DONE.store(true, Ordering::SeqCst);
    w.write(&Frame::End(Stats {
        elements,
        warnings: 1,
    }))
}
