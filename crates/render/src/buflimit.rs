// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Size math for GPU buffers that grow with the scene, checked against the device limit.

/// How many rows of a growing buffer fit and how large the allocation is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowPlan {
    /// Rows that will be stored (the rest are dropped).
    pub rows: u64,
    /// Allocation size in bytes (a multiple of 4, never above the limit).
    pub bytes: u64,
    /// Rows asked for but not stored.
    pub dropped: u64,
}

/// Plan an allocation for `want` rows of `row_bytes` each. The size is rounded up to a power
/// of two for amortised growth, but never past `limit`; rows beyond what the limit can hold
/// are dropped. A limit smaller than one row yields an empty plan (no buffer).
pub fn plan_rows(want: u64, row_bytes: u64, limit: u64) -> RowPlan {
    let limit = limit & !3;
    if want == 0 || row_bytes == 0 || row_bytes > limit {
        return RowPlan {
            rows: 0,
            bytes: 0,
            dropped: want,
        };
    }
    let fit = limit / row_bytes;
    let rows = want.min(fit);
    let need = rows * row_bytes;
    let bytes = need
        .checked_next_power_of_two()
        .map_or(limit, |p| p.min(limit))
        .max(need);
    RowPlan {
        rows,
        bytes: (bytes + 3) & !3,
        dropped: want - rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: u64 = 268_435_456;

    #[test]
    fn impostor_buffer_never_exceeds_limit() {
        // 301.6M of rows against a 268.4M limit: the crash family from the QA report
        let want = 301_600_000 / 64;
        let p = plan_rows(want, 64, LIMIT);
        assert!(p.bytes <= LIMIT);
        assert!(p.rows * 64 <= p.bytes);
        assert_eq!(p.rows, LIMIT / 64);
        assert_eq!(p.dropped, want - p.rows);
    }

    #[test]
    fn power_of_two_growth_is_capped_at_limit() {
        let p = plan_rows(3_000_000, 64, 200_000_000);
        assert!(p.bytes <= 200_000_000);
        assert_eq!(p.dropped, 0);
        let p = plan_rows(10, 64, LIMIT);
        assert_eq!((p.rows, p.bytes, p.dropped), (10, 1024, 0));
    }

    #[test]
    fn exactly_at_the_limit() {
        let p = plan_rows(LIMIT / 64, 64, LIMIT);
        assert_eq!((p.rows, p.bytes, p.dropped), (LIMIT / 64, LIMIT, 0));
        let p = plan_rows(LIMIT / 64 + 1, 64, LIMIT);
        assert_eq!((p.rows, p.bytes, p.dropped), (LIMIT / 64, LIMIT, 1));
    }

    #[test]
    fn limit_smaller_than_one_element() {
        let p = plan_rows(5, 64, 63);
        assert_eq!((p.rows, p.bytes, p.dropped), (0, 0, 5));
        let p = plan_rows(5, 64, 0);
        assert_eq!((p.rows, p.bytes, p.dropped), (0, 0, 5));
    }

    #[test]
    fn nothing_wanted_is_empty() {
        assert_eq!(plan_rows(0, 64, LIMIT).bytes, 0);
    }

    #[test]
    fn unaligned_limit_rounds_down() {
        let p = plan_rows(100, 4, 402);
        assert_eq!(p.rows, 100);
        assert!(p.bytes <= 400 && p.bytes % 4 == 0);
    }

    #[test]
    fn instances_over_limit_refused_not_panic() {
        // limit below one 64-byte matrix: empty plan, no panic
        let p = plan_rows(1, 64, 32);
        assert_eq!(p.rows, 0);
        // far more matrices than fit: truncated, never above the limit
        let p = plan_rows(10_000_000, 64, LIMIT);
        assert_eq!(p.rows, LIMIT / 64);
        assert!(p.bytes <= LIMIT);
    }
}
