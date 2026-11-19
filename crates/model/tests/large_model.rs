//! Large-model timing: 1M synthetic elements (walls, openings, slabs, columns).
//! `cargo test --release -p ddc-model --test large_model -- --ignored --nocapture`
//! Gates: open < 3 s, check_integrity < 5 s; prints peak working set (Windows).

use std::time::Instant;

use ddc_model::{LayerSpec, Model};

const WALLS: i64 = 400_000;
const OPENINGS: i64 = 300_000;
const SLABS: i64 = 100_000;
const COLUMNS: i64 = 200_000;

#[cfg(windows)]
fn peak_mb() -> f64 {
    #[repr(C)]
    #[derive(Default)]
    struct Pmc {
        cb: u32,
        faults: u32,
        peak_ws: usize,
        ws: usize,
        a: usize,
        b: usize,
        c: usize,
        d: usize,
        pagefile: usize,
        peak_pagefile: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn K32GetProcessMemoryInfo(h: isize, p: *mut Pmc, cb: u32) -> i32;
        fn GetCurrentProcess() -> isize;
    }
    let mut p = Pmc {
        cb: std::mem::size_of::<Pmc>() as u32,
        ..Default::default()
    };
    // SAFETY: valid out-pointer of the declared size for the current process
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut p, p.cb) };
    p.peak_ws as f64 / 1048576.0
}

#[cfg(not(windows))]
fn peak_mb() -> f64 {
    0.0
}

fn build(path: &str) {
    let mut m = Model::open(path).unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let lay = [LayerSpec {
        material: "Concrete".into(),
        thickness: 0.2,
    }];
    let wt = m.add_wall_type("W", &lay).unwrap();
    let st = m.add_slab_type("S", &lay).unwrap();
    let ct = m.add_catalog_member_type("R 400x400").unwrap();
    let db = m.sql();
    db.execute_batch("BEGIN").unwrap();
    {
        let mut w = db
            .prepare("INSERT INTO wall(id,level,wtype,ax,ay,bx,by,base_offset,height,offset0) VALUES (?1,?2,?3,?4,?5,?6,?7,0,3,0)")
            .unwrap();
        for i in 1..=WALLS {
            let (x, y) = ((i % 1000) as f64 * 10.0, (i / 1000) as f64 * 10.0);
            w.execute(rusqlite::params![i, lv, wt, x, y, x + 6.0, y])
                .unwrap();
        }
        let mut o = db
            .prepare("INSERT INTO opening(id,wall,along,width,sill,height,kind) VALUES (?1,?2,1.0,0.9,0,2.1,'door')")
            .unwrap();
        for i in 1..=OPENINGS {
            o.execute(rusqlite::params![i, (i - 1) % WALLS + 1])
                .unwrap();
        }
        let mut s = db
            .prepare("INSERT INTO slab(id,level,stype,offset) VALUES (?1,?2,?3,0)")
            .unwrap();
        let mut v = db
            .prepare("INSERT INTO slab_vertex(slab,ring,idx,x,y) VALUES (?1,0,?2,?3,?4)")
            .unwrap();
        for i in 1..=SLABS {
            s.execute(rusqlite::params![i, lv, st]).unwrap();
            let (x, y) = ((i % 300) as f64 * 20.0, (i / 300) as f64 * 20.0);
            for (k, (dx, dy)) in [(0.0, 0.0), (8.0, 0.0), (8.0, 8.0), (0.0, 8.0)]
                .iter()
                .enumerate()
            {
                v.execute(rusqlite::params![i, k as i64, x + dx, y + dy])
                    .unwrap();
            }
        }
        let mut c = db
            .prepare("INSERT INTO col(id,level,top_level,mtype,structural,x,y,base_offset,top_offset,rotation) VALUES (?1,?2,NULL,?3,1,?4,?5,0,0,0)")
            .unwrap();
        for i in 1..=COLUMNS {
            c.execute(rusqlite::params![
                i,
                lv,
                ct,
                (i % 1000) as f64,
                (i / 1000) as f64
            ])
            .unwrap();
        }
    }
    // derived quantities as regeneration would leave them (values are not checked)
    db.execute_batch(
        "INSERT INTO wall_qty SELECT id,0,'Concrete',1.2,3.6 FROM wall;
         INSERT INTO slab_qty SELECT id,0,'Concrete',64,12.8 FROM slab;
         INSERT INTO member_qty SELECT 'column',id,3,0.48,4.8,0 FROM col;",
    )
    .unwrap();
    db.execute_batch("COMMIT; PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
}

#[test]
#[ignore]
fn one_million_elements_open_and_check() {
    let dir = std::env::temp_dir().join(format!("ddc-large-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("big.ddc");
    let ps = p.to_str().unwrap().to_string();
    let t = Instant::now();
    build(&ps);
    let build_s = t.elapsed().as_secs_f64();
    let size_mb = std::fs::metadata(&p).unwrap().len() as f64 / 1048576.0;
    let t = Instant::now();
    let m = Model::open(&ps).unwrap();
    let open_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let iss = m.check_integrity().unwrap();
    let check_s = t.elapsed().as_secs_f64();
    eprintln!(
        "LARGE elements={} file_mb={size_mb:.0} build_s={build_s:.1} open_s={open_s:.2} check_s={check_s:.2} issues={} peak_mb={:.0}",
        WALLS + OPENINGS + SLABS + COLUMNS,
        iss.len(),
        peak_mb()
    );
    drop(m);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(iss.is_empty(), "{:?}", &iss[..iss.len().min(5)]);
    assert!(open_s < 3.0, "open {open_s:.2} s");
    assert!(check_s < 5.0, "check_integrity {check_s:.2} s");
}
