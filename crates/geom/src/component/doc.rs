//! Family editor document: the definition text is the source of truth, edits are made per operator
//! (add, replace, delete), each is validated by parsing and goes into its own undo history
//! (separate from the project). Nested `begin … end` blocks always stay at the end of the text.

use super::kernel::{Kernel, SolidOp};
use super::{Axis, Category, Family, FamilyError, FamilyType, Hosting, Kind, Lod};

const UNDO_DEPTH: usize = 200;

/// Text operator: lines `first..=last` (0-based), head, name (second word), text on one line.
#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub first: usize,
    pub last: usize,
    pub head: String,
    pub name: String,
    pub text: String,
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(k) if !line[..k].contains('"') || line[..k].matches('"').count() % 2 == 0 => {
            &line[..k]
        }
        _ => line,
    }
}

/// Operators as the parser sees them: lines with an unclosed bracket continue, a top-level
/// `begin … end` block is a single `begin` operator named after the nested family.
pub fn stmts(text: &str) -> Vec<Stmt> {
    let mut out: Vec<Stmt> = vec![];
    let (mut open, mut depth) = (0i32, 0usize);
    for (n, raw) in text.lines().enumerate() {
        let line = strip_comment(raw);
        let head = line.split_whitespace().next().unwrap_or("");
        if depth > 0 {
            let last = out.last_mut().expect("begin block");
            last.last = n;
            match head {
                "begin" => depth += 1,
                "end" => depth -= 1,
                "family" if depth == 1 && last.name.is_empty() => {
                    last.name = line.trim()[6..].trim().trim_matches('"').to_string()
                }
                _ => {}
            }
            continue;
        }
        if head == "begin" && open == 0 {
            depth = 1;
            out.push(Stmt {
                first: n,
                last: n,
                head: "begin".into(),
                name: String::new(),
                text: String::new(),
            });
            continue;
        }
        if open > 0 {
            if let Some(last) = out.last_mut() {
                last.last = n;
                last.text.push(' ');
                last.text.push_str(line.trim());
            }
        } else if !line.trim().is_empty() {
            let t = line.trim().to_string();
            let mut w = t.split_whitespace();
            let head = w.next().unwrap_or("").to_string();
            let name = match head.as_str() {
                "type" => match t[4..].trim().strip_prefix('"') {
                    Some(r) => r.split('"').next().unwrap_or("").to_string(),
                    None => w.next().unwrap_or("").to_string(),
                },
                _ => w.next().unwrap_or("").to_string(),
            };
            out.push(Stmt {
                first: n,
                last: n,
                head,
                name,
                text: t,
            });
        }
        open = (open + line.matches('(').count() as i32 - line.matches(')').count() as i32).max(0);
    }
    out
}

/// Difference of two texts by operator (head + name; a `begin … end` block as a whole).
#[derive(Clone, Debug, PartialEq)]
pub enum OpChange {
    Added(Stmt),
    Removed(Stmt),
    Changed { old: Stmt, new: Stmt },
}

