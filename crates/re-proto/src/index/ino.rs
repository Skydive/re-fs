//! Inode numbers derived from UUIDs, so a document keeps its inode across remounts.

use uuid::Uuid;

pub const ROOT: u64 = 1;
pub const TRASH: u64 = 2;
pub const LOST_FOUND: u64 = 3;
/// Inodes below this are reserved for the fixed directories above.
pub const FIRST_DYNAMIC: u64 = 16;

/// The preferred inode for a UUID. `Tree::build` probes upward on the (very unlikely)
/// collision, so callers must use the inode the tree assigned, not this directly.
pub fn preferred(uuid: &Uuid) -> u64 {
    let v = uuid.as_u128();
    let folded = (v as u64) ^ ((v >> 64) as u64);
    // Keep it positive as an i64 (some tools print inodes signed) and out of the reserved range.
    let ino = folded & (i64::MAX as u64);
    if ino < FIRST_DYNAMIC {
        ino + FIRST_DYNAMIC
    } else {
        ino
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_and_outside_reserved_range() {
        let u = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
        assert_eq!(preferred(&u), preferred(&u));
        assert!(preferred(&u) >= FIRST_DYNAMIC);
        assert!(preferred(&Uuid::nil()) >= FIRST_DYNAMIC);
        assert!(preferred(&Uuid::max()) <= i64::MAX as u64);
    }
}
