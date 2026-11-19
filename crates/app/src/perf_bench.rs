//! Performance bench, one JSON line per run (`docs/design/PERF_3D.md`, «Measured»):
//! `ddc-app --synthetic N | --bench FILE|synthetic:N [--frames N] [--size WxH] [--bench-window]
//! [--bench-jsonl OUT]`. Headless: offscreen frames waited on the GPU, no readback. Window: the
//! real swap chain without vsync, frame = time between presents.

use ddc_render::perf::FrameStats;
use ddc_render::synth::{BenchReport, Phase};
use ddc_render::OrbitCamera;
use serde_json::{json, Value};
use std::io::Write as _;
use std::sync::OnceLock;
use web_time::Instant;

pub const SCHEMA: &str = "ddc-perf-bench/1";

static START: OnceLock<Instant> = OnceLock::new();

/// First thing in `main`: the zero of `cold_first_frame_ms`.
pub fn mark_start() {
    START.get_or_init(Instant::now);
}

pub fn since_start_ms() -> f64 {
    START
        .get()
        .map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0)
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Sorted percentile `q` (0..=100) of `ms`; 0 for none.
pub fn percentile(ms: &mut [f64], q: usize) -> f64 {
    if ms.is_empty() {
        return 0.0;
    }
    ms.sort_by(f64::total_cmp);
    ms[(ms.len() * q / 100).min(ms.len() - 1)]
}

fn stats_json(s: &FrameStats) -> Value {
    json!({
        "draws": s.draws, "triangles": s.triangles, "drawn": s.drawn, "chunks": s.chunks,
        "gpu_mb": s.gpu_bytes >> 20,
    })
}

fn phase_json(p: &Phase) -> Value {
    json!({
        "frames": p.frames, "avg_ms": r2(p.avg_ms), "p50_ms": r2(p.p50_ms), "p95_ms": r2(p.p95_ms),
        "cpu_p50_ms": r2(p.cpu_p50_ms), "cpu_p95_ms": r2(p.cpu_p95_ms),
        "gpu_wait_p50_ms": r2((p.p50_ms - p.cpu_p50_ms).max(0.0)),
        "last": stats_json(&p.last),
    })
}

/// What was measured, besides the frames.
pub struct Run<'a> {
    pub model: &'a str,
    pub elements: usize,
    pub triangles: usize,
    pub open_ms: f64,
    /// Process start to the call of the bench (load included).
    pub pre_ms: f64,
    pub size: (u32, u32),
}

fn base(mode: &str, run: &Run) -> serde_json::Map<String, Value> {
    let unix_ms = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let v = json!({
        "schema": SCHEMA, "mode": mode, "model": run.model, "elements": run.elements,
        "triangles": run.triangles, "size": [run.size.0, run.size.1], "open_ms": r2(run.open_ms),
        "version": env!("CARGO_PKG_VERSION"), "unix_ms": unix_ms,
    });
    match v {
        Value::Object(m) => m,
        _ => unreachable!("object literal"),
    }
}

/// The headless line.
pub fn headless_line(run: &Run, r: &BenchReport) -> Value {
    let mut m = base("headless", run);
    let cold = run.pre_ms + r.device_ms + r.setup_ms + r.first_frame_ms;
    let extra = json!({
        "adapter": r.adapter, "backend": r.backend, "device_ms": r2(r.device_ms),
        "setup_ms": r2(r.setup_ms), "first_frame_ms": r2(r.first_frame_ms),
        "cold_first_frame_ms": r2(cold), "settle_ms": r2(r.settle_ms),
        "settle_frames": r.settle_frames, "moving": phase_json(&r.moving),
        "still": phase_json(&r.still),
        "peak_memory_bytes": crate::headless_file::peak_memory(),
    });
    if let Value::Object(e) = extra {
        m.extend(e);
    }
    Value::Object(m)
}

/// Appends `v` as one line to `path`.
pub fn append(path: &str, v: &Value) -> Result<(), String> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{path}: {e}"))?;
    writeln!(f, "{v}").map_err(|e| format!("{path}: {e}"))
}

/// Windowed run: still frames until the renderer settles (bounded), then an orbit of `frames`
/// frames; the time between two `tick`s is one presented frame.
pub struct WinBench {
    pub frames: u32,
    pub jsonl: Option<String>,
    pub model: String,
    pub elements: usize,
    pub triangles: usize,
    pub open_ms: f64,
    pub adapter: String,
    ticks: u32,
    last: Option<Instant>,
    cold_ms: f64,
    first_ms: f64,
    settle_ms: f64,
    settle_start: Option<Instant>,
    orbit: Vec<f64>,
    done: bool,
}

/// Still frames allowed before the orbit starts anyway.
const WIN_SETTLE_MAX: u32 = 600;

impl WinBench {
    pub fn new(model: String, elements: usize, triangles: usize, open_ms: f64, frames: u32) -> Self {
        Self {
            frames: frames.max(1),
            jsonl: None,
            model,
            elements,
            triangles,
            open_ms,
            adapter: String::new(),
            ticks: 0,
            last: None,
            cold_ms: 0.0,
            first_ms: 0.0,
            settle_ms: 0.0,
            settle_start: None,
            orbit: vec![],
            done: false,
        }
    }

