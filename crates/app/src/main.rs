//! ddc-app: the editor (plan + "Wall", "Door/Window" tools + 3D).
//! ddc-app [--safe-graphics] [--screenshot out.png [--scene empty|room|facade|curtain|panel|slab|section|levels|mass|mep] [--view plan|3d|section:N|elevation:S] [--size WxH] [--hover X,Y] [--scale F] [--layout N|id] [--first-run] [--settings-interface] [--settings-page N|library|ai|about...] [--start-screen] [--lang ru|en] [--job-demo] [--import-demo STATE[:ru|:en]] [--xlsx-preview] [--tool-demo A1..A5]
//!   [--scene overlap|phases] [--phase-filter KEY] [--phases-win] [--params-win] [--schedule-demo] [--ribbon-tab ID] [--crop Y0,Y1] [--status-demo] [--recover-demo] [--autosave-settings] [--dwg-demo underlay|preview|result]
//!   [--sheet-demo canvas|active|print|props|sets]]
//! Demo and benchmark: ddc-app [--rooms N | --walls N] [--continuous] [--frames N --headless-report]
//! Graphics modes: ddc-app --screenshot out.png --style KEY --gfx-level KEY; ddc-app --gfx-bench FILE|synthetic:N [--gfx-csv OUT] [--frames N] [--size WxH]
//! Large-model benchmark: ddc-app --synthetic N [--synthetic-tris T] [--synthetic-quality off|medium|high] [--frames N] [--size WxH]
//! Bench of any model: ddc-app --bench FILE|synthetic:N [--bench-window] [--bench-jsonl OUT] (see perf_bench.rs)

use ddc_app::demo::{self, Demo, LAYER_NAMES};
use ddc_app::editor::Editor;
use ddc_app::gfx::{self, Pick, StartupLog};
use ddc_app::shell::{self, EditorApp};
use ddc_app::ui_kit::{self, Tokens};
use ddc_app::viewport::Viewport;
use ddc_app::wall_tool::{Event, Mode};
use ddc_geom::v2;
use ddc_render::{headless, OrbitCamera};
use web_time::Instant;

struct Args {
    grid: (usize, usize),
    frames: u32,
    synthetic: Option<usize>,
    synthetic_tris: usize,
    synthetic_quality: String,
    bench: Option<String>,
    bench_jsonl: Option<String>,
    bench_window: bool,
    headless: bool,
    continuous: bool,
    demo: bool,
    safe: bool,
    screenshot: Option<String>,
    /// `--reveal all|calendar --reveal-out DIR [--reveal-fps N]`: reveal frames without a window.
    reveal: String,
    reveal_out: Option<String>,
    reveal_fps: u32,
    /// `--reveal-frame N`: only that frame (a shot of the show at one moment).
    reveal_frame: Option<u32>,
    scene: String,
    view3d: bool,
    /// `section:N` (the Nth section, from 1) or `elevation:S` (N/S/E/W or a number).
    view: String,
    size: (u32, u32),
    hover: Option<(f32, f32)>,
    scale: f32,
    theme: String,
}

fn args() -> Args {
    let mut a = Args {
        grid: demo::grid_for_rooms(100),
        frames: 120,
        synthetic: None,
        synthetic_tris: 100,
        synthetic_quality: "medium".into(),
        bench: None,
        bench_jsonl: None,
        bench_window: false,
        headless: false,
        continuous: false,
        demo: false,
        safe: false,
        screenshot: None,
        reveal: String::new(),
        reveal_out: None,
        reveal_fps: 30,
        reveal_frame: None,
        scene: "empty".into(),
        view3d: false,
        view: String::new(),
        size: (1400, 900),
        hover: None,
        scale: 1.0,
        theme: String::new(),
    };
    let v: Vec<String> = std::env::args().skip(1).collect();
    let num = |i: usize| v.get(i + 1).and_then(|s| s.parse::<usize>().ok());
    let val = |i: usize| v.get(i + 1).cloned().unwrap_or_default();
    a.demo = v
        .iter()
        .any(|s| ["--rooms", "--walls", "--headless-report"].contains(&s.as_str()));
    for (i, s) in v.iter().enumerate() {
        match s.as_str() {
            "--rooms" => a.grid = demo::grid_for_rooms(num(i).unwrap_or(100)),
            "--walls" => {
                a.grid = demo::grid_for_walls(num(i).unwrap_or(10_000));
                a.continuous = true;
            }
            "--frames" => a.frames = num(i).unwrap_or(120) as u32,
            "--synthetic" => a.synthetic = Some(num(i).unwrap_or(200_000)),
            "--synthetic-tris" => a.synthetic_tris = num(i).unwrap_or(100),
            "--synthetic-quality" => a.synthetic_quality = val(i),
            "--bench" => a.bench = Some(val(i)),
            "--bench-jsonl" => a.bench_jsonl = Some(val(i)),
            "--bench-window" => a.bench_window = true,
            "--headless-report" => a.headless = true,
            "--continuous" => a.continuous = true,
            "--safe-graphics" => a.safe = true,
            "--screenshot" => a.screenshot = Some(val(i)),
            "--scene" => a.scene = val(i),
            "--reveal" => a.reveal = val(i),
            "--reveal-out" => a.reveal_out = Some(val(i)),
            "--reveal-frame" => a.reveal_frame = num(i).map(|n| n as u32),
            "--reveal-fps" => a.reveal_fps = num(i).unwrap_or(30).clamp(1, 120) as u32,
            "--view" => {
                a.view3d = val(i) == "3d";
                a.view = val(i);
            }
            "--scale" => a.scale = val(i).parse().unwrap_or(1.0),
            "--theme" => a.theme = val(i),
            "--hover" => {
                if let Some((x, y)) = val(i).split_once(',') {
                    a.hover = x.parse().ok().zip(y.parse().ok());
                }
            }
            "--size" => {
                if let Some((w, h)) = val(i).split_once('x') {
                    a.size = (w.parse().unwrap_or(1400), h.parse().unwrap_or(900));
                }
            }
            _ => {}
        }
    }
    a
}

fn native_options(pick: Pick) -> eframe::NativeOptions {
    let (size, pos) = ddc_app::win_geom::start(ddc_app::win_geom::load(), ddc_app::win::screens());
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(ddc_app::brand::PRODUCT)
        .with_icon(std::sync::Arc::new(ddc_app::brand::window_icon()))
        .with_inner_size([size.0, size.1])
        .with_min_inner_size([
            ddc_app::win_geom::MIN.0.min(size.0),
            ddc_app::win_geom::MIN.1.min(size.1),
        ]);
    if let Some(p) = pos {
        viewport = viewport.with_position([p.0, p.1]);
    }
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        depth_buffer: 32,
        multisampling: gfx::window_samples() as u16,
        wgpu_options: gfx::wgpu_options(pick),
        viewport,
        ..Default::default()
    }
}

/// Probe backends in order and pick the first whose device opens. `DDC_GFX_START` skips
/// candidates already failed by a parent process (see `relaunch_next`).
fn pick_backend(safe: bool, log: &mut StartupLog) -> Result<(Pick, String, usize, usize), String> {
    pick_backend_at(safe, 0, log)
}

/// `rung` above 0: a recovery start, on the ladder from that position.
fn pick_backend_at(
    safe: bool,
    rung: u32,
    log: &mut StartupLog,
) -> Result<(Pick, String, usize, usize), String> {
    let env = wgpu_env();
    let cands = if rung > 0 {
        gfx::recovery_ladder().split_off(rung as usize)
    } else {
        gfx::candidates(env, safe)
    };
    let start: usize = std::env::var("DDC_GFX_START")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    log.line(&format!(
        "ddc-app {} ({}) · env WGPU_BACKEND={:?} · safe={safe} · start={start}",
        env!("CARGO_PKG_VERSION"),
        env!("DDC_BUILD"),
        std::env::var("WGPU_BACKEND").ok()
    ));
    // an explicit back end is not second-guessed; otherwise software is the last resort
    let fallback = env.is_none();
    let log = std::cell::RefCell::new(log);
    let mut probe = |p| gfx::probe_device(p, &mut |s| log.borrow_mut().line(&s));
    let r = gfx::pick_from(&cands, start, fallback, &mut probe, &mut |s| {
        log.borrow_mut().line(&s)
    });
    r.map_err(|e| format!("no usable graphics adapter: {e}"))
}

