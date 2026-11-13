//! A fingerprint of a file's contents, to tell whether two binaries are the same build.

use std::path::Path;

/// FNV-1a over the file's bytes (0 if it can't be read). Unlike a modification time, this
/// doesn't change when an unchanged binary is copied or rebuilt.
pub fn file(path: &Path) -> u64 {
    let Ok(bytes) = std::fs::read(path) else { return 0 };
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| (hash ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

#[cfg(test)]
mod tests {
    #[test]
    fn same_bytes_same_fingerprint() {
        let dir = std::env::temp_dir();
        let (a, b) = (dir.join("ph-fp-a"), dir.join("ph-fp-b"));
        std::fs::write(&a, b"philharmonic").unwrap();
        std::fs::write(&b, b"philharmonic").unwrap();
        assert_eq!(super::file(&a), super::file(&b));
        std::fs::write(&b, b"philharmonie").unwrap();
        assert_ne!(super::file(&a), super::file(&b));
        assert_eq!(super::file(&dir.join("ph-fp-missing")), 0);
    }
}
