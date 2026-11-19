//! A linked session of the editor: open a `.ddclink`, hold the project lock, publish the open
//! model as a revision (contract `ddc-erp-link/1`). No window code here; `panel` drives it
//! from background jobs, the tests drive it directly.
//!
//! Files live in a work folder per project, `<data dir>/erp/<host_port>/<project id>/`:
//! `link.json` (where the ERP is; no token, no key), models opened or generated from the ERP,
//! and `revs/` with the packages this editor published or the parent data it fetched. The
//! token is only in the OS key store; the context's AI block is only in memory.

use std::path::{Path, PathBuf};

use ddc_link::{Action, Context, ErpUrl, Keeper, LinkError, Linked, LockState, RevisionView, TokenStore};
use serde_json::{json, Value};

use super::publish::{self, PublishArgs};
use super::{gen, package};

/// Why a step failed; `Link` keeps the variant so the window can say what to do.
#[derive(Debug)]
pub enum Fail {
    Link(LinkError),
    Generate(String),
    Publish(String),
    Io(String),
    /// The revision (its number) is a converter model wrapped by the ERP: nothing to open or diff against.
    Imported(u32),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Link(e) => e.fmt(f),
            Fail::Generate(m) | Fail::Publish(m) | Fail::Io(m) => f.write_str(m),
            Fail::Imported(n) => write!(f, "revision {n} is an imported model: the editor cannot build on it"),
        }
    }
}

impl From<LinkError> for Fail {
    fn from(e: LinkError) -> Self {
        Fail::Link(e)
    }
}

fn io(p: &Path, e: std::io::Error) -> Fail {
    Fail::Io(format!("{}: {e}", p.display()))
}

/// The work folder of one ERP project.
pub fn work_dir(base: &Path, erp: &ErpUrl, project_id: &str) -> PathBuf {
    let host: String = erp.authority.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    base.join("erp").join(host).join(project_id)
}

/// What the lock looks like to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockView {
    Mine,
    HeldBy(String),
    Lost,
    /// Read-only role: no lock is taken.
    ReadOnly,
    Stopped(Vec<u32>),
    Unauthorized,
    Released,
}

pub struct Session {
    pub linked: Linked,
    pub ctx: Context,
    pub work: PathBuf,
    keeper: Option<Keeper>,
    held_by: Option<String>,
    /// The brief the open model was generated from, until its first revision is published.
    pub generated_from: Option<PathBuf>,
    /// The revision opened or last published by this editor.
    pub revision: Option<RevisionView>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("project", &self.ctx.project.name).field("work", &self.work).finish()
    }
}

/// An opened link: the session and the model to open in the window (none for `new`).
pub struct Opened {
    pub session: Session,
    pub model: Option<PathBuf>,
    pub action: Action,
}

impl Session {
    fn start(linked: Linked, base: &Path) -> Result<Session, Fail> {
        let ctx = linked.client.context(&linked.project_id)?;
        let work = work_dir(base, &linked.erp, &linked.project_id);
        std::fs::create_dir_all(work.join("revs")).map_err(|e| io(&work, e))?;
        let meta = json!({"schema": "ddc-link-work/1", "erp_url": linked.erp.base, "project_id": linked.project_id,
                          "project_name": ctx.project.name});
        let p = work.join("link.json");
        std::fs::write(&p, serde_json::to_vec_pretty(&meta).unwrap_or_default()).map_err(|e| io(&p, e))?;
        let mut s = Session { linked, ctx, work, keeper: None, held_by: None, generated_from: None, revision: None };
        s.take_lock()?;
        Ok(s)
    }

