//! Single-instance lockfile guard for feature 005 (FR-050, FR-051, FR-052, FR-102).
//!
//! Hardened per analyze finding resolutions:
//! - `O_NOFOLLOW` opens disable symlink traversal (CHK055, CVE-2025-68146 class)
//! - `mode 0600` restricts PID disclosure (CHK056)
//! - `realpath` canonicalization resolves `/var/run → /private/var/run` (CHK057)
//! - `statfs(2)` denylist rejects NFS/SMB/FAT32 mounts (CHK058, FR-102)
//! - PID in lockfile is untrusted diagnostic data only (CHK060)
//! - Race windows enumerated in FR-051 and defeated by construction
//!
//! Mechanism: `flock(LOCK_EX | LOCK_NB)` via the `fs4` crate on
//! `/var/run/fand-smc.lock`. Lock released automatically on fd close, including
//! on `SIGKILL` (kernel releases advisory file locks on process death).

#![allow(unsafe_code)] // libc::open + statfs FFI
#![allow(clippy::missing_errors_doc)] // Errors are propagated via the new Result variant in SmcError

use std::fs::File;
use std::io::Write;
use std::os::fd::FromRawFd;
use std::path::PathBuf;

use fs4::fs_std::FileExt;

/// Default lockfile path. Canonicalized at runtime to resolve the macOS
/// `/var/run → /private/var/run` symlink.
pub const DEFAULT_LOCKFILE_PATH: &str = "/var/run/fand-smc.lock";

/// Filesystems where `flock` is unreliable or unsupported. fand refuses to
/// start if the lockfile parent would land on one of these.
const FLOCK_UNRELIABLE_FS: &[&[u8]] = &[
    b"nfs",
    b"smbfs",
    b"msdos",
    b"exfat",
    b"cd9660",
];

/// RAII guard over the exclusive single-instance lockfile.
///
/// While a `FlockGuard` exists, no other fand instance can write to the SMC.
/// The kernel releases the lock automatically on fd close (normal drop or
/// process death including `SIGKILL`).
#[derive(Debug)]
pub struct FlockGuard {
    /// Kept open for the lifetime of the guard. Dropping this releases the lock.
    _file: File,
    /// Canonicalized lockfile path for diagnostics.
    canonical_path: PathBuf,
    /// PID of the holder as written into the file (this process's PID on acquire).
    holder_pid: libc::pid_t,
}

/// Errors that can occur during `FlockGuard::try_acquire`.
#[derive(Debug)]
pub enum FlockError {
    /// The canonical parent directory is on a filesystem where `flock` is unreliable.
    /// Carries the `f_fstypename` string as seen by `statfs(2)`.
    UnreliableFilesystem(String),
    /// The lockfile could not be created (parent missing, EROFS, ENOENT, etc.).
    CreateFailed(std::io::Error),
    /// The lockfile was already held by another fand instance.
    AlreadyHeld { holder_pid: Option<libc::pid_t> },
    /// Symlink detected — `O_NOFOLLOW` rejected the open.
    SymlinkRejected,
    /// Canonicalization failed (realpath error).
    CanonicalizationFailed(std::io::Error),
    /// A write to the lockfile failed (ftruncate or PID write).
    WriteFailed(std::io::Error),
}

impl FlockGuard {
    /// Try to acquire an exclusive lock on `DEFAULT_LOCKFILE_PATH`.
    ///
    /// Non-blocking. Returns an error immediately if held, if the filesystem
    /// is unreliable, or if a symlink-swap attack is detected.
    pub fn try_acquire() -> Result<Self, FlockError> {
        Self::try_acquire_at(DEFAULT_LOCKFILE_PATH)
    }

    /// Try to acquire the lock at a specific path. Testing hook.
    pub fn try_acquire_at(path: &str) -> Result<Self, FlockError> {
        // Step 1: canonicalize the parent directory (not the lockfile itself —
        // it might not exist yet). Use the parent so we can `statfs` it even
        // when the lockfile is missing.
        let parent = std::path::Path::new(path)
            .parent()
            .unwrap_or_else(|| std::path::Path::new("/"));
        let canonical_parent = std::fs::canonicalize(parent)
            .map_err(FlockError::CanonicalizationFailed)?;

        // Step 2: statfs the canonical parent and reject unreliable filesystems
        // (FR-102 / CHK058).
        Self::check_filesystem(&canonical_parent)?;

        // Step 3: reconstruct the canonical lockfile path as
        // `canonical_parent/<basename>`.
        let basename = std::path::Path::new(path)
            .file_name()
            .ok_or_else(|| FlockError::CreateFailed(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "lockfile path has no basename",
            )))?;
        let canonical_path = canonical_parent.join(basename);