/// Start failure with no window: native box with the reason, log path and next steps, in the
/// UI language; never a silent exit. Returns the error for `main`.
fn fatal_gfx(reason: &str, can_safe: bool, log: &mut StartupLog) -> String {
    log.line(&format!("fatal: {reason}"));
    let argv: Vec<String> = std::env::args().collect();
    let lang = ddc_app::prefs::Prefs::load()
        .startup_lang(&argv, ddc_app::shell::settings_win::system_lang());
    let path = log.path.clone().filter(|p| p.exists());
    let next = ddc_app::gfx_guard::fatal_start(
        lang,
        path.as_deref(),
        reason,
        can_safe,
        &mut gfx::message_box,
    );
    match next {
        ddc_app::gfx_guard::FatalNext::SafeStart => {
            let exe = std::env::current_exe();
            let spawned = exe.and_then(|exe| {
                std::process::Command::new(exe)
                    .args(argv.iter().skip(1).filter(|s| *s != "--safe-graphics"))
                    .arg("--safe-graphics")
                    .env_remove("DDC_GFX_START")
                    .spawn()
            });
            match spawned {
                Ok(_) => std::process::exit(0),
                Err(e) => log.line(&format!("safe start failed: {e}")),
            }
        }
        ddc_app::gfx_guard::FatalNext::OpenLog => {
            if let Some(p) = &path {
                gfx::open_path(p);
            }
        }
        ddc_app::gfx_guard::FatalNext::Close => {}
    }
    reason.to_string()
}

fn wgpu_env() -> Option<ddc_render::wgpu::Backends> {
    std::env::var("WGPU_BACKEND")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| ddc_render::wgpu::Backends::from_comma_list(&s))
}

/// Window creation failed on this backend: run a child on the next one (an event loop cannot be
/// recreated in-process).
fn relaunch_next(next: usize, log: &mut StartupLog) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    log.line(&format!("relaunch with DDC_GFX_START={next}"));
    let st = std::process::Command::new(exe)
        .args(std::env::args().skip(1))
        .env("DDC_GFX_START", next.to_string())
        .status()
        .map_err(|e| e.to_string())?;
    std::process::exit(st.code().unwrap_or(1));
}

/// As `relaunch_next`, on the software adapter.
fn relaunch_safe(log: &mut StartupLog) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    log.line("relaunch with --safe-graphics");
    let st = std::process::Command::new(exe)
        .args(std::env::args().skip(1).filter(|s| s != "--safe-graphics"))
        .arg("--safe-graphics")
        .env_remove("DDC_GFX_START")
        .status()
        .map_err(|e| e.to_string())?;
    std::process::exit(st.code().unwrap_or(1));
}

/// Benchmark on a synthetic model: order and targets are in `docs/design/PERF_3D.md`.
fn synthetic(elements: usize, a: &Args) -> Result<(), String> {
    let spec = ddc_render::synth::SynthSpec {
        elements,
        tris_per_element: a.synthetic_tris,
        ..Default::default()
    };
    let t = web_time::Instant::now();
    let scene = ddc_render::synth::generate(&spec);
    let gen_ms = t.elapsed().as_secs_f64() * 1000.0;
    bench_scene(&format!("synthetic:{elements}"), &scene, elements, gen_ms, a)
}

/// `--bench FILE|synthetic:N`: the model opened as the app opens it, then measured.
fn bench_spec(spec: &str, a: &Args) -> Result<(), String> {
    if let Some(n) = spec.strip_prefix("synthetic:") {
        let n = n.parse().map_err(|_| format!("bad element count: {n}"))?;
        if !a.bench_window {
            return synthetic(n, a);
        }
    }
    let t = web_time::Instant::now();
    let scene = ddc_app::gfx_bench::load(spec)?;
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;
    let elements = scene_elements(&scene);
    if a.bench_window {
        return bench_window(spec, scene, elements, open_ms, a);
    }
    bench_scene(spec, &scene, elements, open_ms, a)
}

fn scene_elements(scene: &ddc_render::Scene) -> usize {
    let mut ids: Vec<u32> = scene.meshes.iter().map(|m| m.element).collect();
    for g in scene.instanced.iter() {
        ids.extend(g.inst.iter().map(|i| i.element));
    }
    ids.sort_unstable();
    ids.dedup();
    ids.len()
}

fn bench_scene(
    model: &str,
    scene: &ddc_render::Scene,
    elements: usize,
    open_ms: f64,
    a: &Args,
) -> Result<(), String> {
    use ddc_render::Quality;
    let gen_ms = open_ms;
    let q = match a.synthetic_quality.as_str() {
        "off" => Quality::Off,
        "high" => Quality::High,
        _ => Quality::Medium,
    };
    // the back end the app itself would try first
    let backends = gfx::candidates(wgpu_env(), a.safe)
        .first()
        .map_or(ddc_render::wgpu::Backends::PRIMARY, |p| p.backends);
    let pre_ms = ddc_app::perf_bench::since_start_ms();
    let r = ddc_render::synth::bench_on(scene, a.size, a.frames, q, backends)?;
    let line = ddc_app::perf_bench::headless_line(
        &ddc_app::perf_bench::Run {
            model,
            elements,
            triangles: scene.triangle_count(),
            open_ms,
            pre_ms,
            size: a.size,
        },
        &r,
    );
    println!("{line}");
    if let Some(p) = &a.bench_jsonl {
        ddc_app::perf_bench::append(p, &line)?;
    }
    println!(
        "synthetic: {} | {}x{} | {elements} elements | {:.1} M tris | {q:?}",
        r.adapter,
        a.size.0,
        a.size.1,
        r.moving.last.total_triangles as f64 / 1e6
    );
    println!(
        "generate {gen_ms:.0} ms | setup {:.0} ms | settle {:.0} ms ({} frames)",
        r.setup_ms, r.settle_ms, r.settle_frames
    );
    for (name, p) in [("moving", &r.moving), ("still", &r.still)] {
        println!(
            "{name}: avg {:.2} ms p95 {:.2} ms | cpu {:.2} ms | draws {} | tris {:.2} M | drawn {}/{} | culled {} small {} occluded {} boxes {} dropped {} | gpu {} / {} MB | evicted {} denied {}",
            p.avg_ms,
            p.p95_ms,
            p.last.cpu_ms,
            p.last.draws,
            p.last.triangles as f64 / 1e6,
            p.last.drawn,
            p.last.chunks,
            p.last.culled,
            p.last.lod_skipped,
            p.last.occluded,
            p.last.boxes,
            p.last.dropped,
            p.last.gpu_bytes >> 20,
            p.last.gpu_budget >> 20,
            p.last.evicted,
            p.last.denied,
        );
    }
    Ok(())
}

