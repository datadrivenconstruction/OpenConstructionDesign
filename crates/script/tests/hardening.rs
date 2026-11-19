// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Holes a script could use to hang, crash or leak run-to-run differences: each one is a
//! plain error with a line and a message, never a dead process.

use std::time::{Duration, Instant};

use ddc_ops::{Cat, Ref};
use ddc_script::{ErrorKind, GeoScript, RunOpts, Sandbox, ScriptError, ScriptRun};
use serde_json::json;

const PAD: &str = "\npart('_pad', extrude(profile.rect(0.1, 0.1), vec3(0,0,0), vec3(1,0,0)))";

fn opts() -> RunOpts {
    RunOpts::new(Ref::name(Cat::Level, "Level 1"), json!({}))
}

fn run(src: &str) -> Result<ScriptRun, ScriptError> {
    Sandbox.run(&format!("{src}{PAD}"), &opts())
}

fn ok(src: &str) -> ScriptRun {
    let a = run(src).unwrap_or_else(|e| panic!("{e}\n{src}"));
    assert_eq!(a.hash, run(src).unwrap().hash, "deterministic: {src}");
    a
}

fn fail(src: &str) -> ScriptError {
    run(src).expect_err("script must fail")
}

#[test]
fn a_list_that_holds_itself_is_refused_not_a_crash() {
    let e = fail("local t = {}\nt[1] = t\ngroup('g', t)");
    assert_eq!((e.kind, e.line), (ErrorKind::Runtime, 3), "{e}");
    assert!(e.message.starts_with("group:") && e.message.contains("nested"), "{e}");
    // two lists pointing at each other
    let e = fail("local a, b = {}, {}\na[1] = b\nb[1] = a\ngroup('g', a)");
    assert!(e.message.contains("nested"), "{e}");
    // honest nesting still works
    ok("local p = part('a', extrude(profile.rect(0.1, 0.1), vec3(0,0,0), vec3(1,0,0)))\n\
        group('g', {{{p}}})");
}

#[test]
fn a_finalizer_cannot_hang_the_shutdown() {
    let t = Instant::now();
    let e = fail("setmetatable({}, {__gc = function() while true do end end})");
    assert_eq!((e.kind, e.line), (ErrorKind::Runtime, 1), "{e}");
    assert!(e.message.contains("__gc"), "{e}");
    assert!(t.elapsed() < Duration::from_secs(5));
    // a metatable without a finalizer still works
    let r = ok("local t = setmetatable({}, {__index = function() return 7 end})\n\
                assert(t.x == 7)");
    assert_eq!(r.parts.len(), 1);
    // adding __gc after the call does not arm a finalizer in Lua, and does not hang either
    let t = Instant::now();
    ok("local mt = {}\nlocal t = setmetatable({}, mt)\nmt.__gc = function() while true do end end");
    assert!(t.elapsed() < Duration::from_secs(5));
}

#[test]
fn many_copies_of_a_big_form_are_refused_before_they_are_made() {
    let mut o = opts();
    o.limits.ops = u64::MAX;
    o.limits.cpu = Duration::from_secs(30);
    let poly = |n: usize, copies: usize| {
        format!(
            "local pts = {{}}\nfor i = 1, {n} do pts[i] = {{math.cos(i), math.sin(i)}} end\n\
             local f = extrude(profile.polygon(pts), vec3(0,0,0), vec3(1,0,0))\n\
             local c = pattern_linear(f, {copies}, vec3(0, 1, 0))\n{PAD}"
        )
    };
    let e = Sandbox.run(&poly(50_000, 100), &o).unwrap_err();
    assert_eq!(e.line, 4, "{e}");
    assert!(e.message.starts_with("pattern:") && e.message.contains("at most"), "{e}");
    // within the budget it runs; a count of 1 is always fine
    let r = Sandbox.run(&poly(50_000, 2), &o).unwrap();
    assert_eq!(r.parts.len(), 1);
    // a polygon over the input cap is refused at the call
    let e = Sandbox
        .run(
            "local pts = {}\nfor i = 1, 100001 do pts[i] = {math.cos(i), math.sin(i)} end\n\
             local p = profile.polygon(pts)",
            &o,
        )
        .unwrap_err();
    assert_eq!(e.line, 3, "{e}");
    assert!(e.message.starts_with("profile.polygon:"), "{e}");
}

