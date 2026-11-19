// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Deterministic mutation fuzz harness shared by the ignored fuzz tests (io, app).
//! Budget per target: DDC_FUZZ_SECS (default 3), seed: DDC_FUZZ_SEED (default 1).
#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(n: usize) {
    let now = LIVE.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(now, Relaxed);
}

// SAFETY: forwards to the system allocator and only maintains counters.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            grow(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(l);
        if !p.is_null() {
            grow(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        LIVE.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = System.realloc(p, l, new);
        if !q.is_null() {
            if new >= l.size() {
                grow(new - l.size());
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}

fn peak_reset() {
    PEAK.store(LIVE.load(Relaxed), Relaxed);
}

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

const HUGE: &[&str] = &[
    "99999999999999999999999999",
    "-99999999999999999999",
    "4294967295",
    "4294967296",
    "-2147483648",
    "1e308",
    "-1e308",
    "1e-320",
    "NaN",
    "inf",
    "0",
    "-0",
    "1E999",
    "18446744073709551616",
];
const NEST: &[&str] = &["(", "[", "<<", "#1=(", "{", "\n0\nSECTION\n2\nENTITIES\n"];

/// One random structural or byte-level mutation.
pub fn mutate_once(r: &mut Rng, d: &mut Vec<u8>) {
    if d.is_empty() {
        d.push(r.next() as u8);
        return;
    }
    match r.below(10) {
        0 => {
            let i = r.below(d.len());
            d[i] ^= 1 << r.below(8);
        }
        1 => {
            let i = r.below(d.len());
            d[i] = r.next() as u8;
        }
        2 => d.truncate(r.below(d.len())),
        3 => {
            // splice: copy a chunk from elsewhere
            let a = r.below(d.len());
            let n = r.below((d.len() - a).min(256) + 1);
            let chunk = d[a..a + n].to_vec();
            let at = r.below(d.len() + 1);
            d.splice(at..at, chunk);
        }
        4 => {
            // replace a digit run with a huge number
            let digits: Vec<usize> = (0..d.len()).filter(|&i| d[i].is_ascii_digit()).collect();
            let s = HUGE[r.below(HUGE.len())].as_bytes();
            if digits.is_empty() {
                let at = r.below(d.len() + 1);
                d.splice(at..at, s.iter().copied());
            } else {
                let a = digits[r.below(digits.len())];
                let mut b = a;
                while b < d.len() && d[b].is_ascii_digit() {
                    b += 1;
                }
                d.splice(a..b, s.iter().copied());
            }
        }
        5 => {
            // deep nesting
            let s = NEST[r.below(NEST.len())].repeat(50 + r.below(4000));
            let at = r.below(d.len() + 1);
            d.splice(at..at, s.into_bytes());
        }
        6 => {
            let a = r.below(d.len());
            let n = r.below((d.len() - a).min(512) + 1);
            d.drain(a..a + n);
        }
        7 => {
            let a = r.below(d.len());
            let n = r.below((d.len() - a).min(64) + 1);
            for b in &mut d[a..a + n] {
                *b = 0;
            }
        }
        8 => {
            // duplicate a chunk many times
            let a = r.below(d.len());
            let n = 1 + r.below((d.len() - a).min(64));
            let chunk = d[a..a + n].to_vec();
            let at = r.below(d.len() + 1);
            d.splice(at..at, chunk.repeat(2 + r.below(200)));
        }
        _ => {
            let a = r.below(d.len());
            let n = r.below((d.len() - a).min(8) + 1);
            for b in &mut d[a..a + n] {
                *b = 0xFF;
            }
        }
    }
}

pub fn mutate(r: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut d = seed.to_vec();
    for _ in 0..1 + r.below(4) {
        mutate_once(r, &mut d);
    }
    d
}

fn env_u64(k: &str, def: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(def)
}

pub const MAX_INPUT_SECS: u64 = 10;
pub const MAX_INPUT_BYTES: usize = 1 << 30;

/// Runs `f` on mutated inputs until the time budget is spent. Fails on a panic, on an
/// input that takes > MAX_INPUT_SECS, or on peak live heap > MAX_INPUT_BYTES.
/// Unmutated seeds run first.
pub fn run_target(
    name: &str,
    seeds: &[Vec<u8>],
    gen: fn(&mut Rng, &[u8]) -> Vec<u8>,
    f: fn(&[u8]),
) {
    assert!(!seeds.is_empty(), "{name}: no seeds");
    let secs = env_u64("DDC_FUZZ_SECS", 3);
    let mut r = Rng::new(env_u64("DDC_FUZZ_SEED", 1));
    let end = Instant::now() + Duration::from_secs(secs);
    let (mut n, mut slowest, mut peak) = (0u64, Duration::ZERO, 0usize);
    while Instant::now() < end || n < seeds.len() as u64 {
        let input = if (n as usize) < seeds.len() {
            seeds[n as usize].clone()
        } else {
            let k = r.below(seeds.len());
            gen(&mut r, &seeds[k])
        };
        let (tx, rx) = mpsc::channel();
        let inp = input.clone();
        let base = LIVE.load(Relaxed);
        peak_reset();
        let t = Instant::now();
        let h = std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(move || {
                let ok = std::panic::catch_unwind(|| f(&inp)).is_ok();
                let _ = tx.send(ok);
            })
            .unwrap();
        let res = rx.recv_timeout(Duration::from_secs(MAX_INPUT_SECS));
        let dt = t.elapsed();
        let pk = PEAK.load(Relaxed).saturating_sub(base);
        let bad = match res {
            Ok(true) if pk <= MAX_INPUT_BYTES => None,
            Ok(true) => Some(format!("peak heap {} MiB", pk >> 20)),
            Ok(false) => Some("panic".to_string()),
            Err(_) => Some(format!("no result in {MAX_INPUT_SECS} s")),
        };
        if let Some(why) = bad {
            let p = std::env::temp_dir().join(format!("ddc-fuzz-fail-{name}-{n}.bin"));
            let _ = std::fs::write(&p, &input);
            panic!(
                "{name}: {why} on input #{n} ({} bytes), saved to {}",
                input.len(),
                p.display()
            );
        }
        let _ = h.join();
        slowest = slowest.max(dt);
        peak = peak.max(pk);
        n += 1;
    }
    eprintln!(
        "FUZZ {name}: {n} inputs, slowest {:.0} ms, peak heap {} MiB",
        slowest.as_secs_f64() * 1000.0,
        peak >> 20
    );
}
