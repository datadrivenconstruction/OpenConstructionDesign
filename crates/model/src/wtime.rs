// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The file system speaks `std::time::SystemTime`, the rest of the code `web_time` (the same type
//! on desktop, its own in the browser build). Every crossing goes through these two functions.

use std::time::{SystemTime as StdTime, UNIX_EPOCH as STD_EPOCH};
use web_time::{SystemTime, UNIX_EPOCH};

/// A time from the file system (`Metadata::modified`) as the code's time.
pub fn from_std(t: StdTime) -> SystemTime {
    match t.duration_since(STD_EPOCH) {
        Ok(d) => UNIX_EPOCH + d,
        Err(e) => UNIX_EPOCH - e.duration(),
    }
}

/// The code's time for the file system (`File::set_modified`).
pub fn to_std(t: SystemTime) -> StdTime {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => STD_EPOCH + d,
        Err(e) => STD_EPOCH - e.duration(),
    }
}

/// `Metadata::modified` as the code's time.
pub fn modified(m: &std::fs::Metadata) -> std::io::Result<SystemTime> {
    m.modified().map(from_std)
}

#[cfg(test)]
mod tests {
    use super::*;
    use web_time::Duration;

    #[test]
    fn times_cross_both_ways_unchanged_also_before_the_epoch() {
        for t in [UNIX_EPOCH + Duration::from_millis(1_700_000_000_123), UNIX_EPOCH - Duration::from_secs(5)] {
            assert_eq!(from_std(to_std(t)), t);
        }
        let now = SystemTime::now();
        assert_eq!(from_std(to_std(now)), now);
    }
}
