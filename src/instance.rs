//! One window per user.
//!
//! The app publishes its door endpoint in the config folder and a second
//! launch would write over it, leaving two windows that each believe they own
//! the door. The lock is held for the process's whole life; the kernel
//! releases it when the process ends, however it ends, so a crash does not
//! leave a lock a person has to remove.
//!
//! `--version` is answered before this lock is asked for: the family's About
//! list fills itself by running every installed member while the app may be
//! open, and a build that refused to answer would leave a blank row.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The lock file's name inside the folder it guards.
const LOCK_FILE: &str = "instance.lock";

/// This process's claim on the app's one window.
///
/// Dropping it closes the file, which releases the operating-system lock.
pub struct InstanceLock {
    _file: File,
    _path: PathBuf,
}

impl InstanceLock {
    /// Claim the app's config folder, or `None` when another instance has it.
    pub fn acquire() -> std::io::Result<Option<Self>> {
        Self::acquire_in(&crate::paths::AppPaths::config_dir())
    }

    /// The probe seam: the same lock in an explicit folder.
    fn acquire_in(folder: &Path) -> std::io::Result<Option<Self>> {
        fs::create_dir_all(folder)?;
        let path = folder.join(LOCK_FILE);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
        // The contents are for a person reading the folder; the lock is the
        // operating-system claim, not the file.
        file.set_len(0)?;
        writeln!(
            file,
            "pid {}\nversion {}",
            std::process::id(),
            env!("CARGO_PKG_VERSION")
        )?;
        Ok(Some(Self {
            _file: file,
            _path: path,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_instance_is_refused_and_a_dropped_one_frees_the_lock() {
        let dir =
            std::env::temp_dir().join(format!("warpflow-instance-probe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        let first = InstanceLock::acquire_in(&dir)
            .expect("the folder is writable")
            .expect("the first instance takes the lock");
        assert!(
            InstanceLock::acquire_in(&dir)
                .expect("a second open does not fail")
                .is_none(),
            "a second instance is refused while the first holds the lock"
        );

        drop(first);
        assert!(
            InstanceLock::acquire_in(&dir)
                .expect("a third open does not fail")
                .is_some(),
            "the kernel releases the lock when the first instance ends"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
