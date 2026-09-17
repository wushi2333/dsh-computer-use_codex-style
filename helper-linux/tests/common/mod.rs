//! Process-wide isolation of the AT-SPI / D-Bus environment for the gated Xvfb suites.
//!
//! # Why this module exists
//!
//! A fixture that starts a *private* session bus is not enough to keep the accessibility
//! stack private. `at-spi-bus-launcher` derives the accessibility bus socket path from
//! `$XDG_RUNTIME_DIR` -- **not** from the session bus it was activated on:
//!
//! ```text
//! launcher socket = $XDG_RUNTIME_DIR/at-spi/bus_N
//! ```
//!
//! Services activated on a session bus are spawned by that bus's `dbus-daemon`, so they
//! inherit the **daemon's** environment -- not the environment of the client that asked for
//! the activation. `DBUS_SESSION_BUS_ADDRESS` in the test process therefore cannot reach the
//! launcher. A private session bus plus an inherited `XDG_RUNTIME_DIR` puts the private
//! accessibility bus at the *desktop's* `/run/user/1000/at-spi/bus_0`. The launcher unlinks
//! the socket that is already there before binding its own, and unlinks it again on exit:
//!
//! ```text
//! unlink("/run/user/1000/at-spi/bus_0") = 0
//! bind(10, {sa_family=AF_UNIX, sun_path="/run/user/1000/at-spi/bus_0"}, 110) = 0
//! ```
//!
//! The desktop's own daemon keeps running, so established connections survive and the damage
//! stays invisible until something new connects -- the accessor then gets
//! `No such file or directory`, and the whole desktop's AT-SPI is down until
//! `systemctl --user restart at-spi-dbus-bus.service`. A test run must never do that to the
//! session it was started from.
//!
//! # How isolation is achieved
//!
//! The private session bus is started with `XDG_RUNTIME_DIR` (and `XDG_CONFIG_HOME`, plus
//! the other XDG roots) pointing at a private directory. The daemon holds that environment
//! and passes it to every service it activates, so the `at-spi-bus-launcher` it spawns binds
//! `$XDG_RUNTIME_DIR/at-spi/bus_0` **inside the private directory**. `install_env` presents
//! the same values to the test process, so the helper asks the private bus for the
//! accessibility address and is handed the private socket. Nothing under the desktop's
//! runtime directory is read, written or unlinked.
//!
//! `XDG_CONFIG_HOME` also moves the per-user D-Bus service directory aside -- it is what
//! `<standard_session_servicedirs/>` resolves for `$XDG_CONFIG_HOME/dbus-1/services`.
//!
//! # Lifetime
//!
//! The tree is created with mode `0700`: `XDG_RUNTIME_DIR` is required to be private, and
//! `dbus-daemon` refuses to create sockets in a directory others can read. `Drop` sends
//! `SIGTERM` to the private session bus -- taking the launcher it spawned with it -- restores
//! the environment this process had before, and removes the tree.

#![cfg(target_os = "linux")]
// Every test binary compiles this module for itself, so a binary that uses only part of it
// would otherwise warn about the rest. This is the standard integration-test shape.
#![allow(dead_code)]

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The variable that turns the gated suites on.
pub const ENABLED: &str = "DSH_CUA_XVFB_TEST";

/// The environment variables a private session owns and restores.
const PRIVATE_ENV: &[&str] = &[
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_SESSION_TYPE",
    "NO_AT_BRIDGE",
];

/// Whether the gated suites were asked to run.
pub fn enabled() -> bool {
    std::env::var(ENABLED)
        .map(|value| value == "1")
        .unwrap_or(false)
}

/// The desktop accessibility socket of the session this run happens on.
///
/// Read **before** any fixture starts, because that is the only moment it is guaranteed to be
/// the desktop's own: after a fixture has run it may be whatever the fixture left behind.
/// `None` when the run has no live accessibility socket (a bare CI container, for example) --
/// the safety gate then reports "no socket to protect" instead of blaming the test.
pub fn desktop_a11y_socket() -> Option<PathBuf> {
    DESKTOP_A11Y_SOCKET.get().cloned().flatten()
}

/// Pinned by [`pin_desktop_a11y_socket`], which every fixture calls *before* it changes the
/// environment.
///
/// The pin has to be taken at that moment and not lazily: once a fixture has installed its own
/// `XDG_RUNTIME_DIR`, reading "the desktop's socket" out of the environment returns the
/// **fixture's** socket, and a gate built on that value would check the wrong file while
/// reporting success.
static DESKTOP_A11Y_SOCKET: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Record where the desktop's accessibility socket is, while the environment still says so.
///
/// Idempotent, so every fixture can call it freely. `None` is pinned too: it means this run has
/// no accessibility socket to protect (a bare CI container), and that answer must not be
/// silently re-derived later from a fixture's environment.
pub fn pin_desktop_a11y_socket() {
    DESKTOP_A11Y_SOCKET.get_or_init(|| {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
        let path = PathBuf::from(runtime).join("at-spi").join("bus_0");
        path.exists().then_some(path)
    });
}

