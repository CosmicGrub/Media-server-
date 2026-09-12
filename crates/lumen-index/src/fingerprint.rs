//! Reading a file's cheap identity fingerprint from OS metadata -- one `stat`, no content read.

use std::io;
use std::path::Path;

use lumen_identity::FsFingerprint;

/// The `(size, mtime)` pair that lets [`lumen_identity::classify`] answer "unchanged" without
/// opening the file. `device_id`/`inode` are populated on Unix, where they come free from the same
/// syscall as `size`; on Windows they are left at `0`.
///
/// That is deliberate, not an oversight: `classify`'s own `Unchanged`/`Modified` decision only ever
/// compares `size` and `mtime_ns` -- see its doc comment and body in `lumen-identity` -- so an
/// unpopulated pair on Windows never weakens the diff this crate exists to do. A real, stable file
/// index on Windows needs an open handle and `GetFileInformationByHandle`, which needs either
/// `unsafe` (denied workspace-wide, `Cargo.toml`'s `[workspace.lints.rust]`) or a new dependency, to
/// populate two fields nothing currently reads. Worth doing the day something does; not before.
///
/// Never follows a symlink: a fingerprint should describe the link's own target identity at the path
/// the library asked about, not silently describe whatever the link happens to point at today.
pub fn fs_fingerprint(path: &Path) -> io::Result<FsFingerprint> {
    let meta = std::fs::symlink_metadata(path)?;
    let size = meta.len();

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(FsFingerprint {
            device_id: meta.dev(),
            inode: meta.ino(),
            size,
            mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILETIME: 100-nanosecond ticks since 1601-01-01. Never compared against a Unix-epoch
        // value from another platform -- an index file is single-machine, so a platform-native tick
        // count that is merely stable and strictly comparable for this file, on this machine, over
        // time is everything `classify` needs. Naming it `mtime_ns` (not literally nanoseconds here)
        // keeps the field's role identical across platforms even though the unit differs.
        Ok(FsFingerprint {
            device_id: 0,
            inode: 0,
            size,
            mtime_ns: i128::from(meta.last_write_time()),
        })
    }
}

#[cfg(not(any(unix, windows)))]
compile_error!("lumen-index's fs_fingerprint needs a Unix or Windows target");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_reads_of_an_untouched_file_agree() {
        let dir = std::env::temp_dir().join(format!("lumen-index-fp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stable.txt");
        std::fs::write(&path, b"hello").unwrap();

        let a = fs_fingerprint(&path).unwrap();
        let b = fs_fingerprint(&path).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.size, 5);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_bigger_file_reports_the_new_size() {
        let dir = std::env::temp_dir().join(format!("lumen-index-fp2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("grows.txt");
        std::fs::write(&path, b"short").unwrap();
        let before = fs_fingerprint(&path).unwrap();

        let longer = b"a good deal longer than before";
        std::fs::write(&path, longer).unwrap();
        let after = fs_fingerprint(&path).unwrap();

        assert_ne!(before.size, after.size);
        assert_eq!(after.size, longer.len() as u64);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_path_is_an_error_not_a_default() {
        let path = std::env::temp_dir().join("lumen-index-fp-does-not-exist-xyz");
        assert!(fs_fingerprint(&path).is_err());
    }
}
