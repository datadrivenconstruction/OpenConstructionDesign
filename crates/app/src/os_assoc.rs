//! File associations for the current user (no admin rights): `.ddc` is claimed, the other formats
//! are listed under "Open with", and IFC, RVT and RFA become the default only when asked.
//! Nothing here runs by itself: it is a user action (Settings, an installer option, or
//! `ddc-app --register-files [--default-models]` / `--unregister-files` typed by hand).

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Req {
    /// `true` registers, `false` removes what registering wrote.
    pub register: bool,
    /// Also make the editor the default program for IFC, RVT and RFA.
    pub default_models: bool,
}

const PROJECT_ID: &str = "DDC.Project";
const FILE_ID: &str = "DDC.File";
/// ERP link file (`.ddclink`): claimed only by a build with the link.
const LINK_ID: &str = "DDC.Link";
const CLASSES: &str = r"HKCU\Software\Classes";
/// Listed under "Open with" only.
const OPEN_WITH: [&str; 8] = ["ifc", "ifczip", "rvt", "rfa", "dwg", "dxf", "pdf", "xlsx"];
/// Become the default only on request.
const MODEL_DEFAULT: [&str; 4] = ["ifc", "ifczip", "rvt", "rfa"];

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn prog_id(id: &str, label: &str, exe: &str) -> Vec<Vec<String>> {
    let key = format!(r"{CLASSES}\{id}");
    let command = format!("\"{exe}\" \"%1\"");
    let icon = format!("\"{exe}\",0");
    vec![
        args(&["add", &key, "/ve", "/d", label, "/f"]),
        args(&[
            "add",
            &format!(r"{key}\DefaultIcon"),
            "/ve",
            "/d",
            &icon,
            "/f",
        ]),
        args(&[
            "add",
            &format!(r"{key}\shell\open\command"),
            "/ve",
            "/d",
            &command,
            "/f",
        ]),
    ]
}

/// The `reg` argument lists that register, in order.
pub fn register_plan(exe: &str, default_models: bool) -> Vec<Vec<String>> {
    let mut v = prog_id(PROJECT_ID, "OpenConstructionDesign project", exe);
    v.extend(prog_id(FILE_ID, crate::brand::PRODUCT, exe));
    v.push(args(&[
        "add",
        &format!(r"{CLASSES}\.ddc"),
        "/ve",
        "/d",
        PROJECT_ID,
        "/f",
    ]));
    if cfg!(feature = "link") {
        v.extend(prog_id(LINK_ID, "DDC Editor ERP link", exe));
        v.push(args(&["add", &format!(r"{CLASSES}\.ddclink"), "/ve", "/d", LINK_ID, "/f"]));
    }
    for e in OPEN_WITH {
        let key = format!(r"{CLASSES}\.{e}\OpenWithProgids");
        v.push(args(&["add", &key, "/v", FILE_ID, "/t", "REG_NONE", "/f"]));
    }
    if default_models {
        for e in MODEL_DEFAULT {
            v.push(args(&[
                "add",
                &format!(r"{CLASSES}\.{e}"),
                "/ve",
                "/d",
                FILE_ID,
                "/f",
            ]));
        }
    }
    v
}

/// The `reg` argument lists that remove it. A default is removed only when it is still ours:
/// the caller runs `owns_default` queries first.
pub fn remove_plan(owned_defaults: &[&str]) -> Vec<Vec<String>> {
    let mut v = vec![
        args(&["delete", &format!(r"{CLASSES}\{PROJECT_ID}"), "/f"]),
        args(&["delete", &format!(r"{CLASSES}\{FILE_ID}"), "/f"]),
        args(&["delete", &format!(r"{CLASSES}\.ddc"), "/f"]),
    ];
    // a build with the link registered these; removing missing keys is a no-op
    v.push(args(&["delete", &format!(r"{CLASSES}\{LINK_ID}"), "/f"]));
    v.push(args(&["delete", &format!(r"{CLASSES}\.ddclink"), "/f"]));
    for e in OPEN_WITH {
        let key = format!(r"{CLASSES}\.{e}\OpenWithProgids");
        v.push(args(&["delete", &key, "/v", FILE_ID, "/f"]));
    }
    for e in owned_defaults {
        v.push(args(&["delete", &format!(r"{CLASSES}\.{e}"), "/ve", "/f"]));
    }
    v
}

#[cfg(windows)]
mod win {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const NO_WINDOW: u32 = 0x0800_0000;

