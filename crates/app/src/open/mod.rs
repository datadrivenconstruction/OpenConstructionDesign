//! One door for opening: Ctrl+O, `file.import` (Ctrl+I), drop, command line, double click,
//! recent and folders all call [`request`]. The rules are the pure [`decide::decide`]; this
//! module runs the plan one step at a time on the existing paths (`project_ui` for projects,
//! `bigfile` for models, the drawing form, the table preview).
pub mod batch;
pub mod build;
pub mod cache;
pub mod card;
pub mod decide;
pub mod first_view;
pub mod gate;
pub mod known;
pub mod memory;
pub mod names;
pub mod preview;
pub mod summary;
pub mod window;

use crate::shell::EditorApp;
use card::{Card, Choice};
use ddc_ui::notify::Level;
use decide::{Act, Ctx, Door, Plan, Step};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

#[derive(Default)]
pub struct State {
    queue: VecDeque<Step>,
    /// A model waits for the new project it goes into: (file, revision before the request).
    after_new: Option<(PathBuf, u64)>,
    /// The project a queued add-on must find open before it runs.
    expect: Option<PathBuf>,
    /// A model file that changed since its project was made (the toast's "Import again").
    changed: Option<PathBuf>,
    card: Option<Card>,
    /// Command line files after the first one (the start screen keeps the first).
    pub rest_args: Vec<PathBuf>,
    /// Settings: "also make the default for IFC and RVT".
    pub assoc_defaults: bool,
    assoc: Option<(mpsc::Receiver<Result<(), String>>, bool)>,
    /// The running association job came from the ERP link: silent unless it fails.
    pub assoc_quiet: bool,
    pub mem: memory::State,
    pub early: preview::Early,
    pub sum: summary::State,
    pub views: first_view::Store,
}