fn main() -> Result<(), String> {
    ddc_app::perf_bench::mark_start();
    std::hint::black_box(&ddc_app::build_record::BUILD_RECORD);
    // file associations are set only by an explicit request, never at start
    let argv: Vec<String> = std::env::args().collect();
    if let Some(code) = ddc_app::os_assoc::handle_cli(&argv) {
        std::process::exit(code);
    }
    let reveal_motion = ddc_app::reveal::motion_arg(&argv)?;
    // every envelope path (tools, MCP, design options) reaches the model lowering
    ddc_app::ops_exec::link();
    if let Some(code) = ddc_app::qa_accept::handle(&argv) {
        std::process::exit(code);
    }
    if argv.iter().any(|s| s == "--help" || s == "-h") {
        print!("{}", ddc_app::headless_file::HELP);
        return Ok(());
    }
    match ddc_app::headless_file::parse(&argv) {
        Ok(Some(fa)) => {
            let mut log = StartupLog::open();
            let safe = argv.iter().any(|s| s == "--safe-graphics");
            let code = ddc_app::headless_file::run(&fa, &mut |l, fa| {
                render_loaded(l, fa, safe, &mut log)
            });
            std::process::exit(code);
        }
        Ok(None) => {}
        Err(e) => {
            eprintln!("error: {e}\n\n{}", ddc_app::headless_file::HELP);
            ddc_app::headless_file::report_parse_error(&argv, &e);
            std::process::exit(1);
        }
    }
    let a = args();
    if let Some(spec) = &a.bench {
        return bench_spec(spec, &a);
    }
    if let Some(n) = a.synthetic {
        return synthetic(n, &a);
    }
    let mut log = StartupLog::open();
    let argv1: Vec<String> = std::env::args().collect();
    // scripted runs: no recovery dialog, no session mark, no crash snapshot of their own
    let no_recovery = ddc_app::headless_file::no_recovery(&argv1);
    if !no_recovery {
        shell::recover_win::install_crash_handler(ddc_app::prefs::Prefs::load().autosave_folder());
    }
    let after = |f: &str| {
        argv1
            .iter()
            .position(|s| s == f)
            .and_then(|i| argv1.get(i + 1))
    };
    if let Some(spec) = after("--gfx-bench") {
        let csv = after("--gfx-csv").map(String::as_str);
        return ddc_app::gfx_bench::run(spec, csv, a.size, a.frames);
    }
    if let Some(n) = after("--links-bench") {
        let n = n.parse().map_err(|_| format!("bad link count: {n}"))?;
        let e = after("--links-elems").and_then(|v| v.parse().ok()).unwrap_or(2000);
        return ddc_app::links_ui::bench::run(n, e, a.size);
    }
    if let Some(out) = &a.screenshot {
        return screenshot(&a, out, &mut log);
    }
    if !a.demo {
        // an explicit choice of back end is not second-guessed
        let last = gfx::recovery_ladder().len() as u32 - 1;
        let guard = (!a.safe && wgpu_env().is_none())
            .then(gfx::data_dir)
            .flatten()
            .map(|d| ddc_app::gfx_guard::Guard::begin(&d, last));
        let rung = guard.as_ref().map_or(0, |g| g.rung());
        let crashed = guard.as_ref().is_some_and(|g| g.crashed());
        if let Some(g) = &guard {
            log.line(&format!("graphics start: rung {rung}, {}", g.why().log_text()));
        }
        let why_key = if a.safe {
            "gfx.why.flag"
        } else {
            guard.as_ref().map_or("", |g| {
                ddc_app::gfx_guard::cause_key(&g.sticky().reason)
            })
        };
        if rung > 0 {
            log.line(&format!("recovery start: rung {rung}, crashed={crashed}"));
            // the window's own multisampling is one more thing an old driver can fail on
            gfx::set_window_samples(1);
        }
        let (pick, desc, i, n) = match pick_backend_at(a.safe, rung, &mut log) {
            Ok(v) => v,
            Err(e) => {
                drop(guard);
                // with no explicit back end the software steps were already tried
                let can_safe = !a.safe && wgpu_env().is_some();
                return Err(fatal_gfx(&e, can_safe, &mut log));
            }
        };
        let chosen = gfx::ladder_position(&gfx::recovery_ladder(), pick);
        if let Some(g) = &guard {
            g.chosen(chosen);
        }
        let resets: u32 = std::env::var("DDC_GFX_RESETS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if resets > 0 {
            // the parent may still be exiting: its project and session locks are waited for
            ddc_app::gfx_guard::expect_parent_locks(std::time::Duration::from_secs(5));
        }
        let loss = ddc_app::gfx_guard::LossCtx {
            rung,
            chosen,
            last,
            resets,
            guarded: guard.is_some(),
        };
        let lost2 = gfx::LostFlag::default();
        // the first project's default names follow the language the window will open in
        ddc_app::set_ui_lang(ddc_app::prefs::Prefs::load().startup_lang(
            &std::env::args().collect::<Vec<_>>(),
            ddc_app::shell::settings_win::system_lang(),
        ));
        let ed = Editor::start_project().map_err(|e| e.to_string())?;
        let d2 = desc.clone();
        let r = eframe::run_native(
            "DDC Editor",
            native_options(pick),
            Box::new(move |cc| {
                let rs = cc
                    .wgpu_render_state
                    .as_ref()
                    .ok_or("wgpu render state unavailable")?;
                gfx::watch_device(&rs.device, &lost2);
                let wake = cc.egui_ctx.clone();
                ddc_render::perf::set_waker(move || wake.request_repaint());
                let mut app = EditorApp::new(ed, Some(rs), d2);
                app.reveal_motion = reveal_motion;
                app.prefs = ddc_app::prefs::Prefs::load();
                app.ed.thin_pref = app.prefs.thin_lines;
                app.view.gfx.safe = rung > 0;
                app.view.gfx.crashed = crashed;
                app.view.gfx.why = why_key.to_string();
                if let Some(d) = ddc_app::gfx::data_dir() {
                    ddc_app::ui_kit::load_dialog_geometry(d.join("dialogs.txt"));
                }
                if let Some(dir) = app.prefs.autosave_folder().filter(|_| !no_recovery) {
                    let crashed = shell::recover_win::begin_session(&dir);
                    app.check_recovery(crashed);
                }
                app.load_layouts();
                let argv: Vec<String> = std::env::args().collect();
                app.layout = ddc_ui::workspace::layout_from_args(&argv)
                    .unwrap_or_else(|| app.prefs.layout.clone());
                app.apply_startup_prefs();
                app.open_at_start(&argv);
                if resets > 0 {
                    app.ed.status = Some(ddc_app::tr("gfx.lost.restarted", &[]));
                }
                let lost = Some((lost2, loss));
                Ok(Box::new(Window(app, None, 0, guard, lost)))
            }),
        );
        if let Err(e) = r {
            log.line(&format!("window failed on {desc}: {e}"));
            if wgpu_env().is_none() && i + 1 < n {
                return relaunch_next(i + 1, &mut log);
            }
            let why = format!("window failed on {desc}: {e}");
            if wgpu_env().is_none() && !pick.software {
                // every hardware step refused a window: software is the fallback
                return relaunch_safe(&mut log);
            }
            return Err(fatal_gfx(&why, !pick.software, &mut log));
        }
        return Ok(());
    }
    let t = Instant::now();
    let demo = demo::build(a.grid.0, a.grid.1);
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "scene: {}x{} rooms, {} walls, {} meshes, {} triangles, joined={}, built in {build_ms:.0} ms",
        a.grid.0,
        a.grid.1,
        demo.walls.len(),
        demo.scene.meshes.len(),
        demo.scene.triangle_count(),
        demo.joined
    );
    if a.headless {
        return report(&demo, a.frames);
    }
    let (pick, ..) = pick_backend(a.safe, &mut log)?;
    eframe::run_native(
        "DDC Editor",
        native_options(pick),
        Box::new(move |cc| Ok(Box::new(App::new(cc, demo, a.continuous)?))),
    )
    .map_err(|e| e.to_string())
}

struct Window(
    EditorApp,
    Option<ddc_ui::Mode>,
    u64,
    Option<ddc_app::gfx_guard::Guard>,
    Option<(gfx::LostFlag, ddc_app::gfx_guard::LossCtx)>,
);

/// Device loss in the real app: snapshot, sticky rung, a new process, the native box.
struct AppLoss<'a> {
    app: &'a mut EditorApp,
}