    pub fn reg(a: &[String]) -> Result<String, String> {
        let out = Command::new("reg")
            .args(a)
            .creation_flags(NO_WINDOW)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHChangeNotify(
            event: i32,
            flags: u32,
            a: *const core::ffi::c_void,
            b: *const core::ffi::c_void,
        );
    }

    /// Explorer re-reads associations.
    pub fn notify() {
        // SHCNE_ASSOCCHANGED, SHCNF_IDLIST
        unsafe { SHChangeNotify(0x0800_0000, 0, core::ptr::null(), core::ptr::null()) }
    }
}

/// Runs the request. Registering stops at the first failure; removing ignores missing keys.
#[cfg(windows)]
pub fn apply(req: &Req) -> Result<(), String> {
    if req.register {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe = exe.to_string_lossy().into_owned();
        for a in register_plan(&exe, req.default_models) {
            win::reg(&a)?;
        }
    } else {
        let mut owned = Vec::new();
        for e in MODEL_DEFAULT {
            let q = args(&["query", &format!(r"{CLASSES}\.{e}"), "/ve"]);
            if win::reg(&q).is_ok_and(|s| s.contains(FILE_ID)) {
                owned.push(e);
            }
        }
        for a in remove_plan(&owned) {
            let _ = win::reg(&a);
        }
    }
    win::notify();
    Ok(())
}

#[cfg(not(windows))]
pub fn apply(_: &Req) -> Result<(), String> {
    Err("file associations are set up for Windows only".into())
}

/// `--register-files [--default-models]` and `--unregister-files`: the exit code, or `None` for
/// a normal start.
pub fn handle_cli(argv: &[String]) -> Option<i32> {
    let has = |f: &str| argv.iter().skip(1).any(|a| a == f);
    let register = has("--register-files");
    if !register && !has("--unregister-files") {
        return None;
    }
    let req = Req {
        register,
        default_models: register && has("--default-models"),
    };
    Some(match apply(&req) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Program Files\DDC\ddc-app.exe";

    fn text(plan: &[Vec<String>]) -> String {
        plan.iter()
            .map(|a| a.join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn registering_claims_ddc_and_lists_the_rest() {
        let t = text(&register_plan(EXE, false));
        assert!(t.contains(r"HKCU\Software\Classes\.ddc /ve /d DDC.Project"));
        assert!(t.contains(r"\shell\open\command /ve /d "));
        assert!(t.contains("\"C:\\Program Files\\DDC\\ddc-app.exe\" \"%1\""));
        for e in OPEN_WITH {
            assert!(
                t.contains(&format!(r".{e}\OpenWithProgids /v DDC.File")),
                "{e}"
            );
        }
    }

    #[test]
    fn other_defaults_only_when_asked() {
        let quiet = text(&register_plan(EXE, false));
        for e in MODEL_DEFAULT {
            assert!(!quiet.contains(&format!(r"Classes\.{e} /ve")), "{e}");
        }
        let asked = text(&register_plan(EXE, true));
        for e in MODEL_DEFAULT {
            assert!(
                asked.contains(&format!(r"Classes\.{e} /ve /d DDC.File")),
                "{e}"
            );
        }
        for e in ["pdf", "xlsx", "dwg", "dxf"] {
            assert!(
                !asked.contains(&format!(r"Classes\.{e} /ve")),
                "{e} is never claimed"
            );
        }
    }

    #[test]
    fn removing_touches_only_our_keys() {
        let t = text(&remove_plan(&["ifc"]));
        assert!(t.contains(r"delete HKCU\Software\Classes\DDC.Project /f"));
        assert!(t.contains(r"Classes\.ifc /ve /f"));
        assert!(
            !t.contains(r"Classes\.rvt /ve"),
            "a default that is not ours stays"
        );
        assert!(!t.contains(r"delete HKCU\Software\Classes\.pdf /f"));
    }

    #[test]
    fn ddclink_claimed_only_with_the_link() {
        let t = text(&register_plan(EXE, false));
        assert_eq!(t.contains(r"Classes\.ddclink /ve /d DDC.Link"), cfg!(feature = "link"));
        assert_eq!(t.contains(r"Classes\DDC.Link\shell\open\command"), cfg!(feature = "link"));
        let r = text(&remove_plan(&[]));
        assert!(r.contains(r"delete HKCU\Software\Classes\.ddclink /f"));
        assert!(r.contains(r"delete HKCU\Software\Classes\DDC.Link /f"));
    }

    #[test]
    fn cli_flags() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(handle_cli(&a(&["exe"])), None);
        assert_eq!(handle_cli(&a(&["exe", "x.ddc"])), None);
    }
}