impl State {
    /// A choice card is up.
    pub fn card_up(&self) -> bool {
        self.card.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing opened; a message says why.
    Nothing,
    /// A project opens (it may still ask about unsaved work).
    Project,
    /// A model or a form started.
    Started,
    /// The question card is up.
    Card,
}

const ALL: [&str; 15] = [
    "ddc", "ifc", "ifczip", "rvt", "rfa", "glb", "gltf", "step", "stp", "stl", "obj", "dwg",
    "dxf", "pdf", "xlsx",
];
const MESHES: [&str; 6] = ddc_io::meshx::READ_EXTS;
const MODELS: [&str; 4] = ["ifc", "ifczip", "rvt", "rfa"];
const DRAWINGS: [&str; 3] = ["dwg", "dxf", "pdf"];

/// A new untouched project, or the start screen.
fn is_empty(app: &EditorApp) -> bool {
    app.chrome.start.open || (app.ed.project.path().is_none() && !app.ed.is_dirty())
}

/// What runs now, for the one busy gate.
pub fn jobs(app: &EditorApp) -> gate::Jobs {
    gate::Jobs {
        big: app.big.busy(),
        // a running save counts: replacing the model under it would lose the write
        project: app.project_ui.busy() || crate::save::busy(app),
        import: app.rvt_import_running(),
        modal: app.project_ui.modal(),
    }
}

fn busy(app: &EditorApp) -> bool {
    jobs(app).busy()
}

fn toast(app: &mut EditorApp, level: Level, text: String) {
    app.sh.toasts.push(level, text, None);
}

fn note(app: &mut EditorApp, level: Level, key: &str) {
    let msg = app.sh.cat.t(key);
    toast(app, level, msg);
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Every door calls this. `inputs` are files and folders as given.
pub fn request(app: &mut EditorApp, inputs: Vec<PathBuf>, door: Door) -> Outcome {
    // an ERP link file is not a model: the link panel takes it
    #[cfg(feature = "link")]
    let inputs = {
        let n = inputs.len();
        let rest = crate::link::panel::take_links(app, inputs);
        if rest.is_empty() && n > 0 {
            return Outcome::Started;
        }
        rest
    };
    start_request(app, inputs, door, false, true)
}

fn start_request(
    app: &mut EditorApp,
    inputs: Vec<PathBuf>,
    door: Door,
    sub: bool,
    report: bool,
) -> Outcome {
    let ex = batch::expand(&inputs, sub);
    if report {
        report_skips(app, &ex);
    }
    if ex.files.is_empty() {
        return Outcome::Nothing;
    }
    let ctx = Ctx {
        empty: is_empty(app),
        door,
        large: ex.is_large(),
    };
    let plan = decide::decide(&ex.files, &ctx);
    if let Some(kind) = plan.ask {
        if busy(app) {
            note(app, Level::Info, "job.busy");
            return Outcome::Nothing;
        }
        app.open.card = Some(Card {
            has_folder: ex.folders > 0,
            count: ex.files.len(),
            bytes: ex.bytes,
            name: ex.files.first().map(|p| name_of(p)).unwrap_or_default(),
            inputs,
            door,
            kind,
            include_sub: sub,
        });
        return Outcome::Card;
    }
    run(app, plan)
}

fn report_skips(app: &mut EditorApp, ex: &batch::Expanded) {
    if let Some(p) = ex.missing.first() {
        let path = p.display().to_string();
        let msg = if ex.missing.len() == 1 {
            app.sh.cat.tf("start.missing", &[("path", path.as_str())])
        } else {
            let n = (ex.missing.len() - 1).to_string();
            app.sh.cat.tf(
                "open_file.missing_many",
                &[("path", path.as_str()), ("n", n.as_str())],
            )
        };
        toast(app, Level::Warning, msg);
    }
    if let Some(p) = ex.unsupported.first() {
        let name = name_of(p);
        let msg = if ex.unsupported.len() == 1 {
            app.sh
                .cat
                .tf("drop.unsupported", &[("name", name.as_str())])
        } else {
            let n = (ex.unsupported.len() - 1).to_string();
            app.sh.cat.tf(
                "open_file.unsupported_many",
                &[("name", name.as_str()), ("n", n.as_str())],
            )
        };
        toast(app, Level::Warning, msg);
    }
}

fn run(app: &mut EditorApp, plan: Plan) -> Outcome {
    if !plan.refused.is_empty() {
        note(app, Level::Info, "open_file.refuse_ddc");
    }
    if plan.tables_left > 0 {
        let n = plan.tables_left.to_string();
        let msg = app.sh.cat.tf("drop.only_first", &[("n", n.as_str())]);
        toast(app, Level::Info, msg);
    }
    let current = app.ed.project.path().map(crate::project::path_key);
    let mut already = false;
    let mut steps = plan.steps;
    steps.retain(|s| {
        let same = s.act == Act::OpenProject
            && current.as_deref() == Some(crate::project::path_key(&s.path).as_str());
        already |= same;
        !same
    });
    if already {
        // reading it again would drop work that is not saved yet
        note(app, Level::Info, "open_file.already_open");
    }
    if steps.is_empty() {
        return if already {
            Outcome::Project
        } else {
            Outcome::Nothing
        };
    }
    let project = steps[0].act == Act::OpenProject;
    if busy(app) || !app.open.queue.is_empty() {
        // behind the running job, in order: a drop is never lost
        let n = steps.len();
        let dropped = gate::enqueue(&mut app.open.queue, steps);
        if dropped > 0 {
            let d = dropped.to_string();
            let msg = app.sh.cat.tf("import_queue.queue_full", &[("n", d.as_str())]);
            toast(app, Level::Warning, msg);
        } else {
            let k = n.to_string();
            let msg = app.sh.cat.tf("import_queue.queued", &[("n", k.as_str())]);
            toast(app, Level::Info, msg);
        }
        return Outcome::Nothing;
    }
    app.open.queue = steps.into();
    app.open.expect = None;
    app.open.after_new = None;
    let mut started = pump(app);
    // a refused first step must not leave the later ones to run behind the open start screen
    while !started && !app.open.queue.is_empty() {
        started = pump(app);
    }
    if project {
        return Outcome::Project;
    }
    if !started {
        // nothing began (no module, say): the start screen stays and the toast has the way out
        return Outcome::Nothing;
    }
    if app.chrome.start.open {
        app.close_start();
    }
    Outcome::Started
}

/// No module for this format: a message with the way to get one, not a dead end.
pub fn no_module(app: &mut EditorApp) {
    let text = app.sh.cat.t("open_file.no_module");
    let action = app.sh.cat.t("open_file.no_module_action");
    app.sh
        .toasts
        .push_cmd(Level::Warning, text, action, "mgr.import_cad");
}

/// Starts the next queued step once nothing else is running or asking.
/// False only when the step it ran refused to start.
fn pump(app: &mut EditorApp) -> bool {
    if app.open.card.is_some() || app.open.after_new.is_some() || busy(app) {
        return true;
    }
    let Some(step) = app.open.queue.pop_front() else {
        return true;
    };
    if let Some(want) = app.open.expect.take() {
        let have = app.ed.project.path().map(crate::project::path_key);
        if have.as_deref() != Some(crate::project::path_key(&want).as_str()) {
            // the project did not open (answer was Cancel, or it failed): the rest does not apply
            app.open.queue.clear();
            return true;
        }
    }
    exec(app, step)
}

fn exec(app: &mut EditorApp, step: Step) -> bool {
    let Step { path, act } = step;
    match act {
        Act::OpenProject => {
            if !app.open.queue.is_empty() {
                app.open.expect = Some(path.clone());
            }
            crate::project_ui::request_open(app, path);
        }
        Act::OpenNew => return open_new(app, path),
        Act::Add => crate::bigfile::start(app, path),
        Act::Underlay => {
            let mut files = vec![path];
            while let Some(i) = app.open.queue.iter().position(|s| s.act == Act::Underlay) {
                if let Some(s) = app.open.queue.remove(i) {
                    files.push(s.path);
                }
            }
            app.open_drawing_files(files);
        }
        Act::Preview => app.import_any(&path),
        Act::NewProcess => {
            if let Err(e) = spawn_process(&path) {
                let name = name_of(&path);
                let msg = app.sh.cat.tf(
                    "open_file.spawn_failed",
                    &[("name", name.as_str()), ("err", e.to_string().as_str())],
                );
                toast(app, Level::Warning, msg);
            }
        }
    }
    true
}

/// A model becomes a new project, built on a worker; the current project is replaced only when
/// the build is ready. A file that is already the source of a recent project opens that project.
/// False when nothing started.
fn open_new(app: &mut EditorApp, path: PathBuf) -> bool {
    if decide::ext(&path) == "rfa" {
        // a component is not a project: it goes into an open one, otherwise say what opens
        if app.ed.project.path().is_some() {
            crate::bigfile::start(app, path);
            return true;
        }
        note(app, Level::Warning, "open_file.rfa_needs_project");
        return false;
    }
    let projects: Vec<PathBuf> = app
        .prefs
        .recent_projects
        .iter()
        .map(PathBuf::from)
        .collect();
    if let Some((p, fit)) = known::find(&path, &projects) {
        if fit == known::Fit::Changed {
            offer_again(app, &path);
        }
        open_known(app, p);
        return true;
    }
    // the unsaved-work question comes before the build, so Cancel never drops a finished one;
    // false when nothing started and nothing was asked, so the start screen stays
    crate::project_ui::request_build(app, path)
}

/// The file was updated after that project was made: the project opens, and one click
/// builds a new one from the file as it is now.
fn offer_again(app: &mut EditorApp, file: &Path) {
    let name = name_of(file);
    let text = app.sh.cat.tf("import_queue.changed", &[("name", name.as_str())]);
    let action = app.sh.cat.t("import_queue.import_again");
    app.open.changed = Some(file.to_path_buf());
    app.sh
        .toasts
        .push_cmd(Level::Warning, text, action, "file.import_again");
}

/// Command `file.import_again`: build from the changed file, skipping the project lookup.
pub fn h_import_again(app: &mut EditorApp) {
    if let Some(p) = app.open.changed.take() {
        crate::project_ui::request_build(app, p);
    }
}

fn open_known(app: &mut EditorApp, p: PathBuf) {
    let name = name_of(&p);
    if app.ed.project.path().is_some_and(|c| c == p.as_path()) {
        let msg = app.sh.cat.tf("open_guard.already_open", &[("name", name.as_str())]);
        toast(app, Level::Info, msg);
        return;
    }
    if let Some(pid) = window::holder(&p) {
        if window::raise(pid) {
            return;
        }
    }
    let msg = app
        .sh
        .cat
        .tf("open_guard.known_project", &[("name", name.as_str())]);
    toast(app, Level::Info, msg);
    crate::project_ui::request_open(app, p);
}

fn spawn_process(path: &Path) -> std::io::Result<()> {
    std::process::Command::new(std::env::current_exe()?)
        .arg(path)
        .spawn()
        .map(|_| ())
}

/// Per frame (called from `project_seam::frame`): the question card, the new-project wait, the
/// queue and the association job.
pub fn frame(app: &mut EditorApp, ctx: &egui::Context) {
    poll_assoc(app);
    let working = app.big.busy();
    memory::tick(app, working);
    let writing = app.big.building();
    app.open.early.poll(ctx, writing);
    if writing {
        ctx.request_repaint_after(preview::EVERY);
    }
    if let Some(mut c) = app.open.card.take() {
        let was = c.include_sub;
        match card::show(ctx, &mut c, &app.sh.cat) {
            Choice::Wait => {
                if c.include_sub != was {
                    let ex = batch::expand(&c.inputs, c.include_sub);
                    c.count = ex.files.len();
                    c.bytes = ex.bytes;
                }
                app.open.card = Some(c);
            }
            Choice::Cancel => {}
            choice => confirm(app, &c, choice == Choice::Secondary),
        }
    }
    if let Some((path, rev)) = app.open.after_new.take() {
        if busy(app) {
            app.open.after_new = Some((path, rev));
        } else if app.ed.revision != rev {
            crate::bigfile::start(app, path);
        } else {
            // the unsaved-work prompt was answered with Cancel
            app.open.queue.clear();
        }
    }
    pump(app);
    if !app.open.queue.is_empty() || app.open.after_new.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

fn confirm(app: &mut EditorApp, c: &Card, add: bool) {
    let ex = batch::expand(&c.inputs, c.include_sub);
    let ctx = Ctx {
        empty: is_empty(app),
        door: c.door,
        large: false,
    };
    let mut plan = decide::decide(&ex.files, &ctx);
    plan.ask = None;
    if add {
        plan.as_add();
    }
    run(app, plan);
}

/// File dialog with the filters of section 2.1: all supported first, then per family.
pub fn pick(app: &mut EditorApp, door: Door) -> Outcome {
    let c = &app.sh.cat;
    let all: Vec<&str> = if door == Door::Import {
        ALL.iter().copied().filter(|e| *e != "ddc").collect()
    } else {
        ALL.to_vec()
    };
    let mut d = crate::file_pick::Dlg::new().add_filter(c.t("open_file.filter.all"), &all[..]);
    if door != Door::Import {
        d = d.add_filter(c.t("open_file.filter.project"), &["ddc"]);
    }
    let d = d
        .add_filter(c.t("open_file.filter.model"), &MODELS)
        .add_filter(c.t("open_file.filter.mesh"), &MESHES)
        .add_filter(c.t("open_file.filter.drawing"), &DRAWINGS)
        .add_filter(c.t("open_file.filter.table"), &["xlsx"]);
    if crate::shot_press::native_blocked() {
        // screenshot mode: show the in-app import window instead of a native dialog
        if door == Door::Import {
            app.run("mgr.import_cad");
            app.chrome.start.open = false;
            app.welcome_closed = true;
        }
        return Outcome::Nothing;
    }
    match crate::shot_press::guard(|| d.pick_files()) {
        Some(files) if !files.is_empty() => request(app, files, door),
        _ => Outcome::Nothing,
    }
}

pub fn h_open(app: &mut EditorApp) {
    pick(app, Door::OpenDialog);
}

pub fn h_import(app: &mut EditorApp) {
    pick(app, Door::Import);
}

pub fn register(app: &mut EditorApp) {
    app.register_command("file.import", h_import);
    app.register_command("file.import_review", summary::h_review);
    app.register_command("file.import_again", h_import_again);
}

// ---- first Ctrl+S of an imported project ------------------------------------------------

/// `<dir>/<stem>.ddc`, then `<stem> (2).ddc`, ...: the first name `taken` does not claim.
pub fn free_name(dir: &Path, stem: &str, taken: &dyn Fn(&Path) -> bool) -> PathBuf {
    let ext = ddc_model::PROJECT_EXT;
    let first = dir.join(format!("{stem}.{ext}"));
    if !taken(&first) {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).{ext}")))
        .find(|p| !taken(p))
        .unwrap_or(first)
}

fn writable_dir(dir: &Path) -> bool {
    let probe = dir.join(format!(".ddc_write_probe_{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            // our own probe file
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Where the first Ctrl+S of an untitled project that came from a file goes, with no dialog:
/// beside that file. `None` means the usual name dialog.
pub fn first_save_target(app: &EditorApp) -> Option<PathBuf> {
    let p = &app.ed.project;
    // a copy restored for a damaged file always asks, seeded with `<name> (recovered)`
    if p.path().is_some() || p.read_only() || p.recovered_from().is_some() {
        return None;
    }
    let src = app
        .ed
        .model
        .sources()
        .ok()?
        .into_iter()
        .find(|s| !s.path.is_empty())?;
    let src = PathBuf::from(src.path);
    let dir = src.parent().filter(|d| d.is_dir())?;
    let stem = src.file_stem()?.to_string_lossy().into_owned();
    if !writable_dir(dir) {
        return None;
    }
    Some(free_name(dir, &stem, &|p| p.exists()))
}

/// Toast after the save began: the place, and the way to change it.
pub fn saved_as_notice(app: &mut EditorApp, target: &Path) {
    let name = name_of(target);
    let text = app.sh.cat.tf("open_file.saved_as", &[("name", name.as_str())]);
    let action = app.sh.cat.t("open_file.change_location");
    app.sh
        .toasts
        .push_cmd(Level::Success, text, action, "file.save_as");
}

/// A registration job is running (or its result is not yet shown).
pub fn assoc_busy(app: &EditorApp) -> bool {
    app.open.assoc.is_some()
}

// ---- file associations: the Settings buttons, and `.ddclink` when the ERP link is on -----------------------------------

pub fn assoc_start(app: &mut EditorApp, req: crate::os_assoc::Req) {
    if app.open.assoc.is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel();
    let register = req.register;
    let tx_panic = tx.clone();
    // touches the OS registration only, never the model
    let _ = crate::jobs::spawn_guarded(
        "os-assoc",
        move || {
            let _ = tx.send(crate::os_assoc::apply(&req));
        },
        move |msg| {
            let _ = tx_panic.send(Err(msg));
        },
    );
    app.open.assoc = Some((rx, register));
}

fn poll_assoc(app: &mut EditorApp) {
    let Some((rx, register)) = &app.open.assoc else {
        return;
    };
    let register = *register;
    let got = match rx.try_recv() {
        Ok(r) => Some(r),
        Err(mpsc::TryRecvError::Empty) => return,
        Err(mpsc::TryRecvError::Disconnected) => Some(Err(crate::jobs::internal_error())),
    };
    app.open.assoc = None;
    let quiet = std::mem::take(&mut app.open.assoc_quiet);
    match got {
        Some(Ok(())) => {
            let key = if register {
                "open_file.assoc.done"
            } else {
                "open_file.assoc.removed"
            };
            if !quiet {
                note(app, Level::Success, key);
            }
        }
        Some(Err(e)) => {
            let msg = app.sh.cat.tf("open_file.assoc.failed", &[("err", e.as_str())]);
            toast(app, Level::Warning, msg);
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_free_name() {
        let dir = Path::new("d");
        let taken = |set: &'static [&'static str]| {
            move |p: &Path| set.iter().any(|s| p == Path::new("d").join(s))
        };
        assert_eq!(free_name(dir, "House", &taken(&[])), dir.join("House.ddc"));
        assert_eq!(
            free_name(dir, "House", &taken(&["House.ddc"])),
            dir.join("House (2).ddc")
        );
        assert_eq!(
            free_name(dir, "House", &taken(&["House.ddc", "House (2).ddc"])),
            dir.join("House (3).ddc")
        );
    }

    fn test_app() -> EditorApp {
        EditorApp::new(
            crate::editor::Editor::new_in_memory().unwrap(),
            None,
            "test".into(),
        )
    }

    #[test]
    fn open_refused_while_import_job_runs() {
        let d = std::env::temp_dir().join(format!("ddc_open_busy_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("a.ifc");
        std::fs::write(&f, b"x").unwrap();
        let mut app = test_app();
        app.chrome.start.open = false;
        app.import_job = Some(crate::import::Job::demo(PathBuf::from("b.rvt"), (1, 2)));
        assert!(app.io_busy());
        let out = request(&mut app, vec![f.clone()], Door::OpenDialog);
        assert!(matches!(out, Outcome::Nothing));
        assert!(app.import_job.is_some());
        assert!(!app.big.busy());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn import_refused_while_open_build_runs() {
        let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut app = test_app();
        app.big.set_open(build::parked_task(release.clone()), None);
        assert!(app.io_busy());
        for name in ["a.ifc", "a.rvt", "a.dxf", "a.xlsx"] {
            app.ed.status = None;
            app.import_any(Path::new(name));
            assert_eq!(app.ed.status, Some(crate::tr("job.busy", &[])), "{name}");
            assert!(app.import_job.is_none(), "{name}");
        }
        release.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[test]
    fn probe_leaves_nothing_behind() {
        let d = std::env::temp_dir().join(format!("ddc_open_probe_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(writable_dir(&d));
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
        assert!(!writable_dir(&d.join("missing")));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn start_app() -> EditorApp {
        let mut a = EditorApp::new(crate::editor::Editor::new_in_memory().unwrap(), None, "test".into());
        a.chrome.start.open = true;
        a
    }

    fn one(path: &str) -> Plan {
        Plan {
            steps: vec![Step {
                path: PathBuf::from(path),
                act: Act::OpenNew,
            }],
            ..Plan::default()
        }
    }

    // same refusal route as a missing .rvt module: build::start answers false
    #[test]
    fn no_module_keeps_start_screen() {
        let mut a = start_app();
        let out = run(&mut a, one("zz_none.unknownext"));
        assert!(matches!(out, Outcome::Nothing));
        assert!(a.chrome.start.open);
    }

    #[test]
    fn refused_first_step_runs_next_and_closes_start() {
        let mut a = start_app();
        let plan = Plan {
            steps: vec![
                Step { path: PathBuf::from("part.rfa"), act: Act::OpenNew },
                Step { path: PathBuf::from("zz_none.ifc"), act: Act::Add },
            ],
            ..Plan::default()
        };
        let out = run(&mut a, plan);
        assert!(matches!(out, Outcome::Started));
        assert!(a.open.queue.is_empty());
        assert!(!a.chrome.start.open);
    }

    #[test]
    fn rfa_on_empty_says_supported_formats() {
        let mut a = start_app();
        assert!(matches!(run(&mut a, one("part.rfa")), Outcome::Nothing));
        assert!(a.chrome.start.open);
        assert_eq!(a.sh.toasts.live.len(), 1);
        assert_eq!(a.sh.toasts.history[0].0, Level::Warning);
    }
}