impl ddc_app::gfx_guard::LossActions for AppLoss<'_> {
    fn save(&mut self) -> Result<(), ddc_app::gfx_guard::SnapFail> {
        let snap = ddc_app::gfx_guard::save_for_loss(self.app);
        ddc_app::quick::save_now(self.app);
        StartupLog::append(&format!("device lost: snapshot {snap:?}"));
        snap.map(|_| ())
    }
    fn save_as(&mut self) -> Result<Option<std::path::PathBuf>, String> {
        let r = ddc_app::gfx_guard::save_as_for_loss(self.app);
        StartupLog::append(&format!("device lost: save as {r:?}"));
        r
    }
    fn persist_rung(&mut self, rung: u32) {
        if let Some(d) = gfx::data_dir() {
            ddc_app::gfx_guard::set_rung(&d, rung);
        }
    }
    fn release_locks(&mut self) {
        ddc_app::gfx_guard::release_for_handoff(self.app);
    }
    fn ask(&mut self, title: &str, body: &str, buttons: gfx::Buttons) -> gfx::Answer {
        gfx::message_box(title, body, buttons)
    }
    fn relaunch(&mut self, resets: u32, open: Option<&std::path::Path>) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let argv: Vec<String> = std::env::args().collect();
        // the session mark stays behind on purpose: the new start offers the snapshot
        std::process::Command::new(exe)
            .args(ddc_app::gfx_guard::relaunch_args(&argv, open))
            .env("DDC_GFX_RESETS", resets.to_string())
            .env_remove("DDC_GFX_START")
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn show(&mut self, title: &str, body: &str) {
        gfx::message_box(title, body, gfx::Buttons::Ok);
    }
}

impl Window {
    fn take_lost(&self) -> Option<String> {
        self.4.as_ref().and_then(|l| l.0.take())
    }

    /// The device is gone: nothing can be drawn any more, so save, hand over and leave.
    fn device_lost(&mut self, reason: &str) -> ! {
        StartupLog::append(&format!("graphics failure mid-session: {reason}"));
        let ctx = self.4.as_ref().map(|l| l.1).unwrap_or_default();
        // the start counts as survived: a loss is no driver crash at start
        self.3 = None;
        let lang = self.0.sh.cat.lang;
        let log = gfx::data_dir()
            .map(|d| d.join("logs").join("startup.log"))
            .filter(|p| p.exists());
        let mut acts = AppLoss { app: &mut self.0 };
        let end = ddc_app::gfx_guard::handle_loss(&ctx, reason, lang, log.as_deref(), &mut acts);
        // process::exit skips Drop: a copied key must not outlive the app here either
        ddc_app::ai::ui::keys::clip_exit();
        std::process::exit(match end {
            ddc_app::gfx_guard::LossEnd::Relaunched(_) => 0,
            ddc_app::gfx_guard::LossEnd::Stopped | ddc_app::gfx_guard::LossEnd::Unsaved => 1,
        });
    }
}

impl Drop for Window {
    /// Normal exit (also after a caught panic the lock stays: see `update`).
    fn drop(&mut self) {
        ddc_app::ai::ui::keys::clip_exit();
        ddc_app::quick::save_now(&mut self.0);
        // a panic outside `update` unwinds through here too: that is no normal exit
        if self.2 != u64::MAX && !std::thread::panicking() {
            self.0.end_autosave_session();
        }
    }
}

impl eframe::App for Window {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // system motion setting: read at start and about once a second (cheap call)
        if self.2 % 60 == 0 {
            self.0.sh.theme.reduced_motion = ddc_app::win::reduced_motion();
        }
        self.2 += 1;
        if self.2 % 30 == 0 {
            let (rect, normal) = ctx.input(|i| {
                let v = i.viewport();
                let normal = !v.maximized.unwrap_or(false) && !v.minimized.unwrap_or(false);
                (v.outer_rect.zip(v.inner_rect), normal)
            });
            if let (Some((o, i)), true) = (rect, normal) {
                ddc_app::win_geom::track(ddc_app::win_geom::Geom {
                    pos: (o.min.x, o.min.y),
                    size: (i.width(), i.height()),
                });
            }
        }
        if let Some(r) = self.take_lost() {
            self.device_lost(&r);
        }
        let rs = frame.wgpu_render_state();
        let app = &mut self.0;
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.ui(ctx, rs)));
        if let Some(r) = self.take_lost() {
            // a panic on a lost device is the loss, not a bug: same path, no crash
            self.device_lost(&r);
        }
        if let Err(e) = run {
            // crash: the hook wrote the note; flush a snapshot, keep the session lock
            self.0.crash_flush();
            self.2 = u64::MAX;
            std::panic::resume_unwind(e);
        }
        for n in ddc_app::safe_file::take_notices() {
            self.0.ed.status = Some(n.text());
        }
        // the frames must really come, even on an idle window, for the start to count as survived
        if let Some(g) = &mut self.3 {
            if g.frame() {
                self.3 = None;
            } else {
                ctx.request_repaint();
            }
        }
        let mode = self.0.sh.theme.mode;
        if self.1 != Some(mode) {
            let title = self.0.project_ui.applied_title();
            ddc_app::win::style_title_bar(title, mode == ddc_ui::Mode::Dark);
            self.1 = Some(mode);
        }
    }
}

/// Room plus a wall laid over its south side: two «overlapping walls» warnings.
fn draw_overlap(ed: &mut Editor) {
    draw_room(ed);
    for (a, b) in [((0.0, 0.0), (3.5, 0.0)), ((6.0, 4.0), (6.0, 1.5))] {
        ed.set_mode(Mode::Polyline);
        ed.handle(Event::Click(v2(a.0, a.1)));
        ed.handle(Event::Click(v2(b.0, b.1)));
        ed.handle(Event::Key(ddc_app::wall_tool::Key::Escape));
    }
    ed.set_mode(Mode::Select);
}

