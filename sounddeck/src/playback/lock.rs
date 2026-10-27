//! Advisory locks that keep configured sound files from being moved or deleted.
//!
//! SoundDeck never writes to audio files. To stop anything else from renaming or
//! deleting them while the app is running, each configured file is opened for
//! reading with a share mode that allows reads and writes but **not** delete.
//! Windows requires DELETE access for both rename and delete, so both fail while
//! the handle is open. Decoders can still open the file because reads are
//! allowed.
//!
//! The lock expires when the sound is removed from the board (the entry, and
//! therefore the handle, is dropped) or when the app exits.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

pub struct FileLock {
    file: File,
}

impl FileLock {
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_SHARE_READ | FILE_SHARE_WRITE. Deliberately no
            // FILE_SHARE_DELETE: that is the bit rename and delete need.
            options.share_mode(0x1 | 0x2);
        }
        Ok(Self {
            file: options.open(path)?,
        })
    }

    /// False once the handle is stale, for example after the volume disappeared.
    pub fn is_valid(&self) -> bool {
        self.file.metadata().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_file(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "sounddeck-lock-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sound.wav");
        fs::write(&path, b"not really audio").unwrap();
        (dir, path)
    }

    #[test]
    fn the_lock_blocks_delete_and_rename_but_allows_reads() {
        let (dir, path) = temp_file("basic");
        let lock = FileLock::acquire(&path).unwrap();
        assert!(lock.is_valid());

        assert!(
            fs::remove_file(&path).is_err(),
            "delete should be denied while locked"
        );
        assert!(
            fs::rename(&path, dir.join("other.wav")).is_err(),
            "rename should be denied while locked"
        );
        assert!(fs::read(&path).is_ok(), "reads must still work");
        assert!(
            fs::File::open(&path).is_ok(),
            "a second read handle must still be allowed"
        );

        drop(lock);
        assert!(fs::remove_file(&path).is_ok(), "lock did not release");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_file_cannot_be_locked() {
        assert!(FileLock::acquire(Path::new("Z:\\nope.wav")).is_err());
    }
}
