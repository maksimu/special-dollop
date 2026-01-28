// Profile isolation for RBI sessions
//
// Provides security mechanisms to ensure RBI sessions are properly isolated:
// 1. Profile lock files - Prevent concurrent use of the same persistent profile
// 2. DBus isolation - Linux namespace isolation for CEF (prevents cross-session IPC)
//
// Based on KCM's isolation implementation.

#[cfg(all(target_os = "linux", feature = "cef"))]
use log::error;
#[cfg(any(not(target_os = "linux"), all(target_os = "linux", feature = "cef")))]
use log::warn;
use log::{debug, info};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;

/// Name of the lock file placed in profile directories
const PROFILE_LOCK_FILE_NAME: &str = "guacr-rbi-profile.lock";

/// Profile directory creation mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProfileCreationMode {
    /// Don't create the directory - it must already exist
    #[default]
    MustExist,
    /// Create the directory if it doesn't exist (single level)
    Create,
    /// Recursively create all parent directories as needed
    CreateRecursive,
}

/// Manages an isolated browser profile directory with locking
///
/// When using persistent profile directories, this ensures only one
/// RBI session can use a given profile at a time via advisory file locks.
///
/// # Security
///
/// - Creates a lock file in the profile directory
/// - Uses `flock()` (or platform equivalent) for exclusive access
/// - Prevents data corruption from concurrent browser instances
/// - Lock is automatically released when this struct is dropped
pub struct ProfileLock {
    /// The lock file handle (keeps the lock active)
    #[cfg(target_os = "linux")]
    lock_file: Option<File>,
    #[cfg(not(target_os = "linux"))]
    lock_file: Option<File>,
    /// Path to the profile directory
    profile_path: PathBuf,
    /// Path to the lock file (stored for potential future use/debugging)
    #[allow(dead_code)]
    lock_file_path: PathBuf,
}

impl ProfileLock {
    /// Acquire an exclusive lock on a profile directory
    ///
    /// # Arguments
    ///
    /// * `profile_directory` - Path to the browser profile directory
    /// * `creation_mode` - How to handle directory creation
    ///
    /// # Returns
    ///
    /// * `Ok(ProfileLock)` - Lock acquired successfully
    /// * `Err(ProfileLockError)` - Failed to acquire lock
    ///
    /// # Security
    ///
    /// This prevents multiple RBI sessions from using the same profile
    /// directory simultaneously, which could lead to:
    /// - Cookie/session data corruption
    /// - localStorage conflicts
    /// - Cache corruption
    /// - Potential data leakage between sessions
    pub fn acquire(
        profile_directory: impl AsRef<Path>,
        creation_mode: ProfileCreationMode,
    ) -> Result<Self, ProfileLockError> {
        let profile_path = profile_directory.as_ref().to_path_buf();
        let lock_file_path = profile_path.join(PROFILE_LOCK_FILE_NAME);

        // Create directory if needed
        match creation_mode {
            ProfileCreationMode::MustExist => {
                if !profile_path.exists() {
                    return Err(ProfileLockError::DirectoryNotFound(profile_path));
                }
                if !profile_path.is_dir() {
                    return Err(ProfileLockError::NotADirectory(profile_path));
                }
            }
            ProfileCreationMode::Create => {
                if !profile_path.exists() {
                    fs::create_dir(&profile_path).map_err(|e| {
                        ProfileLockError::DirectoryCreationFailed(profile_path.clone(), e)
                    })?;
                    debug!("Created profile directory: {}", profile_path.display());
                }
            }
            ProfileCreationMode::CreateRecursive => {
                if !profile_path.exists() {
                    fs::create_dir_all(&profile_path).map_err(|e| {
                        ProfileLockError::DirectoryCreationFailed(profile_path.clone(), e)
                    })?;
                    debug!(
                        "Recursively created profile directory: {}",
                        profile_path.display()
                    );
                }
            }
        }

        // Create/open the lock file
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_file_path)
            .map_err(|e| ProfileLockError::LockFileCreationFailed(lock_file_path.clone(), e))?;