/// Recovery dialog over a scratch autosave folder: one good snapshot, one damaged.
fn recover_demo(app: &mut EditorApp) -> Result<(), String> {
    use shell::recover_win::UNTITLED;
    let dir = std::env::temp_dir().join(format!("ddc_recover_demo_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    app.prefs.autosave_dir = dir.display().to_string();
    let file = dir.join(UNTITLED);
    draw_room(&mut app.ed);
    app.ed
        .model
        .autosave_as(&file, 5)
        .map_err(|e| e.to_string())?;
    let bad = app
        .ed
        .model
        .autosave_as(&file, 5)
        .map_err(|e| e.to_string())?;
    std::fs::write(&bad, b"damaged").map_err(|e| e.to_string())?;
    // the open model moved on: one wall less
    app.ed.selection = app
        .ed
        .walls
        .first()
        .map(|w| vec![ddc_app::editor::Sel::Wall(w.id)])
        .unwrap_or_default();
    app.ed.delete_selection();
    app.check_recovery(true);
    if let Some(r) = &mut app.recover {
        let snap = r.snaps[r.pick].path.clone();
        r.diff = Some(
            app.ed
                .model
                .compare_snapshot(&snap)
                .map_err(|e| e.to_string()),
        );
    }
    Ok(())
}

/// Scripted 6×4 m room (rectangle tool drag from origin).
fn draw_room(ed: &mut Editor) {
    ed.set_mode(Mode::Rect);
    ed.handle(Event::Move(v2(0.0, 0.0)));
    ed.handle(Event::Press(v2(0.0, 0.0)));
    ed.handle(Event::Move(v2(6.0, 4.0)));
    ed.handle(Event::Release(v2(6.0, 4.0)));
    ed.set_mode(Mode::Select);
}

/// Phases: an existing 6x4 m box with an east wall to be demolished and a new 3x4 m extension;
/// the plan view phase comes from the filter (`demolition` is the "Demolition" phase, otherwise the last).
fn draw_phases(ed: &mut Editor, filter: &str) -> Result<(), String> {
    let e = |e: ddc_model::Error| e.to_string();
    let rect = |ed: &mut Editor, a: (f64, f64), b: (f64, f64)| {
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Move(v2(a.0, a.1)));
        ed.handle(Event::Press(v2(a.0, a.1)));
        ed.handle(Event::Move(v2(b.0, b.1)));
        ed.handle(Event::Release(v2(b.0, b.1)));
        ed.set_mode(Mode::Select);
    };
    let ps = ed.model.phases().map_err(e)?;
    let first = ps.first().ok_or("no phases")?.id;
    let last = ps.last().ok_or("no phases")?.id;
    let demo = ps
        .iter()
        .find(|p| p.key.as_deref() == Some("demolition"))
        .map_or(first, |p| p.id);
    ed.model.set_current_phase(first).map_err(e)?;
    rect(ed, (0.0, 0.0), (6.0, 4.0));
    let east = ed
        .walls
        .iter()
        .find(|w| (w.a.x - 6.0).abs() < 1e-6 && (w.b.x - 6.0).abs() < 1e-6)
        .map(|w| w.id)
        .ok_or("no east wall")?;
    ed.model.demolish(&[("wall", east)], demo).map_err(e)?;
    ed.model.set_current_phase(last).map_err(e)?;
    rect(ed, (7.0, 0.0), (10.0, 4.0));
    let f = ddc_model::PhaseFilter::from_key(filter).unwrap_or(ddc_model::PhaseFilter::All);
    let phase = if f == ddc_model::PhaseFilter::Demolition {
        demo
    } else {
        last
    };
    let vp = ddc_model::ViewPhase { phase, filter: f };
    ed.model
        .set_view_phase(ddc_model::ViewKey::Plan(ed.level), vp)
        .map_err(e)?;
    ed.reload().map_err(e)?;
    ed.selection = vec![ddc_app::editor::Sel::Wall(east)];
    Ok(())
}

/// Wall schedule: the project parameter "Fire resistance" with values, grouped by phase.
fn schedule_demo(app: &mut EditorApp) -> Result<(), String> {
    let e = |e: ddc_model::Error| e.to_string();
    let ed = &mut app.ed;
    let pid = ed
        .model
        .add_param(&ddc_model::ParamSpec {
            name: ddc_app::tr("sketch_edit.seed_fire", &[]),
            ptype: ddc_model::ParamType::Text,
            per_type: false,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["wall".into()],
        })
        .map_err(e)?;
    let sets: Vec<ddc_model::ParamSet> = ed
        .walls
        .iter()
        .enumerate()
        .map(|(i, w)| ddc_model::ParamSet {
            param: pid,
            kind: "wall".into(),
            id: w.id,
            value: Some(ddc_model::PVal::Text(
                if i % 2 == 0 { "EI 60" } else { "EI 30" }.into(),
            )),
        })
        .collect();
    ed.model.set_param_values(&sets).map_err(e)?;
    ed.reload().map_err(e)?;
    app.table.demo_setup();
    Ok(())
}

/// Two levels: 10×6 m with a partition and two rooms below, 6×4 m above (active), room selected.
fn draw_levels(ed: &mut Editor) -> Result<(), String> {
    let rect = |ed: &mut Editor, a: (f64, f64), b: (f64, f64)| {
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Move(v2(a.0, a.1)));
        ed.handle(Event::Press(v2(a.0, a.1)));
        ed.handle(Event::Move(v2(b.0, b.1)));
        ed.handle(Event::Release(v2(b.0, b.1)));
        ed.set_mode(Mode::Select);
    };
    rect(ed, (0.0, 0.0), (10.0, 6.0));
    let room = |ed: &mut Editor, name: &str, num: &str, p: (f64, f64)| {
        let level = ed.level;
        ed.model
            .add_room(&ddc_model::RoomSpec {
                level,
                name: name.into(),
                number: num.into(),
                point: p,
                height: 2.7,
            })
            .map_err(|e| e.to_string())
    };
    room(ed, &ddc_app::tr("sketch_edit.seed_living", &[]), "101", (5.0, 3.0))?;
    ed.add_level(&ddc_app::tr("sketch_edit.seed_level2", &[]), 3.0).ok_or("level")?;
    rect(ed, (2.0, 1.0), (8.0, 5.0));
    let id = room(ed, &ddc_app::tr("sketch_edit.seed_bedroom", &[]), "201", (5.0, 3.0))?;
    ed.reload().map_err(|e| e.to_string())?;
    ed.selection = vec![ddc_app::editor::Sel::Room(id)];
    Ok(())
}

/// Room 6×4 m with a sloped 5 % roof slab and a glazed curtain wall 7…13 m with a door panel.
fn draw_facade(ed: &mut Editor, scene: &str) -> Result<(), String> {
    use ddc_app::editor::Sel;
    draw_room(ed);
    let e = |e: ddc_model::Error| e.to_string();
    let st = ed
        .model
        .add_slab_type(
            ddc_app::tr("sketch_edit.seed_slab_200", &[]).as_str(),
            &[ddc_model::LayerSpec {
                material: "concrete".into(),
                thickness: 0.2,
            }],
        )
        .map_err(e)?;
    let slab = ed
        .model
        .add_slab(&ddc_model::SlabSpec {
            level: ed.level,
            slab_type: st,
            offset: 3.0,
            outer: vec![(-0.2, -0.2), (6.2, -0.2), (6.2, 4.2), (-0.2, 4.2)],
            holes: vec![],
        })
        .map_err(e)?;
    ed.reload().map_err(e)?;
    ed.set_slab_slope_percent(slab, 5.0);
    ed.add_curtains(&[
        (v2(7.5, 0.0), v2(13.5, 0.0), 0.0),
        (v2(13.5, 0.0), v2(13.5, 4.0), 0.0),
    ]);
    let cw = ed.curtains.first().map(|c| c.id).ok_or("no curtain wall")?;
    ed.set_panel_kind(cw, 1, ddc_model::PanelKind::Door);
    ed.set_panel_kind(cw, 6, ddc_model::PanelKind::Solid);
    ed.set_panel_kind(cw, 10, ddc_model::PanelKind::Window);
    ed.set_mode(Mode::Select);
    ed.selection = match scene {
        "curtain" => vec![Sel::Curtain(cw)],
        "panel" => vec![Sel::Panel(cw, 1)],
        "slab" => vec![Sel::Slab(slab)],
        _ => vec![],
    };
    Ok(())
}

/// Facade scene plus a second level, floor slab, a door (south) and a window (north) on one line
/// x = 2.45, compass elevations and two sections: through the door/window and through the
/// curtain wall.
fn draw_section_scene(ed: &mut Editor) -> Result<(), String> {
    use ddc_app::editor::sec::spec_from;
    draw_facade(ed, "section")?;
    let e = |e: ddc_model::Error| e.to_string();
    ed.model.add_level(&ddc_app::tr("sketch_edit.seed_level2", &[]), 3.0).map_err(e)?;
    let st = ed
        .model
        .add_slab_type(
            ddc_app::tr("sketch_edit.seed_floor_200", &[]).as_str(),
            &[ddc_model::LayerSpec {
                material: "concrete".into(),
                thickness: 0.2,
            }],
        )
        .map_err(e)?;
    ed.model
        .add_slab(&ddc_model::SlabSpec {
            level: ed.level,
            slab_type: st,
            offset: 0.0,
            outer: vec![(-0.2, -0.2), (6.2, -0.2), (6.2, 4.2), (-0.2, 4.2)],
            holes: vec![],
        })
        .map_err(e)?;
    // an opening from x0 of width w on the horizontal wall y = wy (along is from the wall start)
    let mut open = |wy: f64, x0: f64, w: f64, h: f64, sill: f64, kind: &str| {
        let wall = ed
            .walls
            .iter()
            .find(|q| (q.a.y - wy).abs() < 1e-6 && (q.b.y - wy).abs() < 1e-6)
            .cloned()
            .ok_or("no wall")?;
        let along = if wall.a.x < wall.b.x {
            x0 - wall.a.x
        } else {
            wall.a.x - (x0 + w)
        };
        ed.model
            .add_opening(
                wall.id,
                &ddc_model::OpeningSpec {
                    along,
                    width: w,
                    sill,
                    height: h,
                    kind: kind.into(),
                },
            )
            .map_err(|x| x.to_string())?;
        Ok::<(), String>(())
    };
    open(0.0, 2.0, 0.9, 2.1, 0.0, "door")?;
    open(4.0, 2.0, 1.5, 1.5, 0.9, "window")?;
    ed.reload().map_err(e)?;
    ed.add_compass();
    for (name, a, b, c) in [
        (ddc_app::tr("sketch_edit.seed_section1", &[]), v2(2.45, -1.5), v2(2.45, 5.5), v2(0.0, 2.0)),
        (ddc_app::tr("sketch_edit.seed_section2", &[]), v2(-1.0, 2.0), v2(15.0, 2.0), v2(8.0, 6.0)),
    ] {
        let s = spec_from(&name, a, b, Some(c)).ok_or("bad section")?;
        ed.add_view(&s);
    }
    ed.set_mode(Mode::Select);
    Ok(())
}

