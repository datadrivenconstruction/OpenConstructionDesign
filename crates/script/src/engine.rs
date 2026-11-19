// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The sandboxed interpreter: string, table and math libraries only (no files, network,
//! clock, random numbers, code loading or address-revealing text), a heap limit, and a hook
//! counting executed lines and checking CPU time. Errors map to script line and column.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use mlua::debug::DebugEvent;
use mlua::{HookTriggers, Lua, LuaOptions, StdLib, Value, Variadic, VmState};

use crate::emit::Builder;
use crate::hint;
use crate::stdlib::{self, sorted_pairs};
use crate::{hash_of, ErrorKind, LimitKind, RunOpts, ScriptError, ScriptRun, Stats, ELEMENTS_HARD};

/// VM instructions between CPU and heap checks.
const CHECK_EVERY: u32 = 4096;
const MAX_SCRIPT: usize = 256 << 10;

#[derive(Default)]
struct Guard {
    ops: u64,
    line: u32,
    limit: Option<LimitKind>,
    mem_peak: usize,
}

#[cfg(windows)]
fn thread_cpu() -> Duration {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
    let z = || FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut c, mut e, mut k, mut u) = (z(), z(), z(), z());
    // SAFETY: the pseudo handle of the current thread is always valid; out params are owned.
    let ok = unsafe { GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u) };
    if ok == 0 {
        return Duration::ZERO;
    }
    let t = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    Duration::from_nanos((t(k) + t(u)) * 100)
}

#[cfg(not(windows))]
fn thread_cpu() -> Duration {
    thread_local!(static T0: web_time::Instant = web_time::Instant::now());
    T0.with(|t| t.elapsed())
}

fn sandbox(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();
    for k in [
        "dofile",
        "loadfile",
        "load",
        "loadstring",
        "require",
        "collectgarbage",
        "next",
    ] {
        g.raw_set(k, Value::Nil)?;
    }
    let math: mlua::Table = g.get("math")?;
    math.raw_set("random", Value::Nil)?;
    math.raw_set("randomseed", Value::Nil)?;
    let string: mlua::Table = g.get("string")?;
    string.raw_set("dump", Value::Nil)?;
    let fmt: mlua::Function = string.get("format")?;
    string.raw_set(
        "format",
        lua.create_function(move |lua, args: Variadic<Value>| {
            if let Some(Value::String(s)) = args.first() {
                check_format(&s.to_str()?)?;
            }
            // `%s` of a table or function would print an address, which differs per run
            let args = args
                .into_iter()
                .map(|a| match a {
                    Value::Table(_)
                    | Value::Function(_)
                    | Value::Thread(_)
                    | Value::LightUserData(_) => {
                        Ok(Value::String(lua.create_string(a.type_name())?))
                    }
                    other => Ok(other),
                })
                .collect::<mlua::Result<Variadic<Value>>>()?;
            fmt.call::<Value>(args)
        })?,
    )?;
    for name in ["find", "match", "gmatch", "gsub"] {
        let f: mlua::Function = string.get(name)?;
        string.raw_set(
            name,
            lua.create_function(move |_, args: Variadic<Value>| {
                check_pattern(name, &args)?;
                f.call::<mlua::MultiValue>(args)
            })?,
        )?;
    }
    // a finalizer would run while the interpreter closes, after the limits are lifted
    let setmt: mlua::Function = g.get("setmetatable")?;
    g.raw_set(
        "setmetatable",
        lua.create_function(move |_, (t, m): (Value, Value)| {
            if let Value::Table(mt) = &m {
                if !matches!(mt.raw_get::<Value>("__gc")?, Value::Nil) {
                    return Err(mlua::Error::RuntimeError(
                        "setmetatable: __gc finalizers are not available in scripts".into(),
                    ));
                }
            }
            setmt.call::<Value>((t, m))
        })?,
    )?;
    let tostring: mlua::Function = g.get("tostring")?;
    let safe_tostring = lua.create_function(move |_, v: Value| match &v {
        Value::Table(_) | Value::Function(_) | Value::Thread(_) | Value::LightUserData(_) => {
            Ok(v.type_name().to_string())
        }
        _ => {
            let s = tostring.call::<String>(v)?;
            // a userdata without a name of its own would print its address
            Ok(if s.starts_with("userdata: ") { "userdata".to_string() } else { s })
        }
    })?;
    g.raw_set("tostring", safe_tostring.clone())?;
    g.raw_set(
        "print",
        lua.create_function(move |lua, args: Variadic<Value>| {
            let mut parts = Vec::with_capacity(args.len());
            for a in args {
                parts.push(safe_tostring.call::<String>(a)?);
            }
            stdlib::builder(lua)?.print(parts.join("	"));
            Ok(())
        })?,
    )?;
    g.raw_set("pairs", lua.create_function(sorted_pairs)?)?;
    Ok(())
}