        // Acquire exclusive lock
        #[cfg(target_os = "linux")]
        {
            use libc::{flock, LOCK_EX, LOCK_NB};
            let fd = lock_file.as_raw_fd();

            // Non-blocking exclusive lock
            let result = unsafe { flock(fd, LOCK_EX | LOCK_NB) };
            if result != 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.raw_os_error() == Some(libc::EAGAIN)
                    || err.raw_os_error() == Some(libc::EWOULDBLOCK)
                {
                    return Err(ProfileLockError::ProfileInUse(profile_path));
                }
                return Err(ProfileLockError::LockFailed(lock_file_path, err));
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            // On non-Linux platforms, use fs2 crate or just warn
            // For now, we'll just log a warning and continue
            warn!("Profile locking not fully supported on this platform, proceeding without lock");
        }

        info!(
            "Acquired exclusive lock on profile: {}",
            profile_path.display()
        );

        Ok(Self {
            lock_file: Some(lock_file),
            profile_path,
            lock_file_path,
        })
    }

    /// Get the profile directory path
    pub fn path(&self) -> &Path {
        &self.profile_path
    }

    /// Release the lock explicitly (also happens on drop)
    pub fn release(mut self) {
        self.release_internal();
    }

    fn release_internal(&mut self) {
        if let Some(file) = self.lock_file.take() {
            #[cfg(target_os = "linux")]
            {
                use libc::{flock, LOCK_UN};
                let fd = file.as_raw_fd();
                unsafe {
                    flock(fd, LOCK_UN);
                }
            }
            drop(file);
            debug!("Released lock on profile: {}", self.profile_path.display());
        }
    }
}

impl Drop for ProfileLock {
    fn drop(&mut self) {
        self.release_internal();
    }
}

/// Errors that can occur during profile locking
#[derive(Debug)]
pub enum ProfileLockError {
    /// Profile directory does not exist
    DirectoryNotFound(PathBuf),
    /// Path exists but is not a directory
    NotADirectory(PathBuf),
    /// Failed to create the profile directory
    DirectoryCreationFailed(PathBuf, io::Error),
    /// Failed to create the lock file
    LockFileCreationFailed(PathBuf, io::Error),
    /// Failed to acquire lock on the file
    LockFailed(PathBuf, io::Error),
    /// Profile is already in use by another session
    ProfileInUse(PathBuf),
}

impl std::fmt::Display for ProfileLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileLockError::DirectoryNotFound(p) => {
                write!(f, "Profile directory does not exist: {}", p.display())
            }
            ProfileLockError::NotADirectory(p) => {
                write!(f, "Profile path is not a directory: {}", p.display())
            }
            ProfileLockError::DirectoryCreationFailed(p, e) => {
                write!(
                    f,
                    "Failed to create profile directory {}: {}",
                    p.display(),
                    e
                )
            }
            ProfileLockError::LockFileCreationFailed(p, e) => {
                write!(f, "Failed to create lock file {}: {}", p.display(), e)
            }
            ProfileLockError::LockFailed(p, e) => {
                write!(f, "Failed to lock {}: {}", p.display(), e)
            }
            ProfileLockError::ProfileInUse(p) => {
                write!(f, "Profile is already in use: {}", p.display())
            }
        }
    }
}

impl std::error::Error for ProfileLockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProfileLockError::DirectoryCreationFailed(_, e)
            | ProfileLockError::LockFileCreationFailed(_, e)
            | ProfileLockError::LockFailed(_, e) => Some(e),
            _ => None,
        }
    }
}

// ============================================================================
// DBus Isolation (Linux only, CEF only)
// ============================================================================

/// DBus isolation state for CEF sessions
///
/// On Linux, CEF uses DBus for various system interactions. To prevent
/// cross-session communication, we create an isolated DBus environment
/// using Linux namespaces.
///
/// # How it works
///
/// 1. Create a user namespace (for mount permissions without root)
/// 2. Create a mount namespace
/// 3. Bind-mount a private directory over /var/run/dbus
/// 4. Start a private dbus-daemon in that directory
///
/// This ensures CEF processes in different sessions cannot communicate
/// via DBus, even if they're running on the same host.
#[cfg(all(target_os = "linux", feature = "cef"))]
pub struct DbusIsolation {
    /// PID of the isolated dbus-daemon
    dbus_pid: Option<u32>,
    /// Path to the temporary DBus directory
    temp_dir: Option<tempfile::TempDir>,
}