/// `--view section:N` / `elevation:S`: open that view in the shot.
fn open_shot_view(app: &mut EditorApp, v: &str) {
    let Some((kind, key)) = v.split_once(':') else {
        return;
    };
    let k = match kind {
        "section" => ddc_model::ViewKind::Section,
        "elevation" => ddc_model::ViewKind::Elevation,
        _ => return,
    };
    let list: Vec<(i64, String)> = app
        .ed
        .views
        .iter()
        .filter(|(_, s)| s.kind == k)
        .map(|(i, s)| (*i, s.name.clone()))
        .collect();
    let by_side = ["N", "S", "E", "W"]
        .iter()
        .zip(ddc_model::COMPASS)
        .find(|(c, _)| c.eq_ignore_ascii_case(key))
        .map(|(_, n)| n);
    let id = match (key.parse::<usize>(), by_side) {
        (Ok(n), _) => list.get(n.saturating_sub(1)).map(|x| x.0),
        (_, Some(n)) => list.iter().find(|x| x.1 == n).map(|x| x.0),
        _ => list
            .iter()
            .find(|x| x.1.eq_ignore_ascii_case(key))
            .map(|x| x.0),
    };
    if let Some(id) = id {
        app.open_view(id);
    }
}

/// Render one window frame offscreen and save it as PNG.
fn screenshot(a: &Args, out: &str, log: &mut StartupLog) -> Result<(), String> {
    ddc_app::shot_press::set_screenshot_mode(true);
    let (pick, desc, ..) = pick_backend(a.safe, log)?;
    let rs = ddc_app::shot::render_state(pick)?;
    // default names are seeded below, before the window applies `--lang`
    let argv: Vec<String> = std::env::args().collect();
    if let Some(i) = argv.iter().position(|s| s == "--lang") {
        let ru = argv.get(i + 1).is_some_and(|l| l == "ru");
        ddc_app::set_ui_lang(if ru { ddc_ui::i18n::Lang::Ru } else { ddc_ui::i18n::Lang::En });
    }
    let mut ed = Editor::new_in_memory().map_err(|e| e.to_string())?;
    // qa: `--open-project file` renders a saved project instead of a built-in scene
    let open_project = std::env::args()
        .skip_while(|s| s != "--open-project")
        .nth(1);
    if let Some(f) = &open_project {
        ed = Editor::start_project().map_err(|e| e.to_string())?;
        ed.open_project(std::path::Path::new(f))
            .map_err(|e| format!("{e:?}"))?;
    }
    if a.scene == "room" {
        draw_room(&mut ed);
        if let Some(w) = ed.walls.first() {
            ed.selection = vec![ddc_app::editor::Sel::Wall(w.id)];
        }
    }
    if ["facade", "curtain", "panel", "slab"].contains(&a.scene.as_str()) {
        draw_facade(&mut ed, &a.scene)?;
    }
    if a.scene == "section" {
        draw_section_scene(&mut ed)?;
    }
    if a.scene == "levels" {
        draw_levels(&mut ed)?;
    }
    if a.scene == "overlap" {
        draw_overlap(&mut ed);
    }
    if a.scene == "mass" {
        ddc_app::mass_tools::demo_scene(&mut ed)?;
    }
    if a.scene == "mep" {
        ddc_app::mep3d::demo(&mut ed)?;
        ddc_app::mep3d::demo_office(&mut ed)?;
    }
    ddc_app::shot_scenes::build(&mut ed, &a.scene)?;
    if a.scene == "phases" {
        let argv: Vec<String> = std::env::args().collect();
        let f = argv
            .iter()
            .position(|s| s == "--phase-filter")
            .and_then(|i| argv.get(i + 1))
            .cloned()
            .unwrap_or_default();
        draw_phases(&mut ed, &f)?;
    }
    // dev: `--stream file` — a captured bridge stream imported as the scene
    let argv0: Vec<String> = std::env::args().collect();
    if let Some(f) = argv0
        .iter()
        .position(|s| s == "--stream")
        .and_then(|i| argv0.get(i + 1))
    {
        let bytes = std::fs::read(f).map_err(|e| e.to_string())?;
        let mut r = ddc_io::bridge::proto::FrameReader::new(&bytes[..]);
        let mut frames = vec![];
        while let Some(fr) = r.next_frame().map_err(|e| e.to_string())? {
            frames.push(fr);
        }
        ed.apply_import(std::path::Path::new(f), frames)?;
    }
    ed.status = None; // scene setup is not a user action: no toast in the shot
    let walls = ed.walls.len();
    let mut app = EditorApp::new(ed, Some(&rs), desc.clone());
    app.reveal_motion = ddc_app::reveal::motion_arg(&std::env::args().collect::<Vec<_>>())?;
    app.view3d = a.view3d;
    ddc_app::gfx_ui::apply_cli(&mut app, &std::env::args().collect::<Vec<_>>());
    app.prefs.theme = if a.theme.is_empty() {
        "light".into()
    } else {
        a.theme.clone()
    };
    if (a.scene != "empty" && a.scene != "room")
        || open_project.is_some()
        || argv0.iter().any(|s| s == "--stream")
    {
        app.plan.fit(&app.ed);
    }
    app.prefs.ribbon_small = std::env::args().any(|s| s == "--ribbon-small");
    let argv: Vec<String> = std::env::args().collect();
    // `--ai-panel-w 280`: the AI panel at that width, as the setting would give it
    if let Some(w) = ddc_app::shot::ai_panel_w(&argv) {
        app.prefs.ai_panel_w = w;
    }
    if let Some(l) = ddc_ui::workspace::layout_from_args(&argv) {
        app.layout = l;
    }
    // `--look <id>`: one of the seven looks, for shots
    if let Some(id) = argv.iter().position(|s| s == "--look").and_then(|i| argv.get(i + 1)) {
        app.use_look(id);
    }
    app.force_chooser = argv.iter().any(|s| s == "--first-run");
    // `--onboarding N`: the setup wizard on step N; without it no shot shows the wizard
    let step = argv
        .iter()
        .position(|s| s == "--onboarding")
        .and_then(|i| argv.get(i + 1))
        .and_then(|s| s.parse::<u8>().ok());
    match step {
        Some(n) => app.open_onboarding(n),
        None => app.chrome.onboard.quiet = true,
    }
    open_shot_view(&mut app, &a.view);
    if let Some(dir) = &a.reveal_out {
        let n = ddc_app::reveal::export_cli(&mut app, &a.reveal, std::path::Path::new(dir), a.reveal_fps, a.size, a.reveal_frame)?;
        println!("reveal: {} frames -> {dir}", n.frames);
        if n.stuck_frames > 0 {
            eprintln!("reveal: the camera could not leave the section box in {} frames", n.stuck_frames);
        }
        return Ok(());
    }
    // `--ai-mode chat|build|review`: start on that AI tab (shots for review)
    if let Some(m) = ddc_app::shot::ai_mode(&argv) {
        ddc_app::ai::ui::modes::enter(&mut app, m);
    }
    // dev: `--select-body mesh|col|beam|ceiling` picks the largest body of that kind on the
    // active level; `--move-body dx` moves the selection, `--undo` steps back once
    if let Some(k) = argv
        .iter()
        .position(|s| s == "--select-body")
        .and_then(|i| argv.get(i + 1))
    {
        use ddc_app::editor::bodies::BodyKind as K;
        let kind = match k.as_str() {
            "col" => K::Column,
            "beam" => K::Beam,
            "ceiling" => K::Ceiling,
            _ => K::Mesh,
        };
        let area = |h: &[ddc_geom::V2]| {
            (0..h.len())
                .map(|i| {
                    let (p, q) = (h[i], h[(i + 1) % h.len()]);
                    p.x * q.y - q.x * p.y
                })
                .sum::<f64>()
                .abs()
        };
        let pick = app
            .ed
            .bodies
            .iter()
            .filter(|b| b.kind == kind && !ddc_app::editor::bodies::is_site(&b.category))
            .max_by(|a, b| area(&a.hull).total_cmp(&area(&b.hull)))
            .map(|b| ddc_app::editor::Sel::Body(b.kind, b.id));
        app.ed.selection = pick.into_iter().collect();
    }
    // dev: `--select-elem roof|stair|railing|shaft|grid|fitting` selects the last such element
    if let Some(k) = argv.iter().position(|s| s == "--select-elem").and_then(|i| argv.get(i + 1)) {
        use ddc_app::editor::{structx, Sel};
        let made = |k: &str| {
            let v = ddc_app::shot_scenes::ELEMS.lock().unwrap();
            v.iter().find(|x| x.0 == k).map(|x| x.1)
        };
        let sel = if k == "fitting" {
            app.ed.model.mep_fittings(None).ok().and_then(|v| v.first().map(|f| ddc_app::elem::fitting::sel(f.key)))
        } else if k == "floor" {
            made(k).map(Sel::Slab)
        } else {
            let id = ddc_app::shot_scenes::ELEMS.lock().unwrap().iter().find(|x| x.0 == k).map(|x| x.1);
            id.zip(structx::code_of(k)).map(|(id, c)| Sel::Ext(c, id))
        };
        app.ed.selection = sel.into_iter().collect();
    }
    if let Some(dx) = argv
        .iter()
        .position(|s| s == "--move-body")
        .and_then(|i| argv.get(i + 1))
        .and_then(|v| v.parse::<f64>().ok())
    {
        let ts: Vec<ddc_model::Target> = app
            .ed
            .selection
            .iter()
            .filter_map(|s| match s {
                ddc_app::editor::Sel::Body(k, id) => Some(k.target(*id)),
                _ => None,
            })
            .collect();
        let sel = app.ed.selection.clone();
        app.ed
            .run_op(ddc_app::editor::OpFn(std::sync::Arc::new(
                move |m: &mut ddc_model::Model| m.move_elements(&ts, (dx, 0.0)),
            )))
            .map_err(|e| e.to_string())?;
        app.ed.selection = sel;
    }
    if flag_undo(&argv) {
        let sel = app.ed.selection.clone();
        app.ed.undo();
        app.ed.selection = sel;
    }
    if argv.iter().any(|s| s == "--section-box") {
        if let Some(mut b) = app.scene.bounds {
            b.max.z = b.min.z + (b.max.z - b.min.z) * 0.62;
            b.max.x = b.min.x + (b.max.x - b.min.x) * 0.7;
            app.view.section_box = Some(b);
        }
    }
    app.warnings_open = argv.iter().any(|s| s == "--warnings");
    if argv.iter().any(|s| s == "--settings-interface") {
        app.open_settings(1);
    }
    // `--fold SPEC`, `--run CMD`: folded chrome and commands run before the shot
    let shot_setup = ddc_app::shot::apply_fold(&mut app, &argv)
        .and_then(|()| ddc_app::shot::run_commands(&mut app, &argv));
    if let Err(e) = shot_setup {
        eprintln!("{e}");
        std::process::exit(2);
    }
    let flag = |f: &str| argv.iter().any(|s| s == f);
    if let Some(l) = argv
        .iter()
        .position(|s| s == "--lang")
        .and_then(|i| argv.get(i + 1))
    {
        let lang = ddc_ui::i18n::Lang::from_code(l);
        app.set_lang(lang);
    }
    if flag("--start-screen") {
        app.open_start();
    }
    if let Some(n) = ddc_app::shell::settings_win::page_from_args(&argv) {
        app.open_settings(n);
    }
    if flag("--job-demo") {
        ddc_app::bigfile::start_demo(&mut app);
        app.chrome.tasks.expanded = true;
    }
    if let Some(t) = argv
        .iter()
        .position(|s| s == "--ribbon-tab")
        .and_then(|i| argv.get(i + 1))
    {
        let known = app.sh.reg.tabs();
        app.sh.ribbon.set_tab(t, &known);
    }
    if flag("--status-demo") {
        app.autosave.last_ok = Some("12:04".into());
        app.ed.set_mode(Mode::Polyline);
        app.ed.handle(Event::Move(v2(1.0, 1.0)));
        app.ed.handle(Event::Click(v2(1.0, 1.0)));
        app.ed.handle(Event::Move(v2(4.2, 1.0)));
        app.plan.cursor = Some(v2(4.2, 1.0));
    }
    for (i, s) in argv.iter().enumerate() {
        if s == "--run-cmd" {
            if let Some(id) = argv.get(i + 1) {
                app.run(id);
            }
        }
    }
    let press = ddc_app::shot_press::parse(&argv)?;
    if let Some(p) = &press {
        ddc_app::shot_press::run(p, &mut |id| app.run(id))?;
    }
    if let Some(id) = argv
        .iter()
        .position(|s| s == "--tool-demo")
        .and_then(|i| argv.get(i + 1))
    {
        ddc_app::tools::demo(&mut app, id)?;
    }
    if argv
        .iter()
        .position(|s| s == "--family-draft")
        .and_then(|i| argv.get(i + 1))
        .is_some_and(|w| w == "demo")
    {
        ddc_app::family_editor::demo_draft(&mut app)?;
    }
    if flag("--mass-preview") {
        let lang = app.sh.cat.lang;
        ddc_app::mass_tools::demo_preview(&mut app.ed, lang);
    }
    if flag("--clash-demo") {
        ddc_app::shell::clash_win::demo(&mut app)?;
    }
    if let Some(what) = ddc_app::sheet_view::demo_arg(&argv) {
        ddc_app::sheet_view::demo(&mut app, what)?;
    }
    if flag("--autosave-settings") {
        app.autosave.last_ok = Some("12:04".into());
        app.autosave_open = true;
    }
    if flag("--recover-demo") {
        recover_demo(&mut app)?;
    }
    if let Some(st) = argv
        .iter()
        .position(|s| s == "--dwg-demo")
        .and_then(|i| argv.get(i + 1))
    {
        ddc_app::shell::drawing_win::demo(&mut app, st)?;
    }
    if let Some(st) = argv
        .iter()
        .position(|s| s == "--import-demo")
        .and_then(|i| argv.get(i + 1))
    {
        ddc_app::shell::import_win::demo(&mut app, st)?;
    }
    if flag("--schedule-demo") {
        schedule_demo(&mut app)?;
    }
    if flag("--phases-win") {
        ddc_app::phase_ui::open_window();
    }
    if flag("--params-win") {
        ddc_app::params_ui::open_window();
    }
    if argv.iter().any(|s| s == "--xlsx-preview") {
        let p = std::env::temp_dir().join(format!("ddc_xlsx_demo_{}.xlsx", std::process::id()));
        ddc_app::shell::xlsx_win::demo_book(&app.ed.model, &p)?;
        app.xlsx_open_now(&p, true);
    }
    let ctx = egui::Context::default();
    let px = ddc_app::shot::frame(
        &rs,
        &ctx,
        a.size,
        a.scale,
        press.as_ref().map_or(ddc_app::shot_press::DEFAULT_FRAMES, |p| p.frames),
        a.hover.map(|(x, y)| (x / a.scale, y / a.scale)),
        |c, _| app.ui(c, Some(&rs)),
    );
    let crop = argv
        .iter()
        .position(|s| s == "--crop")
        .and_then(|i| argv.get(i + 1))
        .and_then(|v| v.split_once(','))
        .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)));
    match crop {
        Some((y0, y1)) if y0 < y1 && y1 <= a.size.1 => {
            let row = a.size.0 as usize * 4;
            let px = &px[y0 as usize * row..y1 as usize * row];
            ddc_app::shot::save_png(std::path::Path::new(out), (a.size.0, y1 - y0), px)?;
        }
        _ => ddc_app::shot::save_png(std::path::Path::new(out), a.size, &px)?,
    }
    println!(
        "screenshot: {out} · {}x{}@{} · walls {walls} · {desc}",
        a.size.0, a.size.1, a.scale
    );
    Ok(())
}