#[test]
fn pathological_patterns_are_refused_by_budget_in_microseconds() {
    let t = Instant::now();
    let e = fail("local s = string.rep('a', 100000)
local x = s:find('a-a-b')");
    assert_eq!(e.line, 2, "{e}");
    assert!(e.message.starts_with("string.find:"), "{e}");
    assert!(e.message.contains("text^3") && e.message.contains("1e8"), "{e}");
    assert!(e.message.contains("at most 464 bytes"), "{e}");
    let e = fail("local x = string.rep('a', 1000):find('.-.-.-.-b')");
    assert!(e.message.contains("text^5"), "{e}");
    let e = fail("local x = ('aaa'):match(string.rep('a', 257))");
    assert!(e.message.starts_with("string.match:") && e.message.contains("256"), "{e}");
    let e = fail("for w in string.gmatch(string.rep('a', 20000), 'a-b') do end");
    assert!(e.message.starts_with("string.gmatch:") && e.message.contains("text^2"), "{e}");
    let e = fail("local x = string.gsub(string.rep('a', 20000), 'a+b', 'c')");
    assert!(e.message.starts_with("string.gsub:"), "{e}");
    // %b and %f count as repeating items; ^ saves the start loop
    let e = fail("local x = string.rep('(', 2000):find('%b()%b()%b()')");
    assert!(e.message.contains("3 repeating"), "{e}");
    let e = fail("local x = string.rep('a', 20000):find('^a-a-a-b')");
    assert!(e.message.contains("text^3"), "{e}");
    assert!(t.elapsed() < Duration::from_secs(5));
}

#[test]
fn long_texts_with_simple_patterns_pass() {
    // no repeating item: linear in the text, any length the memory limit allows
    ok("local s = string.rep('ab', 500000)
        assert(s:find('ba', 1) == 2)
        assert(s:find('x') == nil)
        assert(s:find('b', 1, true) == 2)");
    // anchored, one repeating item: text^1
    ok("local s = '12345' .. string.rep('a', 1000000)
assert(s:match('^%d+') == '12345')");
    // unanchored, one item: up to 10000 bytes
    ok("local s = string.rep('a', 9000) .. 'key=value'
assert(s:match('key=(%w+)') == 'value')");
    // two items, anchored: 10000 bytes
    ok("local s = 'x=1;' .. string.rep('b', 9000)
        assert(s:match('^(%w+)=(%d+)') == 'x')");
    // everyday use
    ok("assert(('hello world'):find('o w') == 5)
        assert(select(2, ('a-b-c'):gsub('-', '_')) == 2)
        assert(('key=value'):match('(%w+)=(%w+)') == 'key')
        assert(('[x]'):find('[%[]') == 1)");
}

#[test]
fn format_specifiers_are_parsed_not_searched() {
    for f in ["%p", "%5p", "%-10p", "%.3p", "%05p", "x %% %p"] {
        let e = fail(&format!("local s = string.format('{f}', {{}})"));
        assert_eq!(e.line, 1, "{f}: {e}");
        assert!(e.message.contains("%p"), "{f}: {e}");
    }
    // an escaped percent followed by p is text
    ok("assert(string.format('%%p') == '%p')\nassert(string.format('%5.2f', 1.5) == ' 1.50')");
    // a table through %s prints its type, never an address
    let r = ok("print(string.format('%s|%s', {}, print))");
    assert_eq!(r.log, vec!["table|function".to_string()]);
    let r = ok("print(vec3(1, 2, 3), profile.rect(0.1, 0.1))");
    assert!(!r.log[0].contains("0x"), "{:?}", r.log);
}
