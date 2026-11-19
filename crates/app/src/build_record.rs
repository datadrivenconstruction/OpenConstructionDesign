//! Build record written by build.rs (commit, build time, build id, keyed tag); kept in the
//! executable image for support and release audits, never shown in the UI.
//!
//! Layout, 94 bytes: magic (16), version 1, flags (bit 0 keyed, bit 1 uncommitted changes),
//! commit (20), build time (u64 LE seconds), build id (16), HMAC-SHA256 over version..build id.

pub const LEN: usize = 94;

#[used]
pub static BUILD_RECORD: [u8; LEN] =
    *include_bytes!(concat!(env!("OUT_DIR"), "/build_record.bin"));

/// The record's fields; `tag` is all zero when the build had no key.
#[derive(Debug, PartialEq, Eq)]
pub struct Record {
    pub version: u8,
    pub keyed: bool,
    pub dirty: bool,
    pub commit: [u8; 20],
    pub time: u64,
    pub build_id: [u8; 16],
    pub tag: [u8; 32],
}

/// Parses a record; `None` when the version is unknown.
pub fn parse(b: &[u8; LEN]) -> Option<Record> {
    if b[16] != 1 {
        return None;
    }
    let mut r = Record {
        version: 1,
        keyed: b[17] & 1 != 0,
        dirty: b[17] & 2 != 0,
        commit: [0; 20],
        time: u64::from_le_bytes(b[38..46].try_into().ok()?),
        build_id: [0; 16],
        tag: [0; 32],
    };
    r.commit.copy_from_slice(&b[18..38]);
    r.build_id.copy_from_slice(&b[46..62]);
    r.tag.copy_from_slice(&b[62..94]);
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_build_carries_a_version_one_record() {
        let r = parse(&BUILD_RECORD).expect("version 1");
        assert!(r.time > 0, "build time");
        assert_ne!(r.build_id, [0; 16]);
        // an unkeyed build leaves the tag empty; a keyed one fills it
        assert_eq!(r.keyed, r.tag != [0; 32], "{r:?}");
        if let Some(c) = option_env!("DDC_BUILD_COMMIT") {
            let hex: String = r.commit.iter().map(|b| format!("{b:02x}")).collect();
            assert!(hex.starts_with(c) || r.commit == [0; 20], "{hex} vs {c}");
        }
    }

    #[test]
    fn unknown_version_is_refused() {
        let mut b = BUILD_RECORD;
        b[16] = 2;
        assert!(parse(&b).is_none());
    }
}