/// `--file` + `--screenshot`: plan, 3D and section images of the loaded state
/// (`report_shot`); returns the `render` object of the headless report.
fn render_loaded(
    l: ddc_app::headless_file::Loaded,
    fa: &ddc_app::headless_file::FileArgs,
    safe: bool,
    log: &mut StartupLog,
) -> Result<serde_json::Value, String> {
    let t = Instant::now();
    let (pick, desc, ..) = pick_backend(safe, log)?;
    let rs = ddc_app::shot::render_state(pick)?;
    let gpu_open_ms = (t.elapsed().as_secs_f64() * 1e5).round() / 100.0;
    let mut v = ddc_app::report_shot::render(l, fa, &rs, desc)?;
    v["gpu_open_ms"] = gpu_open_ms.into();
    v["since_start_ms"] = ddc_app::perf_bench::since_start_ms().into();
    Ok(v)
}

fn flag_undo(argv: &[String]) -> bool {
    argv.iter().any(|s| s == "--undo")
}

fn report(demo: &Demo, frames: u32) -> Result<(), String> {
    let (w, h) = (1280, 720);
    let mut cam = OrbitCamera::default();
    if let Some(b) = demo.scene.bounds {
        cam.fit(&b, w as f32 / h as f32);
    }
    let center = demo.scene.pick(&cam.ray(0.0, 0.0, w as f32 / h as f32));
    let sel = center.map(|c| c.element);
    let r = headless::run(&demo.scene, &cam, (w, h), frames, sel, (w / 2, h / 2))?;
    println!(
        "headless: {} | {w}x{h} | {} frames | {:.2} ms/frame | {:.0} fps | center pick {:?} | center px {:?}",
        r.adapter,
        r.frames,
        r.avg_ms,
        r.fps,
        center.map(|c| (c.mesh, c.triangle, c.element)),
        r.probe_rgba
    );
    if std::env::var_os("DDC_BENCH_QUALITY").is_some() {
        use ddc_render::Quality;
        for (w, h) in [(1920u32, 1080u32), (1366, 768)] {
            let mut cam = cam.clone();
            if let Some(b) = demo.scene.bounds {
                cam.fit(&b, w as f32 / h as f32);
            }
            for q in [Quality::Off, Quality::Medium, Quality::High] {
                let r = headless::run_with(&demo.scene, &cam, (w, h), frames, sel, (0, 0), q)?;
                println!(
                    "headless-q: {w}x{h} {q:?}: {:.2} ms/frame, {:.0} fps",
                    r.avg_ms, r.fps
                );
            }
        }
    }
    Ok(())
}

