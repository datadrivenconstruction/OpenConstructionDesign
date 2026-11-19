use ddc_ops::op::LevelOp;
use ddc_ops::{Actor, Envelope, Group, Len, Op};
use rusqlite::Connection;

use super::*;
use crate::schema::VERSION;
use crate::AuditEntry;

fn levels(m: &Model) -> i64 {
    m.sql()
        .query_row("SELECT count(*) FROM level", [], |r| r.get(0))
        .unwrap()
}

fn small_env(actor: Actor) -> Envelope {
    let mut e = Envelope::new(actor, "Add level L1");
    e.why = Some("the brief asks for a first floor".into());
    e.groups.push(Group {
        id: 1,
        title: "Levels".into(),
        ops: vec![Op::Level(LevelOp::Create {
            slot: 1,
            name: "L1".into(),
            elevation: Len::m(3.0),
        })],
        depends_on: vec![],
        why: String::new(),
    });
    e
}

fn src_type(m: &Model, seq: i64) -> String {
    m.sql()
        .query_row("SELECT typeof(src) FROM op WHERE seq=?1", [seq], |r| r.get(0))
        .unwrap()
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ddc-journal-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A file as an older schema wrote it: `op` without the provenance columns, one level and its undo row.
fn make_old_file(p: &str) {
    {
        let mut m = Model::open(p).unwrap();
        m.add_level("L1", 0.0).unwrap();
    }
    // own connection: the model's authorizer guards schema changes
    let c = Connection::open(p).unwrap();
    for col in ["actor", "title", "why", "src"] {
        c.execute_batch(&format!("ALTER TABLE op DROP COLUMN {col}"))
            .unwrap();
    }
    c.execute(
        "UPDATE meta SET value=?1 WHERE key='schema_version'",
        [VERSION - 1],
    )
    .unwrap();
}

#[test]
fn journal_keeps_forward_envelope() {
    let mut m = Model::open(":memory:").unwrap();
    let env = small_env(Actor::Ai {
        session: "s-1".into(),
        model: "any".into(),
    });
    let (_, seq) = m
        .atomic_with("wall_ai_plan", &Provenance::of(&env), |m| {
            m.add_level("L1", 3.0)
        })
        .unwrap();
    assert_eq!(seq, m.op_top().unwrap());
    assert_eq!(src_type(&m, seq), "text", "small envelope stays readable");
    assert_eq!(m.journal_src(seq).unwrap(), Some(env.clone()));
    let j = &m.journal(1).unwrap()[0];
    assert_eq!(j.actor, Some(env.actor.clone()));
    assert_eq!(j.title.as_deref(), Some("Add level L1"));
    assert_eq!(j.why, env.why);
    assert!(j.has_src);

    // above 4 kB the envelope is compressed and still round-trips
    let big = ddc_ops::sample::envelope();
    assert!(big.to_json().unwrap().len() > SRC_INLINE);
    let (_, seq2) = m
        .atomic_with("wall_ai_plan", &Provenance::of(&big), |m| {
            m.add_level("L2", 6.0)
        })
        .unwrap();
    assert_eq!(src_type(&m, seq2), "blob");
    assert_eq!(m.journal_src(seq2).unwrap(), Some(big));

    // undo still runs the inverse SQL and drops the entry with its envelope
    m.undo().unwrap();
    m.undo().unwrap();
    assert_eq!(levels(&m), 0);
    assert_eq!(m.journal_src(seq).unwrap(), None);
}

#[test]
fn actor_recorded_per_entry() {
    let mut m = Model::open(":memory:").unwrap();
    m.add_level("L1", 0.0).unwrap();
    let script = Actor::Script {
        name: "renumber.ops.json".into(),
    };
    m.atomic_with("level_script", &Provenance::by(script.clone(), "Renumber"), |m| {
        m.add_level("L2", 3.0)?;
        m.add_level("L3", 6.0)
    })
    .unwrap();
    m.atomic_with("level_xlsx", &Provenance::by(Actor::Xlsx, "XLSX import"), |m| {
        m.add_level("L4", 9.0)
    })
    .unwrap();
    // a plain user atomic over several ops merges into one user entry
    m.atomic("props_multi", |m| {
        m.add_level("L5", 12.0)?;
        m.add_level("L6", 15.0)
    })
    .unwrap();
    let j = m.journal(4).unwrap();
    assert_eq!(j[0].actor, Some(Actor::User));
    assert_eq!(j[1].actor, Some(Actor::Xlsx));
    assert_eq!(j[1].title.as_deref(), Some("XLSX import"));
    assert_eq!(j[2].actor, Some(script));
    assert_eq!(j[2].title.as_deref(), Some("Renumber"));
    assert_eq!(j[3].actor, Some(Actor::User));
    assert!(j.iter().all(|e| !e.has_src), "no envelope was given");
}

#[test]
fn user_edit_row_has_user_actor_and_no_envelope() {
    let mut m = Model::open(":memory:").unwrap();
    m.add_level("L1", 0.0).unwrap();
    let j = &m.journal(1).unwrap()[0];
    assert_eq!(j.actor, Some(Actor::User));
    assert!(!j.has_src);
    assert_eq!((j.title.as_deref(), j.why.as_deref()), (None, None));
    assert_eq!(m.journal_src(j.seq).unwrap(), None);
    let raw: String = m
        .sql()
        .query_row("SELECT actor FROM op WHERE seq=?1", [j.seq], |r| r.get(0))
        .unwrap();
    assert_eq!(raw, actor_json(&Actor::User).unwrap(), "USER matches the wire form");
}

#[test]
fn old_file_migrates_and_opens() {
    let d = temp_dir("migrate");
    let p = d.join("m.ddc");
    let ps = p.to_str().unwrap();
    make_old_file(ps);
    {
        let mut m = Model::open(ps).unwrap();
        assert_eq!(m.schema_version().unwrap(), VERSION);
        let old = &m.journal(1).unwrap()[0];
        assert_eq!(old.kind, "level_add");
        assert_eq!(old.actor, None, "pre-40 rows stay unknown, not claimed as user");
        assert!(!old.has_src);
        // new rows on the migrated file carry provenance
        let env = small_env(Actor::Agent {
            name: "mcp:planner".into(),
        });
        let (_, seq) = m
            .atomic_with("level_agent", &Provenance::of(&env), |m| {
                m.add_level("L2", 3.0)
            })
            .unwrap();
        assert_eq!(m.journal_src(seq).unwrap(), Some(env));
    }
    // reopen is idempotent: columns are added once
    let m = Model::open(ps).unwrap();
    let n: i64 = m
        .sql()
        .query_row(
            "SELECT count(*) FROM pragma_table_info('op') WHERE name IN ('actor','title','why','src')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 4);
    assert_eq!(m.journal_len().unwrap(), 2);
    drop(m);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn old_file_undo_still_works() {
    let d = temp_dir("undo");
    let p = d.join("m.ddc");
    let ps = p.to_str().unwrap();
    make_old_file(ps);
    let mut m = Model::open(ps).unwrap();
    assert_eq!(levels(&m), 1);
    m.undo().unwrap();
    assert_eq!(levels(&m), 0, "the pre-40 inverse SQL still undoes");
    assert_eq!(m.journal_len().unwrap(), 0);
    drop(m);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn audit_links_journal_seq() {
    let mut m = Model::open(":memory:").unwrap();
    let env = small_env(Actor::Ai {
        session: "s-1".into(),
        model: "any".into(),
    });
    let (_, seq) = m
        .atomic_with("wall_ai_plan", &Provenance::of(&env), |m| {
            m.add_level("L1", 3.0)
        })
        .unwrap();
    let a = m
        .audit_append(&AuditEntry {
            session: "s-1".into(),
            actor: "chat".into(),
            event: "apply".into(),
            endpoint: None,
            model: Some("any".into()),
            text: None,
            plan: None,
            op_seq: Some(seq),
            tok_in: 0,
            tok_out: 0,
            cost: None,
            status: "ok".into(),
        })
        .unwrap();
    assert_eq!(m.journal(1).unwrap()[0].audit, Some(a));
    // undo, then a user edit takes the same number: it must not inherit the AI audit row
    m.undo().unwrap();
    m.add_level("L9", 9.0).unwrap();
    let j = &m.journal(1).unwrap()[0];
    assert_eq!(j.seq, seq, "the number is reused");
    assert_eq!(j.audit, None);
    assert_eq!(j.actor, Some(Actor::User));
}