#[cfg(all(target_os = "linux", feature = "cef"))]
impl DbusIsolation {
    /// Set up an isolated DBus environment
    ///
    /// # Safety
    ///
    /// This function uses Linux-specific system calls:
    /// - `unshare(CLONE_NEWUSER)` - Create user namespace
    /// - `unshare(CLONE_NEWNS)` - Create mount namespace
    /// - `mount()` - Bind mount private directory
    ///
    /// These require appropriate kernel support and may fail in
    /// restricted environments (some containers, older kernels).
    ///
    /// # Returns
    ///
    /// * `Ok(DbusIsolation)` - Isolation set up successfully
    /// * `Err(String)` - Failed to set up isolation
    pub fn setup() -> Result<Self, String> {
        use libc::{
            fork, getgid, getuid, mount, unshare, CLONE_NEWNS, CLONE_NEWUSER, MS_BIND, MS_PRIVATE,
            MS_REC,
        };
        use std::ffi::CString;
        use std::ptr;

        let original_uid = unsafe { getuid() };
        let original_gid = unsafe { getgid() };

        // Create user namespace
        let result = unsafe { unshare(CLONE_NEWUSER) };
        if result != 0 {
            return Err(format!(
                "Failed to create user namespace: {}",
                io::Error::last_os_error()
            ));
        }

        // Set up UID mapping
        // Map our original UID to 1000 inside the namespace
        let uid_map = format!("1000 {} 1\n", original_uid);
        fs::write("/proc/self/uid_map", &uid_map)
            .map_err(|e| format!("Failed to write uid_map: {}", e))?;

        // Disable setgroups (required before gid_map)
        fs::write("/proc/self/setgroups", "deny\n")
            .map_err(|e| format!("Failed to write setgroups: {}", e))?;

        // Set up GID mapping
        let gid_map = format!("1000 {} 1\n", original_gid);
        fs::write("/proc/self/gid_map", &gid_map)
            .map_err(|e| format!("Failed to write gid_map: {}", e))?;

        // Create mount namespace
        let result = unsafe { unshare(CLONE_NEWNS) };
        if result != 0 {
            return Err(format!(
                "Failed to create mount namespace: {}",
                io::Error::last_os_error()
            ));
        }

        // Make mounts private
        let root = CString::new("/").unwrap();
        let result = unsafe {
            mount(
                ptr::null(),
                root.as_ptr(),
                ptr::null(),
                MS_REC | MS_PRIVATE,
                ptr::null(),
            )
        };
        if result != 0 {
            return Err(format!(
                "Failed to make mounts private: {}",
                io::Error::last_os_error()
            ));
        }

        // Create temporary directory for DBus
        let temp_dir = tempfile::Builder::new()
            .prefix("guacr-cef-dbus-")
            .tempdir()
            .map_err(|e| format!("Failed to create DBus temp directory: {}", e))?;

        let temp_path = CString::new(temp_dir.path().to_string_lossy().as_bytes()).unwrap();
        let dbus_dir = CString::new("/var/run/dbus").unwrap();

        // Check if system DBus directory exists
        if !Path::new("/var/run/dbus").exists() {
            warn!("System DBus directory /var/run/dbus does not exist, skipping DBus isolation");
            return Ok(Self {
                dbus_pid: None,
                temp_dir: Some(temp_dir),
            });
        }

        // Bind mount our temp directory over system DBus
        let result = unsafe {
            mount(
                temp_path.as_ptr(),
                dbus_dir.as_ptr(),
                ptr::null(),
                MS_BIND,
                ptr::null(),
            )
        };
        if result != 0 {
            return Err(format!(
                "Failed to bind mount DBus directory: {}",
                io::Error::last_os_error()
            ));
        }

        debug!(
            "Mounted private DBus directory {} at /var/run/dbus",
            temp_dir.path().display()
        );

        // Set environment variables
        std::env::set_var(
            "DBUS_SYSTEM_BUS_ADDRESS",
            "unix:path=/var/run/dbus/system_bus_socket",
        );
        std::env::set_var(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/var/run/dbus/system_bus_socket",
        );

        // Fork and exec dbus-daemon
        let pid = unsafe { fork() };
        if pid < 0 {
            return Err(format!(
                "Failed to fork for dbus-daemon: {}",
                io::Error::last_os_error()
            ));
        }

        if pid == 0 {
            // Child process - exec dbus-daemon
            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new("/usr/bin/dbus-daemon")
                .args([
                    "--session",
                    "--nofork",
                    "--nosyslog",
                    "--nopidfile",
                    "--address=unix:path=/var/run/dbus/system_bus_socket",
                ])
                .exec();

            // If we get here, exec failed
            error!("Failed to exec dbus-daemon: {}", err);
            std::process::exit(1);
        }

        info!("Started isolated dbus-daemon with PID {}", pid);

        Ok(Self {
            dbus_pid: Some(pid as u32),
            temp_dir: Some(temp_dir),
        })
    }