struct App {
    demo: Demo,
    view: Viewport,
    tokens: Tokens,
    last: Instant,
    frame_ms: f32,
    continuous: bool,
    bench: Option<ddc_app::perf_bench::WinBench>,
}

/// `--bench SPEC --bench-window`: the scene in a real window without vsync, scripted orbit.
fn bench_window(
    spec: &str,
    scene: ddc_render::Scene,
    elements: usize,
    open_ms: f64,
    a: &Args,
) -> Result<(), String> {
    let mut log = StartupLog::open();
    let (pick, desc, ..) = pick_backend(a.safe, &mut log)?;
    let tris = scene.triangle_count();
    let mut wb =
        ddc_app::perf_bench::WinBench::new(spec.to_string(), elements, tris, open_ms, a.frames);
    wb.jsonl = a.bench_jsonl.clone();
    wb.adapter = desc;
    let demo = Demo {
        scene,
        walls: vec![],
        joined: false,
    };
    let mut opts = native_options(pick);
    opts.viewport = opts
        .viewport
        .with_inner_size([a.size.0 as f32, a.size.1 as f32]);
    opts.vsync = false;
    opts.wgpu_options.present_mode = ddc_render::wgpu::PresentMode::AutoNoVsync;
    eframe::run_native(
        "DDC Editor bench",
        opts,
        Box::new(move |cc| {
            let mut app = App::new(cc, demo, true)?;
            app.bench = Some(wb);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| e.to_string())
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, demo: Demo, continuous: bool) -> Result<Self, String> {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .ok_or("wgpu render state unavailable")?;
        shell::install_renderer(rs, &demo.scene);
        let tokens = ui_kit::light();
        ui_kit::apply(&cc.egui_ctx, &tokens);
        Ok(Self {
            demo,
            view: Viewport::new(),
            tokens,
            last: Instant::now(),
            frame_ms: 16.0,
            continuous,
            bench: None,
        })
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(b) = &mut self.bench {
            let refining = frame.wgpu_render_state().is_some_and(|rs| {
                rs.renderer
                    .read()
                    .callback_resources
                    .get::<ddc_render::Renderer>()
                    .is_some_and(|r| r.chunks.frame_stats().refining)
            });
            let px = ctx.screen_rect().size() * ctx.pixels_per_point();
            if let Some(line) = b.tick(&mut self.view.camera, refining, (px.x as u32, px.y as u32)) {
                println!("{line}");
                if let Some(p) = &b.jsonl {
                    if let Err(e) = ddc_app::perf_bench::append(p, &line) {
                        eprintln!("bench: {e}");
                    }
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32() * 1000.0;
        self.last = now;
        self.frame_ms = self.frame_ms * 0.9 + dt * 0.1;
        let t = self.tokens;
        ui_kit::top_bar(ctx, &t, |ui| {
            ui_kit::heading(ui, ddc_app::brand::PRODUCT);
            ui_kit::dim(
                ui,
                &t,
                ddc_app::tr(
                    "sketch_edit.main_stats",
                    &[
                        ("walls", &self.demo.walls.len().to_string()),
                        ("tris", &self.demo.scene.triangle_count().to_string()),
                        ("fps", &format!("{:.0}", 1000.0 / self.frame_ms.max(0.01))),
                        ("ms", &format!("{:.1}", self.frame_ms)),
                    ],
                ),
            );
            ui_kit::dim(ui, &t, ddc_app::tr("sketch_edit.main_hint", &[]));
        });
        ui_kit::side_panel(ctx, &t, ui_kit::PROPS_W, |ui| {
            ui_kit::heading(ui, &ddc_app::tr("sketch_edit.main_props", &[]));
            match self.view.selected {
                None => ui_kit::dim(ui, &t, ddc_app::tr("sketch_edit.main_none", &[])),
                Some(h) => {
                    let wall = self.demo.walls.get(h.element as usize);
                    ui_kit::props(ui, "sel", |ui| {
                        ui_kit::kv(
                            ui,
                            &ddc_app::tr("sketch_edit.main_element", &[]),
                            format!("#{}", h.element),
                        );
                        ui_kit::kv(
                            ui,
                            &ddc_app::tr("sketch_edit.main_type", &[]),
                            ddc_app::tr(
                                if wall.is_some() {
                                    "sketch_edit.main_wall"
                                } else {
                                    "sketch_edit.main_node"
                                },
                                &[],
                            ),
                        );
                        ui_kit::kv(
                            ui,
                            &ddc_app::tr("sketch_edit.main_mesh_tri", &[]),
                            format!("{} / {}", h.mesh, h.triangle),
                        );
                        if let Some(w) = wall {
                            let u = ddc_app::units::active_units();
                            let ru = ddc_app::ui_is_ru();
                            ui_kit::kv(
                                ui,
                                &ddc_app::tr("sketch_edit.main_length", &[]),
                                ddc_app::unit_text::len(w.length, ru, u),
                            );
                            for (k, (th, vol)) in w.layers.iter().enumerate() {
                                let layer = ddc_app::tr("sketch_edit.main_layer", &[]);
                                let name = LAYER_NAMES.get(k).copied().unwrap_or(layer.as_str());
                                ui_kit::kv(
                                    ui,
                                    &format!("{name} {}", ddc_app::unit_text::len(*th, ru, u)),
                                    ddc_app::unit_text::vol(*vol, ru, u),
                                );
                            }
                        }
                    });
                }
            }
        });
        ui_kit::viewport_panel(ctx, &t, |ui| {
            self.view.show(ui, &self.demo.scene);
        });
        if self.continuous {
            ctx.request_repaint();
        }
    }
}