    /// Call at the start of every update; moves `cam` during the orbit. `refining`: the renderer
    /// still refines a still camera. Returns the finished line once.
    pub fn tick(&mut self, cam: &mut OrbitCamera, refining: bool, size: (u32, u32)) -> Option<Value> {
        if self.done {
            return None;
        }
        let now = Instant::now();
        let dt = self.last.map(|l| (now - l).as_secs_f64() * 1000.0);
        self.last = Some(now);
        self.ticks += 1;
        // tick 2 starts after the first frame was presented
        if self.ticks == 2 {
            self.cold_ms = since_start_ms();
            self.first_ms = dt.unwrap_or(0.0);
            self.settle_start = Some(now);
        }
        if self.ticks < 2 {
            return None;
        }
        let settling = self.orbit.is_empty() && self.settle_ms == 0.0;
        if settling {
            if refining && self.ticks < WIN_SETTLE_MAX {
                return None;
            }
            self.settle_ms = self
                .settle_start
                .map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0)
                .max(f64::MIN_POSITIVE);
            cam.orbit(0.5, 0.0);
            self.orbit.push(f64::NAN);
            return None;
        }
        if let Some(d) = dt {
            if let Some(slot) = self.orbit.last_mut().filter(|v| v.is_nan()) {
                *slot = d;
            } else {
                self.orbit.push(d);
            }
        }
        if self.orbit.len() as u32 >= self.frames {
            self.done = true;
            return Some(self.line(size));
        }
        cam.orbit(0.5, 0.0);
        None
    }

    fn line(&self, size: (u32, u32)) -> Value {
        let run = Run {
            model: &self.model,
            elements: self.elements,
            triangles: self.triangles,
            open_ms: self.open_ms,
            pre_ms: 0.0,
            size,
        };
        let mut m = base("window", &run);
        let mut ms: Vec<f64> = self.orbit.iter().copied().filter(|v| v.is_finite()).collect();
        let n = ms.len();
        let avg = if n > 0 { ms.iter().sum::<f64>() / n as f64 } else { 0.0 };
        let p50 = percentile(&mut ms, 50);
        let p95 = percentile(&mut ms, 95);
        let extra = json!({
            "adapter": self.adapter, "first_frame_ms": r2(self.first_ms),
            "cold_first_frame_ms": r2(self.cold_ms), "settle_ms": r2(self.settle_ms),
            "moving": {"frames": n, "avg_ms": r2(avg), "p50_ms": r2(p50), "p95_ms": r2(p95)},
            "peak_memory_bytes": crate::headless_file::peak_memory(),
        });
        if let Value::Object(e) = extra {
            m.extend(e);
        }
        Value::Object(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_picks_inside_the_samples() {
        let mut v: Vec<f64> = (1..=20).rev().map(f64::from).collect();
        assert_eq!(percentile(&mut v, 50), 11.0);
        assert_eq!(percentile(&mut v, 95), 20.0);
        assert_eq!(percentile(&mut [], 95), 0.0);
    }

    #[test]
    fn window_bench_settles_then_orbits_and_reports_once() {
        let mut b = WinBench::new("m".into(), 3, 30, 1.5, 5);
        let mut cam = OrbitCamera::default();
        let yaw0 = cam.yaw;
        let mut line = None;
        for i in 0..40 {
            // refines for the first 4 ticks
            if let Some(v) = b.tick(&mut cam, i < 4, (640, 480)) {
                assert!(line.is_none(), "one line only");
                line = Some(v);
            }
        }
        let v = line.expect("a line");
        assert_eq!(v["schema"], SCHEMA);
        assert_eq!(v["mode"], "window");
        assert_eq!(v["moving"]["frames"], 5);
        assert!(v["moving"]["p95_ms"].as_f64().is_some_and(|p| p >= 0.0));
        assert!(v["settle_ms"].as_f64().is_some_and(|s| s > 0.0));
        assert_ne!(cam.yaw, yaw0, "the orbit moved the camera");
        assert!(b.tick(&mut cam, false, (640, 480)).is_none());
    }

    #[test]
    fn headless_line_has_the_gate_fields() {
        let r = BenchReport {
            device_ms: 10.0,
            setup_ms: 20.0,
            first_frame_ms: 30.0,
            ..Default::default()
        };
        let run = Run {
            model: "synthetic:10",
            elements: 10,
            triangles: 1000,
            open_ms: 1.0,
            pre_ms: 5.0,
            size: (64, 64),
        };
        let v = headless_line(&run, &r);
        assert_eq!(v["cold_first_frame_ms"], 65.0);
        assert_eq!(v["mode"], "headless");
        for k in ["p50_ms", "p95_ms", "cpu_p50_ms", "gpu_wait_p50_ms"] {
            assert!(v["moving"].get(k).is_some(), "{k}");
        }
        // one line: no newline inside
        assert!(!v.to_string().contains('\n'));
    }
}