/// Whether a fresh process can actually connect to the socket at `path`.
///
/// `Path::exists` only proves a file is there. A socket whose owner exited stays on disk, and
/// a *live* daemon whose socket was unlinked is exactly the failure this module guards
/// against, so the honest check is to connect. It runs in a child process so the probe cannot
/// leave a connection on the bus.
pub fn socket_accepts_connections(path: &Path) -> bool {
    let Some(address) = path.to_str().map(|value| format!("unix:path={value}")) else {
        return false;
    };
    Command::new("busctl")
        .arg(format!("--address={address}"))
        .args(["--no-pager", "--timeout=5", "list"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// A private runtime directory plus the private session bus living in it.
///
/// Everything the accessibility stack needs is scoped to `root`; see the module docs for why
/// that is a safety property and not tidiness.
///
/// The isolation is process-wide because it has to be -- the helper reads `XDG_RUNTIME_DIR`
/// and `DBUS_SESSION_BUS_ADDRESS` from its own environment. Suites that share one fixture at a
/// time are therefore serialized by their display lock, and each fixture owns the session for
/// as long as it is alive.
pub struct PrivateSession {
    root: PathBuf,
    address: String,
    pid: i32,
    previous: Vec<(&'static str, Option<OsString>)>,
}

/// Hands out a distinct number to every private tree in this process.
static NEXT_TREE: AtomicU32 = AtomicU32::new(0);

impl PrivateSession {
    /// Create the private runtime directory and start a private session bus in it.
    ///
    /// Returns `None` only when the tooling is missing or `dbus-daemon` refused to start; the
    /// caller decides whether that is a skip or a failure (the `DSH_CUA_XVFB_REQUIRE` gate).
    pub fn start() -> Option<Self> {
        let root = private_root("session");
        let config = root.join("session.conf");
        let mut file = std::fs::File::create(&config).ok()?;
        // <standard_session_servicedirs/> is what makes the accessibility stack reachable on a
        // private bus: the toolkit app activates org.a11y.Bus from those service files, exactly
        // as it would on a login session, so nothing here needs a real desktop.
        file.write_all(
            br#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <standard_session_servicedirs/>
  <policy context="default">
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
    <allow own="*"/>
    <allow user="*"/>
  </policy>
</busconfig>
"#,
        )
        .ok()?;

        // This environment is what makes the session private. XDG_RUNTIME_DIR decides where the
        // a11y launcher puts its socket; removing DBUS_SESSION_BUS_ADDRESS keeps the daemon from
        // being handed the desktop's bus to attach to or to advertise.
        let out = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--fork", "--print-address=1", "--print-pid=1"])
            .env("XDG_RUNTIME_DIR", &root)
            .env("XDG_CONFIG_HOME", root.join("conf"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .output()
            .ok()?;
        if !out.status.success() {
            let _ = std::fs::remove_dir_all(&root);
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines();
        let address = lines.next()?.trim().to_string();
        let pid: i32 = lines.next()?.trim().parse().ok()?;

        let session = Self {
            root,
            address,
            pid,
            previous: Vec::new(),
        };
        // The daemon already holds the right environment for anything it spawns, but the bus
        // also *publishes* an activation environment over org.freedesktop.DBus. Setting it
        // explicitly costs one round trip and makes the isolation independent of how the daemon
        // happened to inherit it.
        session.publish_activation_environment();
        Some(session)
    }

    /// The private runtime directory; `at-spi/bus_0` inside it is the private a11y socket.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The private session bus address.
    pub fn address(&self) -> &str {
        &self.address
    }

    fn publish_activation_environment(&self) {
        // Deliberately *not* `--systemd`: importing these values into the user's systemd
        // manager would rewrite the real session's activation environment, which is the very
        // state this module exists to leave alone.
        let _ = Command::new("dbus-update-activation-environment")
            .args([
                "--",
                "XDG_RUNTIME_DIR",
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_CACHE_HOME",
            ])
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .env("XDG_RUNTIME_DIR", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("conf"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .output();
    }

    /// Install this private session in the test process's own environment.
    ///
    /// Services are spawned by the private `dbus-daemon`, whose environment `start` already
    /// fixed; this call covers everything the **helper** does. The helper reads
    /// `XDG_RUNTIME_DIR` and `DBUS_SESSION_BUS_ADDRESS` straight out of its own environment,
    /// so presenting the private values is what routes its AT-SPI connection to the private bus
    /// instead of the desktop's.
    ///
    /// It is also the defence against `diagnostics::hydrate_session_bus_env`, which backfills
    /// `XDG_RUNTIME_DIR` and `DBUS_SESSION_BUS_ADDRESS` from the real session when they are
    /// missing. Both are set explicitly here, so that backfill has nothing to do.
    ///
    /// The previous values are restored by `Drop`.
    pub fn install_env(&mut self) {
        if self.previous.is_empty() {
            self.previous = PRIVATE_ENV
                .iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect();
        }
        for (key, value) in self.env_pairs() {
            std::env::set_var(key, value);
        }
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &self.address);
        std::env::set_var("XDG_SESSION_TYPE", "x11");
        // The bridge must be on for a toolkit app to publish a tree, and must not be suppressed
        // by whatever the operator's environment says.
        std::env::set_var("NO_AT_BRIDGE", "0");
    }

    fn env_pairs(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("XDG_RUNTIME_DIR", self.root.clone()),
            ("XDG_CONFIG_HOME", self.root.join("conf")),
            ("XDG_DATA_HOME", self.root.join("data")),
            ("XDG_CACHE_HOME", self.root.join("cache")),
        ]
    }

    /// Point a child process at this private session.
    pub fn configure_command<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        for (key, value) in self.env_pairs() {
            command.env(key, value);
        }
        command.env("DBUS_SESSION_BUS_ADDRESS", &self.address);
        command.env("XDG_SESSION_TYPE", "x11");
        command.env("NO_AT_BRIDGE", "0");
        command
    }
}

impl Drop for PrivateSession {
    fn drop(&mut self) {
        // SIGTERM to the private session bus. The launcher it spawned is its child, so the
        // private accessibility bus goes away with it instead of lingering as an orphan.
        unsafe {
            libc::kill(self.pid, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && is_pid_alive(self.pid) {
            std::thread::sleep(Duration::from_millis(50));
        }
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        // The private tree holds this run's a11y socket, session socket and dconf state; none
        // of it is worth keeping, and leaving it behind would accumulate a runtime directory
        // per test run.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A unique, private `XDG_CONFIG_HOME` for a suite that starts no session bus of its own.
///
/// The `xvfb_window2` fixture runs raw X clients and never starts a session bus, but its
/// `launch_app` tests start desktop applications, and a toolkit app that cannot reach a
/// session bus falls back to `$XDG_RUNTIME_DIR/at-spi/bus_0` over the X11 properties -- the
/// desktop's accessibility socket. Moving `XDG_CONFIG_HOME` (the per-user D-Bus service
/// directory) aside keeps such a fallback from finding a bus to talk to. Nothing the suite
/// asserts depends on a session bus being present.
pub struct PrivateConfig {
    root: PathBuf,
    previous: Option<OsString>,
}

impl PrivateConfig {
    pub fn new(tag: &str) -> Option<Self> {
        let root = private_root(tag);
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", &root);
        Some(Self { root, previous })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for PrivateConfig {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Create a fresh, empty, mode-0700 private tree under the temp directory.
fn private_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "cua-xvfb-{tag}-{}-{}",
        std::process::id(),
        NEXT_TREE.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    // XDG_RUNTIME_DIR must not be readable by anyone else: dbus-daemon refuses to create its
    // sockets in a directory others can read, and the a11y socket is under the same rule.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
    for subdir in ["conf", "data", "cache"] {
        let path = root.join(subdir);
        let _ = std::fs::create_dir_all(&path);
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
    }
    root
}

fn is_pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tooling_available() -> bool {
        ["dbus-daemon", "busctl"]
            .iter()
            .all(|tool| Command::new(tool).arg("--help").output().is_ok())
    }

    /// The whole point of the module: a fixture's private session must not be the desktop's.
    ///
    /// This is the regression test for the bug that deleted a live desktop's
    /// `/run/user/1000/at-spi/bus_0`; it fails if `start` ever stops setting
    /// `XDG_RUNTIME_DIR` for the daemon, which is what made the private launcher bind the
    /// desktop's socket.
    #[test]
    fn a_private_session_is_never_the_desktop_runtime_dir() {
        if !tooling_available() {
            eprintln!("SKIP: dbus-daemon or busctl is unavailable");
            return;
        }
        let desktop = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        let session = PrivateSession::start().expect("a private session bus starts");
        assert_ne!(
            Some(session.root().to_path_buf()),
            desktop,
            "the private runtime dir must not be the desktop's"
        );
        assert!(
            session.address().starts_with("unix:"),
            "a session bus address is a unix: address: {}",
            session.address()
        );
        // The private tree must be usable as XDG_RUNTIME_DIR: private, and owned by us.
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(session.root()).expect("the private root exists");
        assert_eq!(metadata.mode() & 0o777, 0o700, "XDG_RUNTIME_DIR must be private");
        assert_eq!(metadata.uid(), unsafe { libc::getuid() });
    }

    /// The environment is process-global state; a fixture must hand it back untouched.
    #[test]
    fn installing_a_private_session_restores_the_previous_environment() {
        if !tooling_available() {
            eprintln!("SKIP: dbus-daemon or busctl is unavailable");
            return;
        }
        let before: Vec<_> = PRIVATE_ENV
            .iter()
            .map(|key| (*key, std::env::var_os(key)))
            .collect();
        let mut session = PrivateSession::start().expect("a private session bus starts");
        let root = session.root().to_path_buf();
        session.install_env();
        assert_eq!(
            std::env::var("XDG_RUNTIME_DIR").ok(),
            root.to_str().map(str::to_string),
            "the fixture must present its own runtime dir"
        );
        drop(session);
        for (key, value) in before {
            assert_eq!(
                std::env::var_os(key),
                value,
                "{key} must be restored when the fixture ends"
            );
        }
        assert!(
            !root.exists(),
            "the private tree {} must be removed when the fixture ends",
            root.display()
        );
    }
}