        // Step 4: open with O_CREAT | O_WRONLY | O_NOFOLLOW, mode 0600
        // (CHK055, CHK056). Use raw libc::open because std::fs::OpenOptions
        // does not expose O_NOFOLLOW on all platforms.
        let c_path = std::ffi::CString::new(
            canonical_path.as_os_str().as_encoded_bytes(),
        )
        .map_err(|e| FlockError::CreateFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("lockfile path contains NUL: {e}"),
        )))?;

        // SAFETY: c_path is a valid null-terminated C string owned by this scope.
        // The open flags are well-formed per POSIX. The mode 0o600 is a literal.
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_CREAT | libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600_i32,
            )
        };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            return match err.raw_os_error() {
                Some(libc::ELOOP) => Err(FlockError::SymlinkRejected),
                _ => Err(FlockError::CreateFailed(err)),
            };
        }

        // SAFETY: fd is a valid open file descriptor owned by this function.
        // Wrapping it in File transfers ownership; File's Drop will close it.
        let file = unsafe { File::from_raw_fd(fd) };

        // Step 5: try the exclusive non-blocking lock.
        // `fs4::try_lock_exclusive` returns Ok(()) on success and
        // Err(ErrorKind::WouldBlock) when the lock is already held.
        if let Err(e) = file.try_lock_exclusive() {
            // Read the PID content for the diagnostic (treated as untrusted).
            let holder_pid = Self::read_holder_pid_from_path(&canonical_path);
            drop(file); // release fd
            // Any error from try_lock_exclusive is treated as "already held"
            // for diagnostic purposes — the only realistic error kinds are
            // WouldBlock (EWOULDBLOCK) and filesystem-level issues which are
            // rare on local APFS.
            let _ = e;
            return Err(FlockError::AlreadyHeld { holder_pid });
        }

        // Step 6: truncate and write our own PID (diagnostic only per CHK060).
        // NOTE: truncate AFTER lock, never before — the lock is the
        // synchronization point (FR-051 race-window (b)).
        let our_pid = std::process::id() as libc::pid_t;
        let mut writable = &file;
        if let Err(e) = writable.set_len(0) {
            return Err(FlockError::WriteFailed(e));
        }
        if let Err(e) = writeln!(writable, "{our_pid}") {
            return Err(FlockError::WriteFailed(e));
        }
        if let Err(e) = writable.flush() {
            return Err(FlockError::WriteFailed(e));
        }

        Ok(Self {
            _file: file,
            canonical_path,
            holder_pid: our_pid,
        })
    }

    /// Check the filesystem type of the given canonical parent directory against
    /// the unreliable-flock denylist (FR-102).
    fn check_filesystem(parent: &std::path::Path) -> Result<(), FlockError> {
        #[cfg(target_os = "macos")]
        unsafe {
            let c_parent = match std::ffi::CString::new(
                parent.as_os_str().as_encoded_bytes(),
            ) {
                Ok(s) => s,
                Err(_) => return Ok(()), // path contains NUL — fall through, open will fail later
            };
            let mut buf: libc::statfs = core::mem::zeroed();
            if libc::statfs(c_parent.as_ptr(), &mut buf) != 0 {
                // statfs failed — treat as a soft warning; don't block the
                // acquire, the subsequent open will surface the real error.
                return Ok(());
            }
            // f_fstypename is a fixed-size char array; convert to bytes until NUL.
            let fstype_bytes: &[u8] = {
                let raw: &[i8; 16] = &buf.f_fstypename;
                let ptr = raw.as_ptr().cast::<u8>();
                let mut len = 0;
                while len < 16 && *ptr.add(len) != 0 {
                    len += 1;
                }
                core::slice::from_raw_parts(ptr, len)
            };
            if FLOCK_UNRELIABLE_FS.iter().any(|deny| *deny == fstype_bytes) {
                let type_str = String::from_utf8_lossy(fstype_bytes).into_owned();
                return Err(FlockError::UnreliableFilesystem(type_str));
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = parent;
        }
        Ok(())
    }

    /// Return the canonicalized lockfile path.
    #[must_use]
    pub fn canonical_path(&self) -> &std::path::Path {
        &self.canonical_path
    }

    /// Return the PID the lockfile was last stamped with.
    ///
    /// For a held guard, this is always the current process's PID. For a
    /// conflict diagnostic, callers should use `read_holder_pid_from_path`
    /// directly on the conflicted path.
    #[must_use]
    pub fn holder_pid(&self) -> libc::pid_t {
        self.holder_pid
    }

    /// Read the PID recorded in the lockfile at `path`. Returns `None` if the
    /// file is missing, empty, malformed, or unreadable.
    ///
    /// **CRITICAL**: the returned value is **untrusted diagnostic data only**
    /// (CHK060). Callers MUST NOT use it as authoritative liveness proof — a
    /// stale PID, a recycled PID, or an attacker-controlled PID may all be
    /// returned. The authoritative "is the lock held?" answer comes from
    /// `flock(LOCK_EX | LOCK_NB)` failing, never from this function.
    #[must_use]
    pub fn read_holder_pid_from_path(path: &std::path::Path) -> Option<libc::pid_t> {
        let contents = std::fs::read_to_string(path).ok()?;
        contents.trim().parse::<libc::pid_t>().ok()
    }
}

