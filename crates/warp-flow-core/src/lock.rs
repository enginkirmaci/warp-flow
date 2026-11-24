//! One run per repository, across processes.
//!
//! The host already refuses a second run inside one process, but nothing stops
//! the CLI and a window from driving the same issue folder at once. Both write
//! `run.md`, ask questions and stage commits, so two engines would interleave
//! their records and their merges. This lock is held for the runner's whole
//! life; the kernel releases it when the process exits, however it exits, so a
//! crash does not leave a lock a person has to remove.
//!
//! The lock file lives inside the repository's `.git/`, where it cannot make
//! the checkout dirty (or in the issue folder when the runner has no
//! repository). Its contents name the owner for the refusal message; the file
//! itself is not the lock.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::run::timestamp;
use crate::{Error, Result, VERSION};

/// The lock file's name inside the folder it guards.
const LOCK_FILE: &str = ".warp-flow.lock";

/// An exclusive, process-wide claim on one run.
///
/// Dropping it closes the file, which releases the operating-system lock.
pub(crate) struct RunLock {
    _file: File,
    _path: PathBuf,
}

impl RunLock {
    /// Claim `folder` for this process, or say who holds it.
    ///
    /// `folder` is the repository's `.scratch/` when a repository is known, and
    /// the issue folder otherwise. It is created when missing.
    pub(crate) fn acquire(folder: &Path) -> Result<Self> {
        fs::create_dir_all(folder).map_err(|error| Error::io(folder, error))?;
        let path = folder.join(LOCK_FILE);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| Error::io(&path, error))?;
        // A lock is per open file description, and a child inherits the
        // descriptions of the process that forked it until it execs. Another
        // run's gate can fork at the instant this run's predecessor releases
        // its lock, so a first `try_lock` can see that short-lived holder.
        // Twenty short retries outlive the fork-to-exec window without waiting
        // on a real second engine.
        let mut waits = 0;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if waits < 20 => {
                    waits += 1;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    let owner = fs::read_to_string(&path).unwrap_or_default();
                    let owner = owner.trim().replace('\n', " ");
                    let who = if owner.is_empty() {
                        "an unknown process".to_owned()
                    } else {
                        owner
                    };
                    return Err(Error::Run(format!(
                        "another warp-flow process is already driving this run ({who}); stop \
                         it before starting another engine on `{}`",
                        folder.display()
                    )));
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(Error::io(&path, error));
                }
            }
        }
        // The lock is ours, so the file may be rewritten with the current
        // owner. A failure to write is not a failure to lock: the message is
        // a convenience, not the claim.
        let _ = file.set_len(0);
        let _ = writeln!(
            file,
            "pid {} · warp-flow {} · since {}",
            std::process::id(),
            VERSION,
            timestamp(SystemTime::now())
        );
        let _ = file.flush();
        Ok(Self {
            _file: file,
            _path: path,
        })
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}