/// Operator with full text: for a `begin … end` block, all its lines.
fn whole(text: &str, mut s: Stmt) -> Stmt {
    if s.head == "begin" {
        let l: Vec<&str> = text.lines().collect();
        let end = s.last.min(l.len().saturating_sub(1));
        s.text = l[s.first.min(end)..=end].join("
");
    }
    s
}

fn same(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}

/// What changed in `new` relative to `old`: in new-text order, then the deleted ones.
/// Repeats of the same (head, name) pair are matched in order of appearance.
pub fn op_diff(old: &FamDoc, new: &str) -> Vec<OpChange> {
    let mut left: Vec<Stmt> = stmts(old.text()).into_iter().map(|s| whole(old.text(), s)).collect();
    let mut out = vec![];
    for n in stmts(new) {
        let n = whole(new, n);
        match left.iter().position(|o| o.head == n.head && o.name == n.name) {
            Some(k) => {
                let o = left.remove(k);
                if !same(&o.text, &n.text) {
                    out.push(OpChange::Changed { old: o, new: n });
                }
            }
            None => out.push(OpChange::Added(n)),
        }
    }
    out.extend(left.into_iter().map(OpChange::Removed));
    out
}

/// Length literal in mm (no trailing zeros).
pub fn mm(v: f64) -> String {
    let s = format!("{:.6}", v * 1000.0);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    let s = if s == "-0" { "0" } else { s };
    format!("{s}mm")
}

/// Angle literal in degrees.
pub fn deg(v: f64) -> String {
    let s = format!("{:.6}", v.to_degrees());
    let s = s.trim_end_matches('0').trim_end_matches('.');
    let s = if s == "-0" { "0" } else { s };
    format!("{s}deg")
}

fn axis_name(a: Axis) -> &'static str {
    match a {
        Axis::X => "x",
        Axis::Y => "y",
        Axis::Z => "z",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RefAt {
    /// Formula or number (`= …`).
    Fixed(String),
    /// Free, with an initial position.
    Free(f64),
}

#[derive(Clone, Debug, Default)]
pub struct FamDoc {
    text: String,
    undo: Vec<String>,
    redo: Vec<String>,
    /// Has changes since the last load into the project / save.
    pub dirty: bool,
}

impl FamDoc {
    pub fn new(text: impl Into<String>) -> Self {
        let mut t: String = text.into();
        if !t.ends_with('\n') {
            t.push('\n');
        }
        Self {
            text: t,
            ..Default::default()
        }
    }

    /// Empty family: reference planes for center and level, insertion point at their intersection.
    pub fn blank(name: &str, category: Category, host: Hosting) -> Self {
        Self::new(format!(
            "family {name}\ncategory {}\nhost {}\nref CenterLR x = 0\nref CenterFB y = 0\nref Ref_Level z = 0\n",
            category.name(),
            host.name()
        ))
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn family(&self) -> Result<Family, FamilyError> {
        Family::parse(&self.text)
    }

    pub fn stmts(&self) -> Vec<Stmt> {
        stmts(&self.text)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(t) => {
                self.redo.push(std::mem::replace(&mut self.text, t));
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(t) => {
                self.undo.push(std::mem::replace(&mut self.text, t));
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    /// New text if it parses; otherwise the document is unchanged.
    pub fn set_text(&mut self, mut t: String) -> Result<(), FamilyError> {
        if !t.ends_with('\n') {
            t.push('\n');
        }
        if t == self.text {
            return Ok(());
        }
        Family::parse(&t)?;
        self.undo.push(std::mem::replace(&mut self.text, t));
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
        Ok(())
    }

    fn lines(&self) -> Vec<String> {
        self.text.lines().map(str::to_string).collect()
    }

    fn commit(&mut self, lines: Vec<String>) -> Result<(), FamilyError> {
        self.set_text(lines.join("\n") + "\n")
    }

    /// Insertion line: before the first `begin` block, otherwise at the end.
    fn tail(&self) -> usize {
        self.stmts()
            .iter()
            .find(|s| s.head == "begin")
            .map_or(self.text.lines().count(), |s| s.first)
    }

    /// Current operator equal to `want` by head, name and text.
    fn current(&self, want: &Stmt) -> Option<Stmt> {
        self.stmts()
            .into_iter()
            .map(|s| whole(&self.text, s))
            .find(|s| s.head == want.head && s.name == want.name && same(&s.text, &want.text))
    }

    /// Discard one draft edit: the text reverts to the operator's previous form (one
    /// undo history entry, like a manual edit).
    pub fn reject_op(&mut self, c: &OpChange) -> Result<(), FamilyError> {
        let stale = || FamilyError::Parse {
            line: 1,
            msg: "statement changed since the comparison; compare again".into(),
        };
        match c {
            OpChange::Added(n) => {
                let cur = self.current(n).ok_or_else(stale)?;
                self.remove_stmt(&cur)
            }
            OpChange::Changed { old, new } => {
                let cur = self.current(new).ok_or_else(stale)?;
                self.replace_stmt(&cur, &old.text)
            }
            OpChange::Removed(o) if o.head == "begin" => {
                let t = format!("{}{}
", self.text, o.text);
                self.set_text(t)
            }
            OpChange::Removed(o) => self.add_stmt(&o.text),
        }
    }

    pub fn find(&self, head: &str, name: &str) -> Option<Stmt> {
        self.stmts()
            .into_iter()
            .find(|s| s.head == head && s.name == name)
    }

    pub fn add_stmt(&mut self, line: &str) -> Result<(), FamilyError> {
        let mut l = self.lines();
        let at = self.tail().min(l.len());
        l.insert(at, line.trim().to_string());
        self.commit(l)
    }

    pub fn replace_stmt(&mut self, s: &Stmt, line: &str) -> Result<(), FamilyError> {
        let mut l = self.lines();
        if s.last >= l.len() || s.first > s.last {
            return Err(FamilyError::Parse {
                line: s.first + 1,
                msg: "statement moved; reopen the family".into(),
            });
        }
        let _ = l.splice(s.first..=s.last, line.lines().map(str::to_string));
        self.commit(l)
    }

    pub fn remove_stmt(&mut self, s: &Stmt) -> Result<(), FamilyError> {
        self.replace_stmt(s, "")
    }

    fn upsert(&mut self, head: &str, name: &str, line: &str) -> Result<(), FamilyError> {
        match self.find(head, name) {
            Some(s) => self.replace_stmt(&s, line),
            None => self.add_stmt(line),
        }
    }

    pub fn add_ref(&mut self, name: &str, axis: Axis, at: RefAt) -> Result<(), FamilyError> {
        let tail = match at {
            RefAt::Fixed(e) => format!("= {}", e.trim()),
            RefAt::Free(v) => format!("~ {}", mm(v)),
        };
        let line = format!("ref {name} {} {tail}", axis_name(axis));
        if self.find("ref", name).is_some() {
            return Err(FamilyError::Param {
                name: name.into(),
                msg: "name used twice".into(),
            });
        }
        self.add_stmt(&line)
    }

    /// Dragging a plane: for a free plane the initial position changes (the sketch re-solves),
    /// for one pinned by a number the number itself changes; a formula-driven plane does not move.
    pub fn move_ref(&mut self, name: &str, v: f64) -> Result<(), FamilyError> {
        let s = self.find("ref", name).ok_or_else(|| FamilyError::Param {
            name: name.into(),
            msg: "no such reference plane".into(),
        })?;
        let w: Vec<&str> = s.text.splitn(4, char::is_whitespace).collect();
        let ax = w.get(2).copied().unwrap_or("x");
        let rest = w.get(3).copied().unwrap_or("").trim();
        if rest.starts_with('~') {
            return self.replace_stmt(&s, &format!("ref {name} {ax} ~ {}", mm(v)));
        }
        let e = rest.strip_prefix('=').unwrap_or(rest).trim();
        let constant = super::expr::parse(e)
            .ok()
            .is_some_and(|x| x.eval(&|_| None).is_ok());
        if !constant {
            return Err(FamilyError::Param {
                name: name.into(),
                msg: "driven by a formula".into(),
            });
        }
        self.replace_stmt(&s, &format!("ref {name} {ax} = {}", mm(v)))
    }

    /// Plane: pinned ↔ free (the position is preserved).
    pub fn set_ref_free(&mut self, name: &str, free: bool, at: f64) -> Result<(), FamilyError> {
        let s = self.find("ref", name).ok_or_else(|| FamilyError::Param {
            name: name.into(),
            msg: "no such reference plane".into(),
        })?;
        let ax = s.text.split_whitespace().nth(2).unwrap_or("x").to_string();
        let tail = if free {
            format!("~ {}", mm(at))
        } else {
            format!("= {}", mm(at))
        };
        self.replace_stmt(&s, &format!("ref {name} {ax} {tail}"))
    }

    /// `tail` is a value (`600mm`, `"Oak"`) or a formula (`= Width / 2`).
    pub fn add_param(
        &mut self,
        name: &str,
        kind: Kind,
        instance: bool,
        tail: &str,
    ) -> Result<(), FamilyError> {
        if self.find("param", name).is_some() {
            return Err(FamilyError::Param {
                name: name.into(),
                msg: "duplicate parameter".into(),
            });
        }
        let scope = if instance { "instance" } else { "type" };
        self.add_stmt(&format!(
            "param {name} {} {scope} {}",
            kind.name(),
            tail.trim()
        ))
    }

    pub fn set_param(
        &mut self,
        name: &str,
        kind: Kind,
        instance: bool,
        tail: &str,
    ) -> Result<(), FamilyError> {
        let s = self.find("param", name).ok_or_else(|| FamilyError::Param {
            name: name.into(),
            msg: "no such parameter".into(),
        })?;
        let scope = if instance { "instance" } else { "type" };
        let line = format!("param {name} {} {scope} {}", kind.name(), tail.trim());
        self.replace_stmt(&s, line.trim_end())
    }

    pub fn add_dim(&mut self, value: &str, a: &str, b: &str) -> Result<(), FamilyError> {
        self.add_stmt(&format!("dim {} {a} {b}", value.trim()))
    }

    pub fn add_angle(&mut self, value: &str, a: &str, b: &str) -> Result<(), FamilyError> {
        self.add_stmt(&format!("angle {} {a} {b}", value.trim()))
    }

    pub fn add_eq(&mut self, planes: &[&str]) -> Result<(), FamilyError> {
        self.add_stmt(&format!("eq {}", planes.join(" ")))
    }

    pub fn add_align(&mut self, a: &str, b: &str) -> Result<(), FamilyError> {
        self.add_stmt(&format!("align {a} {b}"))
    }

    /// Visibility condition of a solid or nested family; None means always visible.
    pub fn set_visible(&mut self, target: &str, cond: Option<&str>) -> Result<(), FamilyError> {
        match (self.find("visible", target), cond) {
            (Some(s), None) => self.remove_stmt(&s),
            (None, None) => Ok(()),
            (_, Some(c)) => {
                self.upsert("visible", target, &format!("visible {target} {}", c.trim()))
            }
        }
    }

    pub fn set_origin(&mut self, x: &str, y: &str, z: &str) -> Result<(), FamilyError> {
        let line = format!("origin {}, {}, {}", x.trim(), y.trim(), z.trim());
        match self.stmts().into_iter().find(|s| s.head == "origin") {
            Some(s) => self.replace_stmt(&s, &line),
            None => self.add_stmt(&line),
        }
    }

    fn set_head(&mut self, head: &str, value: &str) -> Result<(), FamilyError> {
        let line = format!("{head} {value}");
        match self.stmts().into_iter().find(|s| s.head == head) {
            Some(s) => self.replace_stmt(&s, &line),
            None => {
                // right after the family line
                let mut l = self.lines();
                let at = self
                    .stmts()
                    .iter()
                    .find(|s| s.head == "family")
                    .map_or(0, |s| s.last + 1);
                l.insert(at.min(l.len()), line);
                self.commit(l)
            }
        }
    }

    pub fn set_host(&mut self, h: Hosting) -> Result<(), FamilyError> {
        self.set_head("host", h.name())
    }

    pub fn set_category(&mut self, c: Category) -> Result<(), FamilyError> {
        self.set_head("category", c.name())
    }

    fn solid_stmt(&self, solid: &str) -> Result<Stmt, FamilyError> {
        self.stmts()
            .into_iter()
            .find(|s| {
                matches!(s.head.as_str(), "extrude" | "revolve" | "sweep" | "blend")
                    && s.name == solid
            })
            .ok_or_else(|| FamilyError::Param {
                name: solid.into(),
                msg: "no such solid".into(),
            })
    }

    /// Argument `arg` of the `clause(...)` clause of solid `solid` (for example rect, 2nd).
    pub fn solid_arg(&self, solid: &str, clause: &str, arg: usize) -> Result<String, FamilyError> {
        let s = self.solid_stmt(solid)?;
        let rest = s.text.split_once(char::is_whitespace).map_or("", |x| x.1);
        let it = super::items(rest).map_err(|msg| FamilyError::Param {
            name: solid.into(),
            msg,
        })?;
        it.iter()
            .find(|(w, a)| w == clause && a.is_some())
            .and_then(|(_, a)| {
                super::split_top(a.as_deref().unwrap_or(""))
                    .get(arg)
                    .cloned()
            })
            .ok_or_else(|| FamilyError::Param {
                name: solid.into(),
                msg: format!("no argument {arg} of {clause}()"),
            })
    }

    /// Alignment lock: the argument becomes a reference to a plane (`value` is the name) or
    /// a number again (lock removed).
    pub fn set_solid_arg(
        &mut self,
        solid: &str,
        clause: &str,
        arg: usize,
        value: &str,
    ) -> Result<(), FamilyError> {
        let s = self.solid_stmt(solid)?;
        let (head, rest) = s
            .text
            .split_once(char::is_whitespace)
            .unwrap_or((s.text.as_str(), ""));
        let perr = |msg: String| FamilyError::Param {
            name: solid.into(),
            msg,
        };
        let it = super::items(rest).map_err(perr)?;
        let mut done = false;
        let mut parts = vec![head.to_string()];
        for (w, a) in it {
            match a {
                Some(a) if w == clause && !done => {
                    let mut v = super::split_top(&a);
                    if arg >= v.len() {
                        return Err(perr(format!("no argument {arg} of {clause}()")));
                    }
                    v[arg] = value.trim().to_string();
                    parts.push(format!("{w}({})", v.join(", ")));
                    done = true;
                }
                Some(a) => parts.push(format!("{w}({a})")),
                None => parts.push(w),
            }
        }
        if !done {
            return Err(perr(format!("no clause {clause}()")));
        }
        self.replace_stmt(&s, &parts.join(" "))
    }

    /// Solid from a profile via the kernel (adapter); the preview builds from the same text.
    pub fn add_solid(
        &mut self,
        name: &str,
        op: &SolidOp,
        k: &dyn Kernel,
    ) -> Result<(), FamilyError> {
        let line = k
            .statement(name, op, Lod::Fine)
            .map_err(|msg| FamilyError::Geometry {
                solid: name.into(),
                msg,
            })?;
        self.add_stmt(&line)
    }

    /// Catalog import, as in the project: same-named `type` rows are replaced, new ones are appended
    /// after the last, other types stay; values come from definition `f`.
    pub fn set_types(&mut self, f: &Family, types: &[FamilyType]) -> Result<(), FamilyError> {
        let st = self.stmts();
        let mut l = self.lines();
        let olds: Vec<&Stmt> = st.iter().filter(|s| s.head == "type").collect();
        let line = |t: &FamilyType| {
            format!(
                "type \"{}\" {}",
                t.name.replace('"', "'"),
                f.assignments_text(&t.values)
            )
            .trim_end()
            .to_string()
        };
        let (mut replace, mut add): (Vec<(usize, usize, String)>, Vec<String>) = (vec![], vec![]);
        for t in types {
            let name = t.name.replace('"', "'");
            match olds.iter().find(|s| s.name == name) {
                Some(s) => replace.push((s.first, s.last, line(t))),
                None => add.push(line(t)),
            }
        }
        // new ones go after the last type: indices of the replaced rows before them do not shift
        let at = olds.last().map_or(self.tail(), |s| s.last + 1).min(l.len());
        let _ = l.splice(at..at, add);
        replace.sort_by_key(|r| r.0);
        for (first, last, text) in replace.into_iter().rev() {
            let end = last.min(l.len().saturating_sub(1));
            let _ = l.splice(first..=end, [text]);
        }
        self.commit(l)
    }

    /// Nested family: a `begin … end` block at the end of the text (the same name is replaced).
    pub fn add_nested(&mut self, def: &str) -> Result<(), FamilyError> {
        let child = Family::parse(def)?;
        let mut l = self.lines();
        if let Some(s) = self
            .stmts()
            .into_iter()
            .find(|s| s.head == "begin" && s.name == child.name)
        {
            let end = s.last.min(l.len().saturating_sub(1));
            l.drain(s.first..=end).for_each(drop);
        }
        while l.last().is_some_and(|x| x.trim().is_empty()) {
            l.pop();
        }
        l.push("begin".into());
        l.extend(def.lines().map(str::to_string));
        l.push("end".into());
        self.commit(l)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Value;

    #[test]
    fn statements_group_continuations_and_nested_blocks() {
        let t = "family A\nparam W length type 1m\nextrude B rect(0, 0,\n  W, W) z(0, W)\nbegin\nfamily Leg\nparam H length type 1m\nend\n";
        let s = stmts(t);
        assert_eq!(s.len(), 4);
        assert_eq!((s[2].first, s[2].last), (2, 3));
        assert_eq!(s[3].head, "begin");
        assert_eq!(s[3].name, "Leg");
        assert_eq!((s[3].first, s[3].last), (4, 7));
    }

    #[test]
    fn doc_edits_validate_and_undo() {
        let mut d = FamDoc::blank("Desk", Category::Furniture, Hosting::None);
        d.add_param("Width", Kind::Length, false, "1200mm").unwrap();
        d.add_ref("Left", Axis::X, RefAt::Free(-0.5)).unwrap();
        d.add_ref("Right", Axis::X, RefAt::Free(0.5)).unwrap();
        d.add_eq(&["Left", "CenterLR", "Right"]).unwrap();
        d.add_dim("Width", "Left", "Right").unwrap();
        d.add_stmt("extrude Top rect(Left, -300mm, Right, 300mm) z(700mm, 740mm)")
            .unwrap();
        let f = d.family().unwrap();
        let r = f.resolve(&[], &[]).unwrap();
        assert!((r.num("Right").unwrap() - 0.6).abs() < 1e-9);
        // an invalid edit is rejected, the document is unchanged
        let before = d.text().to_string();
        assert!(d.add_dim("Width", "Left", "Nowhere").is_err());
        assert_eq!(d.text(), before);
        // lock: remove (number), undo (plane again), redo
        d.set_solid_arg("Top", "rect", 2, "500mm").unwrap();
        assert_eq!(d.solid_arg("Top", "rect", 2).unwrap(), "500mm");
        assert!(d.undo());
        assert_eq!(d.solid_arg("Top", "rect", 2).unwrap(), "Right");
        assert!(d.redo());
        d.set_solid_arg("Top", "rect", 2, "Right").unwrap();
        assert!(d
            .family()
            .unwrap()
            .locks()
            .contains(&("Top".to_string(), "Right".to_string())));
        // dragging a formula-pinned plane is disallowed; for a free plane it changes the initial position
        assert!(d.move_ref("Left", -0.7).is_ok());
        d.add_ref("Mid", Axis::X, RefAt::Fixed("Width / 4".into()))
            .unwrap();
        assert!(d.move_ref("Mid", 0.1).is_err());
        d.set_visible("Top", Some("Width > 500mm")).unwrap();
        assert!(d.find("visible", "Top").is_some());
        d.set_visible("Top", None).unwrap();
        assert!(d.find("visible", "Top").is_none());
        d.set_host(Hosting::Floor).unwrap();
        assert_eq!(d.family().unwrap().hosting, Hosting::Floor);
    }

    #[test]
    fn set_types_merges_like_project_import() {
        let mut d = FamDoc::blank("Desk", Category::Furniture, Hosting::None);
        d.add_param("Width", Kind::Length, false, "1200mm").unwrap();
        d.add_stmt("type \"A\" Width=1000mm").unwrap();
        d.add_stmt("type \"B\" Width=1100mm").unwrap();
        let f = d.family().unwrap();
        let t = |n: &str, w: f64| FamilyType {
            name: n.into(),
            values: vec![("Width".into(), Value::Num(w))],
        };
        d.set_types(&f, &[t("B", 1.5), t("C", 0.9)]).unwrap();
        let types = d.family().unwrap().types;
        let names: Vec<&str> = types.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["A", "B", "C"]);
        let w = |k: usize| match types[k].values[0].1 {
            Value::Num(x) => x,
            _ => f64::NAN,
        };
        assert!(
            (w(0) - 1.0).abs() < 1e-9 && (w(1) - 1.5).abs() < 1e-9 && (w(2) - 0.9).abs() < 1e-9
        );
    }
}
