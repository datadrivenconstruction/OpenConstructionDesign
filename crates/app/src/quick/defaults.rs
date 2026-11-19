//! Smart defaults: the last type and sizes per category (kept between launches) are filled in when
//! a tool starts; a tool started in 3D or a section switches the view to the current level's plan.
use crate::editor::Editor;
use crate::shell::EditorApp;
use crate::wall_tool::Mode;
use ddc_ui::uxstate::UxState;

/// Category of the active tool: `wall`, `door`, `window`, `tool.<kind>`.
pub fn tool_key(ed: &Editor) -> Option<String> {
    if let Some(t) = ed.model_tool() {
        return Some(format!("tool.{}", t.kind.key()));
    }
    if ed.opening_mode {
        return Some(match ed.otool.ty.kind {
            crate::opening_tool::Kind::Door => "door".into(),
            crate::opening_tool::Kind::Window => "window".into(),
            crate::opening_tool::Kind::Void => "void".into(),
        });
    }
    matches!(ed.wall_mode(), Mode::Polyline | Mode::Rect).then(|| "wall".into())
}

/// Once per frame: on tool start apply defaults and switch to plan; while active, remember values.
pub fn track(app: &mut EditorApp) {
    let k = tool_key(&app.ed);
    if k != app.quick.tool_was && k.is_some() {
        if app.view3d || app.section.is_some() {
            app.view3d = false;
            app.section = None;
        }
        restore(&mut app.ed, &app.quick.ux);
    }
    app.quick.tool_was = k;
    remember(app);
}

/// Apply the defaults now (palette arguments are applied on top).
pub fn restore_now(app: &mut EditorApp) {
    let k = tool_key(&app.ed);
    if k.is_some() && k != app.quick.tool_was {
        restore(&mut app.ed, &app.quick.ux);
    }
    app.quick.tool_was = k;
}

pub fn restore(ed: &mut Editor, ux: &UxState) {
    let Some(k) = tool_key(ed) else { return };
    let len = |f: &str| ux.get_f64(&k, f).filter(|v| v.is_finite() && *v >= 0.0);
    match k.as_str() {
        "wall" => {
            if let Some(i) = ux
                .get(&k, "type")
                .and_then(|n| ed.types.iter().position(|t| t.name == n))
            {
                ed.current_type = i;
            }
            if let Some(h) = len("height").filter(|h| *h > 0.0) {
                ed.height = h;
            }
        }
        "door" | "window" | "void" => {
            let ty = &mut ed.otool.ty;
            for (f, v) in [
                ("width", &mut ty.width),
                ("height", &mut ty.height),
                ("sill", &mut ty.sill),
            ] {
                if let Some(x) = len(f) {
                    *v = x;
                }
            }
        }
        _ => {
            let Some(t) = ed.model_tool_mut() else {
                return;
            };
            if let Some(i) = ux
                .get(&k, "type")
                .and_then(|n| t.types.iter().position(|x| x == n))
            {
                t.opts.ty = i;
            }
            let o = &mut t.opts;
            for (f, v) in ext_fields(o) {
                if let Some(x) = len(f) {
                    *v = x;
                }
            }
            if let Some(dn) = ux.get_f64(&k, "dn").filter(|d| *d >= 1.0) {
                o.dn = dn as u32;
            }
        }
    }
}

fn ext_fields(o: &mut crate::tools::Opts) -> [(&'static str, &mut f64); 8] {
    [
        ("top_off", &mut o.top_off),
        ("z", &mut o.z),
        ("height", &mut o.height),
        ("width", &mut o.width),
        ("depth", &mut o.depth),
        ("slope_deg", &mut o.slope_deg),
        ("overhang", &mut o.overhang),
        ("rot_deg", &mut o.rot_deg),
    ]
}

/// Remember the active tool's values (changes mark the state as needing a save).
pub fn remember(app: &mut EditorApp) {
    let ed = &mut app.ed;
    let Some(k) = tool_key(ed) else { return };
    let ux = &mut app.quick.ux;
    let mut ch = false;
    match k.as_str() {
        "wall" => {
            if let Some(t) = ed.types.get(ed.current_type) {
                ch |= ux.set(&k, "type", &t.name);
            }
            ch |= ux.set(&k, "height", ed.height);
        }
        "door" | "window" | "void" => {
            let t = ed.otool.ty;
            ch |= ux.set(&k, "width", t.width);
            ch |= ux.set(&k, "height", t.height);
            ch |= ux.set(&k, "sill", t.sill);
        }
        _ => {
            let Some(t) = ed.model_tool_mut() else {
                return;
            };
            if let Some(n) = t.types.get(t.opts.ty) {
                ch |= ux.set(&k, "type", n);
            }
            ch |= ux.set(&k, "dn", t.opts.dn);
            for (f, v) in ext_fields(&mut t.opts) {
                ch |= ux.set(&k, f, *v);
            }
        }
    }
    if ch {
        app.quick.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_used_values_come_back_and_tool_goes_to_plan() {
        let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        a.run("arch.door");
        track(&mut a);
        assert_eq!(tool_key(&a.ed).as_deref(), Some("door"));
        a.ed.otool.ty.width = 1.25;
        track(&mut a);
        assert_eq!(a.quick.ux.get_f64("door", "width"), Some(1.25));
        a.ed.set_mode(Mode::Select);
        a.ed.opening_mode = false;
        track(&mut a);
        assert_eq!(a.quick.tool_was, None);
        // new session: the same defaults; starting from 3D switches to plan
        let ux = a.quick.ux.clone();
        let mut b = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        b.quick.ux = ux;
        b.view3d = true;
        b.run("arch.door");
        track(&mut b);
        assert!((b.ed.otool.ty.width - 1.25).abs() < 1e-9);
        assert!(!b.view3d);
        // walls: height
        b.ed.set_mode(Mode::Select);
        b.ed.opening_mode = false;
        track(&mut b);
        b.quick.ux.set("wall", "height", 3.3);
        b.run("arch.wall");
        track(&mut b);
        assert!((b.ed.height - 3.3).abs() < 1e-9);
    }
}
