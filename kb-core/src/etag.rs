//! `etag` — the one canonical FNV-1a 64-bit content hash used for optimistic
//! concurrency (`--if-unchanged`) and the `etag` CLI surface.
//!
//! Owned here (in `kb-core`) so both the CLI (`kb/main.rs`) and the indexer
//! (`kb-db`) compute the *same* 16-hex string for the same bytes. Previously
//! each shipped its own copy, so a fix to one would silently desync the other
//! and the etag-mismatch check would stop matching. Single owner, no drift.

/// FNV-1a 64-bit of `b`, rendered as 16 lowercase hex chars (the `etag`).
///
/// The empty string hashes to `cbf29ce484222325` — the FNV-1a 64 offset
/// basis — which is the unit-test golden (see `kb-db` / below) that catches a
/// regressed prime/offset.
pub fn of(b: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(PRIME);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::of;

    #[test]
    fn empty_is_fnv1a64_offset_basis() {
        // FNV-1a 64 of the empty input is the offset basis itself.
        assert_eq!(of(b""), "cbf29ce484222325");
    }

    #[test]
    fn is_deterministic_and_stable_length() {
        let a = of(b"hello world");
        let b = of(b"hello world");
        assert_eq!(a, b, "same input must give the same etag");
        assert_eq!(a.len(), 16, "etag is 16 hex chars");
        let c = of(b"different");
        assert_ne!(a, c, "different input gives a different etag");
    }
}