impl Drop for FlockGuard {
    fn drop(&mut self) {
        // Explicitly unlock before dropping the File. BSD flock semantics say
        // the lock is released on fd close, but fs4's internal bookkeeping
        // needs the explicit unlock call to avoid leaving stale state on
        // some Darwin kernels where rapid acquire/release cycles observe
        // inconsistent lock-held snapshots.
        let _ = fs4::fs_std::FileExt::unlock(&self._file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_lockfile(name: &str) -> String {
        // Include PID + a random nonce so the lib-test and bin-test binaries
        // (which cargo runs as separate processes sharing /tmp) don't race on
        // the same filename. Also protects against reruns leaving stale files.
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let nonce = {
            let mut buf = [0u8; 8];
            unsafe { libc::getentropy(buf.as_mut_ptr().cast(), buf.len()); }
            u64::from_ne_bytes(buf)
        };
        dir.join(format!("{pid}-{nonce:016x}-{name}"))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn acquire_and_release_cycle() {
        let path = temp_lockfile("fand-test-acquire-release.lock");
        let _ = std::fs::remove_file(&path);

        let guard = match FlockGuard::try_acquire_at(&path) {
            Ok(g) => g,
            Err(e) => panic!("first acquire at {path} failed: {e:?}"),
        };
        assert_eq!(guard.holder_pid(), std::process::id() as libc::pid_t);
        drop(guard);

        // Second acquire after drop should succeed (lock was released).
        let guard2 = match FlockGuard::try_acquire_at(&path) {
            Ok(g) => g,
            Err(e) => panic!("second acquire at {path} failed: {e:?}"),
        };
        drop(guard2);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn conflict_detection() {
        let path = temp_lockfile("fand-test-conflict.lock");
        let _ = std::fs::remove_file(&path);

        let _guard = FlockGuard::try_acquire_at(&path).expect("first acquire");
        // Second acquire with first still alive must fail with AlreadyHeld.
        let err = FlockGuard::try_acquire_at(&path).expect_err("second acquire should fail");
        assert!(matches!(err, FlockError::AlreadyHeld { .. }));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mode_is_0600() {
        let path = temp_lockfile("fand-test-mode.lock");
        let _ = std::fs::remove_file(&path);

        let _guard = FlockGuard::try_acquire_at(&path).expect("acquire");
        let meta = std::fs::metadata(&path).expect("stat");
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "lockfile mode must be 0600 per FR-050");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn symlink_attack_rejected() {
        let path = temp_lockfile("fand-test-symlink-attack.lock");
        let target = temp_lockfile("fand-test-symlink-target.txt");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&target);
        // Create target file then a symlink pointing to it as the lockfile.
        std::fs::write(&target, "decoy").expect("create target");
        std::os::unix::fs::symlink(&target, &path).expect("create symlink");

        let err = FlockGuard::try_acquire_at(&path).expect_err("symlink must be rejected");
        assert!(
            matches!(err, FlockError::SymlinkRejected),
            "expected SymlinkRejected, got {err:?}"
        );

        // Target must be untouched (still contains the decoy content).
        let target_content = std::fs::read_to_string(&target).expect("read target");
        assert_eq!(target_content, "decoy", "target file must not be modified");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn pid_content_matches_process_id() {
        let path = temp_lockfile("fand-test-pid-content.lock");
        let _ = std::fs::remove_file(&path);

        let _guard = FlockGuard::try_acquire_at(&path).expect("acquire");
        let content = std::fs::read_to_string(&path).expect("read");
        let pid: libc::pid_t = content.trim().parse().expect("parse pid");
        assert_eq!(pid, std::process::id() as libc::pid_t);

        let _ = std::fs::remove_file(&path);
    }
}