/// `string.format` takes specifiers `%[flags][width][.precision]conv`; `%p` prints an address.
fn check_format(f: &str) -> mlua::Result<()> {
    let b = f.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            i += 1;
            continue;
        }
        i += 1;
        while i < b.len() && b"-+ #0123456789.".contains(&b[i]) {
            i += 1;
        }
        if i < b.len() && b[i] == b'p' {
            return Err(mlua::Error::RuntimeError(
                "string.format: %p prints an address and is not allowed".into(),
            ));
        }
        i += 1;
    }
    Ok(())
}

/// Matching runs in C where no limit can interrupt it, so its work is bounded up front:
/// text^exponent must stay within `PATTERN_BUDGET`. The exponent is the number of
/// quantified items (`* + - ?`, `%b`, `%f`), plus one when the pattern is not anchored
/// with `^` (every start position is tried); a pattern without quantifiers is linear.
const PATTERN_BUDGET: f64 = 1e8;
const PATTERN_LEN: usize = 256;

/// Quantified items of a pattern and whether it starts with `^`.
fn pattern_shape(p: &[u8]) -> (u32, bool) {
    let anchored = p.first() == Some(&b'^');
    let (mut q, mut i) = (0, usize::from(anchored));
    while i < p.len() {
        match p[i] {
            b'%' => {
                match p.get(i + 1) {
                    Some(b'b') => {
                        q += 1;
                        i += 2;
                    }
                    Some(b'f') => q += 1,
                    _ => {}
                }
                i += 1;
            }
            b'[' => {
                i += 1;
                if p.get(i) == Some(&b'^') {
                    i += 1;
                }
                // a `]` right after the opening is a member
                if p.get(i) == Some(&b']') {
                    i += 1;
                }
                while i < p.len() && p[i] != b']' {
                    if p[i] == b'%' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'*' | b'+' | b'-' | b'?' => q += 1,
            _ => {}
        }
        i += 1;
    }
    (q, anchored)
}

fn check_pattern(name: &str, args: &[Value]) -> mlua::Result<()> {
    let bad = |m: String| Err(mlua::Error::RuntimeError(format!("string.{name}: {m}")));
    // a plain `find` is a linear search
    if name == "find" && matches!(args.get(3), Some(Value::Boolean(true))) {
        return Ok(());
    }
    let (Some(Value::String(text)), Some(Value::String(pat))) = (args.first(), args.get(1))
    else {
        return Ok(());
    };
    let (text, pat) = (text.as_bytes(), pat.as_bytes());
    if pat.len() > PATTERN_LEN {
        return bad(format!("the pattern is {} bytes; at most {PATTERN_LEN}", pat.len()));
    }
    let (q, anchored) = pattern_shape(&pat);
    let exp = if q == 0 { 1 } else { q + u32::from(!anchored) };
    let cost = (text.len().max(1) as f64).powi(exp as i32);
    if cost > PATTERN_BUDGET {
        let most = PATTERN_BUDGET.powf(1.0 / f64::from(exp)).floor() as u64;
        return bad(format!(
            "the text is {} bytes and the pattern has {q} repeating item(s){}: work grows as \
             text^{exp}, over the budget of 1e8 steps; with this pattern the text may have at \
             most {most} bytes (anchor with ^ or use fewer of * + - ? %b %f to allow more)",
            text.len(),
            if anchored { "" } else { " and is not anchored" },
        ));
    }
    Ok(())
}

fn limit_error(kind: LimitKind, line: u32, opts: &RunOpts) -> ScriptError {
    let l = &opts.limits;
    let message = match kind {
        LimitKind::Cpu => format!("stopped after {} s of CPU time", l.cpu.as_secs_f64()),
        LimitKind::Memory => format!("stopped at the {} MB memory limit", l.memory >> 20),
        LimitKind::Ops => format!("stopped after {} operations", l.ops),
        LimitKind::Elements => format!("more than {} elements", max_elements(opts)),
    };
    ScriptError {
        kind: ErrorKind::Limit(kind),
        line: line.max(1),
        col: 1,
        message,
        hint: hint::limit(kind),
    }
}

fn max_elements(opts: &RunOpts) -> usize {
    if opts.limits.allow_many {
        ELEMENTS_HARD
    } else {
        opts.limits.elements.min(ELEMENTS_HARD)
    }
}

pub(crate) fn run(src: &str, opts: &RunOpts) -> Result<ScriptRun, ScriptError> {
    let internal = |e: mlua::Error| ScriptError {
        kind: ErrorKind::Runtime,
        line: 1,
        col: 1,
        message: format!("internal: {e}"),
        hint: String::new(),
    };
    if src.len() > MAX_SCRIPT {
        return Err(ScriptError {
            kind: ErrorKind::Syntax,
            line: 1,
            col: 1,
            message: format!(
                "script is {} KB; at most {} KB",
                src.len() >> 10,
                MAX_SCRIPT >> 10
            ),
            hint: "use loops, pattern() and components instead of listing every part".into(),
        });
    }
    let params = match &opts.params {
        serde_json::Value::Object(m) => m.clone(),
        serde_json::Value::Null => Default::default(),
        _ => {
            return Err(ScriptError {
                kind: ErrorKind::Param,
                line: 1,
                col: 1,
                message: "params must be an object {name: value}".into(),
                hint: hint::PARAMS.into(),
            })
        }
    };
    let lua = Lua::new_with(
        StdLib::STRING | StdLib::TABLE | StdLib::MATH,
        LuaOptions::default(),
    )
    .map_err(internal)?;
    lua.set_app_data(Builder {
        level: Some(opts.level.clone()),
        params_in: params,
        max_elements: max_elements(opts),
        ..Builder::default()
    });
    sandbox(&lua).map_err(internal)?;
    stdlib::install(&lua).map_err(internal)?;
    lua.set_memory_limit(opts.limits.memory).map_err(internal)?;

    let guard = Rc::new(RefCell::new(Guard::default()));
    let (cpu0, cpu_max, max_ops) = (thread_cpu(), opts.limits.cpu, opts.limits.ops);
    let hg = guard.clone();
    let triggers = HookTriggers::new()
        .every_line()
        .every_nth_instruction(CHECK_EVERY);
    lua.set_hook(triggers, move |lua, dbg| {
        let mut g = hg.borrow_mut();
        let stop = |g: &mut Guard, k: LimitKind| {
            g.limit = Some(k);
            Err(mlua::Error::RuntimeError(format!("limit: {k:?}")))
        };
        if g.limit.is_some() {
            let k = g.limit.unwrap_or(LimitKind::Ops);
            return stop(&mut g, k);
        }
        match dbg.event() {
            DebugEvent::Line => {
                g.ops += 1;
                if let Some(l) = dbg.current_line() {
                    g.line = l as u32;
                    if let Some(mut b) = lua.app_data_mut::<Builder>() {
                        b.line = l as u32;
                    }
                }
                if g.ops > max_ops {
                    return stop(&mut g, LimitKind::Ops);
                }
            }
            _ => {
                g.mem_peak = g.mem_peak.max(lua.used_memory());
                let credit = lua.app_data_ref::<Builder>().map(|b| b.cpu_credit).unwrap_or_default();
                if thread_cpu().saturating_sub(cpu0).saturating_sub(credit) > cpu_max {
                    return stop(&mut g, LimitKind::Cpu);
                }
            }
        }
        Ok(VmState::Continue)
    })
    .map_err(internal)?;

    let res = lua.load(src).set_name("=script").exec();
    lua.remove_hook();
    let cpu = thread_cpu().saturating_sub(cpu0);
    let g = guard.borrow();
    let b = lua
        .remove_app_data::<Builder>()
        .ok_or_else(|| internal(mlua::Error::RuntimeError("builder lost".into())))?;
    if let Some(k) = g.limit.or(b.limit) {
        return Err(limit_error(k, g.line, opts));
    }
    if let Err(e) = res {
        if matches!(e, mlua::Error::MemoryError(_)) {
            return Err(limit_error(LimitKind::Memory, g.line, opts));
        }
        return Err(hint::map_error(&e, src, g.line));
    }
    let declared: Vec<&str> = b.params.iter().map(|p| p.name.as_str()).collect();
    if let Some(k) = b.params_in.keys().find(|k| !declared.contains(&k.as_str())) {
        return Err(ScriptError {
            kind: ErrorKind::Param,
            line: 1,
            col: 1,
            message: format!("`{k}` is not a parameter of this script"),
            hint: format!("declared params: {}", declared.join(", ")),
        });
    }
    if b.ops.is_empty() {
        return Err(ScriptError {
            kind: ErrorKind::Runtime,
            line: g.line.max(1),
            col: 1,
            message: "the script created nothing".into(),
            hint: "call part(name, form) for every element to build".into(),
        });
    }
    let parts = b.keys();
    let env = b.envelope(opts.actor.clone(), &opts.title);
    let hash = hash_of(&env, &parts);
    Ok(ScriptRun {
        env,
        parts,
        params: b.params,
        measures: b.measures,
        constraints: b.constraints,
        log: b.log,
        hash,
        stats: Stats {
            ops: g.ops,
            cpu_ms: cpu.as_secs_f64() * 1e3,
            memory_peak: g.mem_peak.max(lua.used_memory()),
            elements: b.elements,
        },
    })
}