    /// Takes the project lock and starts the heartbeat. Someone else holding it is not an
    /// error: the session stays, Publish waits.
    pub fn take_lock(&mut self) -> Result<(), Fail> {
        if !self.ctx.permissions.can_publish {
            return Ok(());
        }
        self.keeper = None;
        match self.linked.client.take_lock(&self.linked.project_id) {
            Ok(l) => {
                self.held_by = Some(l.held_by);
                self.keeper = Some(Keeper::start(self.linked.client.clone(), &self.linked.project_id, l.expires_at, ddc_link::HEARTBEAT));
                Ok(())
            }
            Err(LinkError::LockHeld { held_by, .. }) => {
                self.held_by = Some(held_by);
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn lock(&self) -> LockView {
        if let Some(s) = self.linked.client.stopped_with() {
            return LockView::Stopped(s);
        }
        if !self.ctx.permissions.can_publish {
            return LockView::ReadOnly;
        }
        match &self.keeper {
            Some(k) => match k.state().0 {
                LockState::Mine { .. } => LockView::Mine,
                LockState::Lost => LockView::Lost,
                LockState::Stopped { supported } => LockView::Stopped(supported),
                LockState::Unauthorized => LockView::Unauthorized,
                LockState::Released => LockView::Released,
            },
            None => match &self.held_by {
                Some(h) => LockView::HeldBy(h.clone()),
                None => LockView::Released,
            },
        }
    }

    pub fn can_publish(&self) -> bool {
        self.lock() == LockView::Mine
    }

    /// Gives the lock back (closing the link). The token stays in the key store.
    pub fn close(&mut self) -> Result<(), Fail> {
        match self.keeper.take() {
            Some(k) => {
                // our own name stays in `held_by` otherwise and the panel shows us as the other holder
                self.held_by = None;
                k.release().map_err(Fail::from)
            }
            None => Ok(()),
        }
    }

    /// The ERP said the lock is gone (upload 409).
    pub fn lock_lost(&mut self) {
        if let Some(k) = &self.keeper {
            k.mark_lost();
        }
    }

    /// Everything a publish job needs, taken out so it can run on another thread.
    pub fn publish_job(&self, file: &Path, message: Option<String>) -> Result<PublishJob, Fail> {
        if !self.can_publish() {
            return Err(Fail::Link(LinkError::LockLost));
        }
        Ok(PublishJob {
            client: self.linked.client.clone(),
            erp: self.linked.erp.clone(),
            project_id: self.linked.project_id.clone(),
            project_name: self.ctx.project.name.clone(),
            author: self.held_by.clone(),
            depth: self.ctx.brief.as_ref().and_then(|b| b["depth_level"].as_str().map(String::from)),
            brief: self.generated_from.clone(),
            file: file.to_path_buf(),
            work: self.work.clone(),
            message,
        })
    }

    /// A publish job came back.
    pub fn published(&mut self, r: &Result<RevisionView, Fail>) {
        match r {
            Ok(v) => {
                self.generated_from = None;
                self.revision = Some(v.clone());
            }
            Err(Fail::Link(LinkError::LockLost)) => self.lock_lost(),
            Err(_) => {}
        }
    }

    /// Re-reads the context (revision, lock holder, permissions).
    pub fn refresh(&mut self) -> Result<(), Fail> {
        self.ctx = self.linked.client.context(&self.linked.project_id)?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// Opens a `.ddclink`: token, context, lock, and the model the link asks for.
pub fn open_link(path: &Path, store: &dyn TokenStore, base: &Path) -> Result<Opened, Fail> {
    let info = ddc_link::ClientInfo::editor(env!("CARGO_PKG_VERSION"));
    let (linked, link) = ddc_link::connect(path, store, &info)?;
    let mut session = Session::start(linked, base)?;
    let model = match link.action {
        Action::New => None,
        Action::Open => {
            let id = link.revision_id.as_deref().unwrap_or_default();
            Some(fetch_model(&mut session, id)?)
        }
        Action::Generate => Some(generate(&mut session)?),
    };
    Ok(Opened { session, model, action: link.action })
}

/// A project linked before (its `link.json`), with the token from the key store.
pub fn resume(work: &Path, store: &dyn TokenStore, base: &Path) -> Result<Session, Fail> {
    let p = work.join("link.json");
    let meta: Value = serde_json::from_slice(&std::fs::read(&p).map_err(|e| io(&p, e))?)
        .map_err(|e| Fail::Io(format!("{}: {e}", p.display())))?;
    let erp = ErpUrl::parse(meta["erp_url"].as_str().unwrap_or_default())?;
    let pid = meta["project_id"].as_str().unwrap_or_default();
    let linked = ddc_link::resume(&erp, pid, store)?.ok_or(Fail::Link(LinkError::NoToken))?;
    Session::start(linked, base)
}

/// The work folder holding `file`, when `file` came from an ERP project.
pub fn work_of(file: &Path) -> Option<PathBuf> {
    let d = file.parent()?;
    d.join("link.json").is_file().then(|| d.to_path_buf())
}

/// Downloads `model.ddc` of a revision into the work folder.
fn fetch_model(s: &mut Session, revision_id: &str) -> Result<PathBuf, Fail> {
    let rev = s.linked.client.revision(revision_id)?;
    if rev.imported() {
        return Err(Fail::Imported(rev.number));
    }
    let bytes = s.linked.client.download(revision_id, "model.ddc")?;
    let listed = rev.manifest["files"].as_array().and_then(|f| f.iter().find(|x| x["path"] == "model.ddc")).and_then(|x| x["sha256"].as_str());
    if listed != Some(package::sha256(&bytes).as_str()) {
        return Err(Fail::Link(LinkError::Protocol("model.ddc does not match its manifest hash".into())));
    }
    let out = s.work.join(format!("R{}.ddc", rev.number));
    std::fs::write(&out, &bytes).map_err(|e| io(&out, e))?;
    s.revision = Some(rev);
    Ok(out)
}

/// Generates the model from the project brief of the context.
fn generate(s: &mut Session) -> Result<PathBuf, Fail> {
    let brief = s.ctx.brief.clone().ok_or_else(|| Fail::Generate("the ERP project has no brief yet".into()))?;
    let v = brief["version"].as_i64().unwrap_or(0);
    let bp = s.work.join(format!("brief-v{v}.json"));
    std::fs::write(&bp, serde_json::to_vec_pretty(&brief).unwrap_or_default()).map_err(|e| io(&bp, e))?;
    let out = s.work.join(format!("generated-v{v}.ddc"));
    if out.exists() {
        std::fs::remove_file(&out).map_err(|e| io(&out, e))?;
    }
    gen::generate(&brief, &out).map_err(|e| Fail::Generate(e.to_string()))?;
    s.generated_from = Some(bp);
    Ok(out)
}

/// One publish, ready to run off the window thread.
pub struct PublishJob {
    client: ddc_link::Client,
    erp: ErpUrl,
    project_id: String,
    project_name: String,
    author: Option<String>,
    depth: Option<String>,
    brief: Option<PathBuf>,
    file: PathBuf,
    work: PathBuf,
    message: Option<String>,
}

impl PublishJob {
    /// Builds the package against the chain tip, uploads it, keeps it as the next parent.
    pub fn run(self, progress: &mut dyn FnMut(&str)) -> Result<RevisionView, Fail> {
        progress("tip");
        let tip = self.client.chain_tip(&self.project_id)?;
        let parent = match &tip {
            Some(t) => Some(self.parent_package(t)?),
            None => None,
        };
        let out_dir = self.work.join("out");
        std::fs::create_dir_all(&out_dir).map_err(|e| io(&out_dir, e))?;
        let rid = publish::new_uuid().map_err(Fail::Publish)?;
        let out = out_dir.join(format!("{rid}.ddcrev"));
        let args = PublishArgs {
            file: self.file.clone(),
            out: out.clone(),
            parent,
            force: true,
            depth: self.depth.clone(),
            name: Some(self.project_name.clone()),
            author: self.author.clone(),
            message: self.message.clone(),
            project_id: Some(self.project_id.clone()),
            erp_url: Some(self.erp.base.clone()),
            brief: self.brief.clone(),
            revision_id: Some(rid.clone()),
            ..PublishArgs::default()
        };
        publish::publish(&args, progress).map_err(|e| Fail::Publish(format!("{}: {}", e.kind, e.msg)))?;
        progress("upload");
        let up = self.client.upload(&self.project_id, &out);
        let v = match up {
            Ok(v) => v,
            Err(e) => {
                // an ERP this editor cannot talk to: the package stays for a manual hand-over
                if !e.is_stop() {
                    let _ = std::fs::remove_file(&out);
                }
                return Err(e.into());
            }
        };
        let keep = self.work.join("revs").join(format!("{}.ddcrev", v.revision_id));
        if std::fs::rename(&out, &keep).is_err() {
            let _ = std::fs::remove_file(&out);
        }
        Ok(v)
    }

    /// The parent for the diff: this editor's own package of that revision, else the
    /// revision's element table fetched from the ERP in a package of its own (manifest
    /// listing only that file, so the package check still holds).
    fn parent_package(&self, tip: &RevisionView) -> Result<PathBuf, Fail> {
        if tip.imported() {
            return Err(Fail::Imported(tip.number));
        }
        let revs = self.work.join("revs");
        let own = revs.join(format!("{}.ddcrev", tip.revision_id));
        if package::verify(&own).is_ok_and(|m| m["revision_id"] == tip.revision_id.as_str()) {
            return Ok(own);
        }
        let stub = revs.join(format!("{}.parent.ddcrev", tip.revision_id));
        if package::verify(&stub).is_ok_and(|m| m["revision_id"] == tip.revision_id.as_str()) {
            return Ok(stub);
        }
        let entry = tip.manifest["files"]
            .as_array()
            .and_then(|f| f.iter().find(|x| x["role"] == "elements"))
            .cloned()
            .ok_or_else(|| Fail::Link(LinkError::Protocol("the parent revision lists no element table".into())))?;
        let path = entry["path"].as_str().unwrap_or("elements.parquet").to_string();
        let bytes = self.client.download(&tip.revision_id, &path)?;
        if entry["sha256"].as_str() != Some(package::sha256(&bytes).as_str()) {
            return Err(Fail::Link(LinkError::Protocol(format!("{path} of the parent does not match its manifest hash"))));
        }
        let mut m = tip.manifest.clone();
        m["files"] = json!([entry]);
        let entries = vec![("manifest.json".to_string(), serde_json::to_vec(&m).unwrap_or_default()), (path, bytes)];
        let zip = package::zip_bytes(&entries, &|_| false).map_err(Fail::Io)?;
        std::fs::write(&stub, zip).map_err(|e| io(&stub, e))?;
        Ok(stub)
    }
}