    /// Clean up the isolated DBus environment
    pub fn cleanup(&mut self) {
        if let Some(pid) = self.dbus_pid.take() {
            debug!("Terminating dbus-daemon (PID: {})", pid);

            // Send SIGTERM
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }

            // Wait briefly
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Check if still running, send SIGKILL if needed
            let result = unsafe { libc::kill(pid as i32, 0) };
            if result == 0 {
                warn!("dbus-daemon did not terminate, sending SIGKILL");
                unsafe {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
            }

            // Reap the child
            let mut status: i32 = 0;
            unsafe {
                libc::waitpid(pid as i32, &mut status, libc::WNOHANG);
            }
        }

        // temp_dir is cleaned up automatically when dropped
        if self.temp_dir.take().is_some() {
            debug!("Cleaned up DBus temp directory");
        }
    }
}

#[cfg(all(target_os = "linux", feature = "cef"))]
impl Drop for DbusIsolation {
    fn drop(&mut self) {
        self.cleanup();
    }
}

// Stub for non-Linux or non-CEF builds
#[cfg(not(all(target_os = "linux", feature = "cef")))]
pub struct DbusIsolation;

#[cfg(not(all(target_os = "linux", feature = "cef")))]
impl DbusIsolation {
    pub fn setup() -> Result<Self, String> {
        debug!("DBus isolation not available on this platform/configuration");
        Ok(Self)
    }

    pub fn cleanup(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_lock_temp_dir() {
        // Create a temp directory for testing
        let temp = tempfile::tempdir().unwrap();
        let profile_path = temp.path().join("test_profile");

        // Should fail if directory doesn't exist (MustExist mode)
        let result = ProfileLock::acquire(&profile_path, ProfileCreationMode::MustExist);
        assert!(matches!(
            result,
            Err(ProfileLockError::DirectoryNotFound(_))
        ));

        // Should succeed with Create mode
        let lock = ProfileLock::acquire(&profile_path, ProfileCreationMode::Create).unwrap();
        assert!(profile_path.exists());
        assert!(profile_path.join(PROFILE_LOCK_FILE_NAME).exists());

        // Should fail to acquire second lock on same profile
        #[cfg(target_os = "linux")]
        {
            let result2 = ProfileLock::acquire(&profile_path, ProfileCreationMode::MustExist);
            assert!(matches!(result2, Err(ProfileLockError::ProfileInUse(_))));
        }

        // Release lock
        drop(lock);

        // Should succeed now
        let _lock2 = ProfileLock::acquire(&profile_path, ProfileCreationMode::MustExist).unwrap();
    }

    #[test]
    fn test_profile_creation_modes() {
        let temp = tempfile::tempdir().unwrap();

        // Test CreateRecursive
        let nested_path = temp.path().join("a/b/c/profile");
        let lock =
            ProfileLock::acquire(&nested_path, ProfileCreationMode::CreateRecursive).unwrap();
        assert!(nested_path.exists());
        drop(lock);

        // Test Create (single level)
        let single_path = temp.path().join("single_profile");
        let lock = ProfileLock::acquire(&single_path, ProfileCreationMode::Create).unwrap();
        assert!(single_path.exists());
        drop(lock);

        // Test Create fails for nested
        let nested_fail = temp.path().join("x/y/z");
        let result = ProfileLock::acquire(&nested_fail, ProfileCreationMode::Create);
        assert!(matches!(
            result,
            Err(ProfileLockError::DirectoryCreationFailed(_, _))
        ));
    }
}
