//! One lazily opened X11 connection, shared by every window2 request.
//!
//! The session is X11 or it is not; probing must never be fatal, so every constructor
//! returns `Result` and the caller degrades honestly instead of panicking. The
//! connection is opened once and reused because the window2 tools issue many small
//! requests per call (atoms, geometry, captures) and a fresh connection per request
//! would put a handshake in front of every screenshot.

use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Result};
use x11rb::connection::Connection as _;
use x11rb::protocol::composite::ConnectionExt as _;
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

/// Send one extension query and flatten x11rb's two failure modes into one message.
///
/// x11rb separates "the request could not be sent" (`ConnectionError`) from "the server
/// answered with an error" (`ReplyError`). Every probe in this module treats both the
/// same way — the extension is not usable — so this collapses them once instead of
/// repeating the same `.and_then(|cookie| cookie.reply())` mismatch at each call site.
macro_rules! probe_extension {
    ($caps:ident, $raw:expr, $call:expr, $version:expr) => {
        match $call {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => Some($version(reply)),
                Err(error) => {
                    $caps
                        .detail
                        .push(format!("{} was refused: {error:?}", $raw));
                    None
                }
            },
            Err(error) => {
                $caps
                    .detail
                    .push(format!("{} could not be queried: {error}", $raw));
                None
            }
        }
    };
}

/// Why an X11 request could not be served.
#[derive(Debug, Clone)]
pub enum X11Error {
    /// No usable X session (no DISPLAY, or the connect failed).
    NoSession(String),
    /// The server answered, but refused this particular request.
    Protocol(String),
    /// The extension the request needs is not present on this server.
    MissingExtension(String),
}

impl std::fmt::Display for X11Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            X11Error::NoSession(detail) => write!(f, "no X11 session: {detail}"),
            X11Error::Protocol(detail) => write!(f, "X11 request failed: {detail}"),
            X11Error::MissingExtension(name) => {
                write!(f, "the X server does not provide the {name} extension")
            }
        }
    }
}

impl std::error::Error for X11Error {}

/// An open connection plus the screen facts every request needs.
pub struct X11Connection {
    connection: RustConnection,
    screen_num: usize,
    root: u32,
    width: u16,
    height: u16,
    root_depth: u8,
    bits_per_pixel: u8,
}

impl X11Connection {
    /// Connect to `$DISPLAY`.
    pub fn connect() -> Result<Self, X11Error> {
        let (connection, screen_num) =
            x11rb::connect(None).map_err(|error| X11Error::NoSession(format!("{error}")))?;
        let screen = connection.setup().roots.get(screen_num).ok_or_else(|| {
            X11Error::NoSession(format!("screen {screen_num} is not in the setup reply"))
        })?;
        let bits_per_pixel = connection
            .setup()
            .pixmap_formats
            .iter()
            .find(|format| format.depth == screen.root_depth)
            .map(|format| format.bits_per_pixel)
            .unwrap_or_else(|| screen.root_depth.into());
        Ok(Self {
            root: screen.root,
            width: screen.width_in_pixels,
            height: screen.height_in_pixels,
            root_depth: screen.root_depth,
            bits_per_pixel,
            screen_num,
            connection,
        })
    }

    pub fn inner(&self) -> &RustConnection {
        &self.connection
    }

    pub fn root(&self) -> u32 {
        self.root
    }

    /// Whether the setup reply is reachable at all, used by health.
    pub fn setup_vendor(&self) -> String {
        String::from_utf8_lossy(&self.connection.setup().vendor).to_string()
    }

    pub fn screen_num(&self) -> usize {
        self.screen_num
    }

    pub fn screen_size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// The setup's protocol version, for health reporting.
    pub fn protocol_version(&self) -> (u16, u16) {
        (
            self.connection.setup().protocol_major_version,
            self.connection.setup().protocol_minor_version,
        )
    }

    pub fn root_depth(&self) -> u8 {
        self.root_depth
    }

    /// Bytes per pixel on the root visual, which is what a captured image is packed in.
    pub fn bytes_per_pixel(&self) -> usize {
        usize::from(self.bits_per_pixel / 8).max(1)
    }
}

impl std::fmt::Debug for X11Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11Connection")
            .field("root", &format_args!("{:#x}", self.root))
            .field("size", &format_args!("{}x{}", self.width, self.height))
            .field("root_depth", &self.root_depth)
            .finish()
    }
}

/// A cached connection together with the display it was opened on.
struct Cached {
    display: String,
    connection: X11Connection,
}

static SHARED: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();

/// Bumped every time a new connection is opened.
///
/// Atom values are meaningful only on the connection that interned them, and this
/// process can replace its connection (a session switch, or a dead socket). Anything
/// that caches per-connection identifiers keys on this counter so it can never outlive
/// the connection that produced it.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The generation of the current connection; changes whenever the connection does.
pub fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

fn slot() -> &'static Mutex<Option<Cached>> {
    SHARED.get_or_init(|| Mutex::new(None))
}

/// The display this process would connect to right now.
fn current_display() -> String {
    std::env::var("DISPLAY").unwrap_or_default()
}

/// Run `f` with the process-wide connection, opening it on first use.
///
/// The cache is keyed on `$DISPLAY`, not just "have we connected yet". A helper that
/// outlives a session change — the user switches sessions, or an X server is restarted —
/// must not keep sending requests to a display that is gone; the stale connection would
/// fail every call with a broken pipe until the process was restarted.
///
/// A failed connect is *not* cached either: a helper started before the session exists
/// (or on Wayland, then switched) has to be able to succeed later.
pub fn with_connection<T>(f: impl FnOnce(&X11Connection) -> T) -> Result<T> {
    let mut guard = slot()
        .lock()
        .map_err(|_| anyhow!("the X11 connection lock was poisoned"))?;
    let display = current_display();
    let stale = guard
        .as_ref()
        .is_some_and(|cached| cached.display != display);
    if stale {
        *guard = None;
    }
    if guard.is_none() {
        *guard = Some(Cached {
            display,
            connection: X11Connection::connect()?,
        });
        GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    let connection = &guard.as_ref().expect("just initialized").connection;
    Ok(f(connection))
}

/// Like [`with_connection`], for a closure that itself returns a `Result`.
///
/// Without this, every fallible request would come back as `Result<Result<..>>` and the
/// call sites would need a double `?` purely as ceremony.
pub fn with_connection_flat<T>(
    f: impl FnOnce(&X11Connection) -> Result<T>,
) -> Result<T> {
    with_connection(f)?
}

/// Whether a cached connection exists for the display in effect right now.
pub fn is_connected() -> bool {
    slot()
        .lock()
        .map(|guard| {
            guard
                .as_ref()
                .is_some_and(|cached| cached.display == current_display())
        })
        .unwrap_or(false)
}

/// Forget the cached connection, so the next request reconnects.
///
/// Used when a request fails with a connection-level error: the socket is gone, and
/// holding a dead connection would make every later call fail the same way.
pub fn invalidate() {
    if let Ok(mut guard) = slot().lock() {
        if guard.is_some() {
            *guard = None;
            GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// Capabilities the connected server actually advertises.
#[derive(Debug, Clone, Default)]
pub struct X11Capabilities {
    pub connected: bool,
    pub shm: Option<(u16, u16)>,
    pub xtest: Option<(u8, u16)>,
    pub xfixes: Option<(u32, u32)>,
    pub composite: Option<(u32, u32)>,
    pub window_manager: Option<u32>,
    pub detail: Vec<String>,
}

/// Ask the server which extensions back the window2 surface.
///
/// Every probe is independent: a missing Composite must not stop XTest from being
/// reported, because that is exactly the degradation health has to describe.
pub fn capabilities() -> Result<X11Capabilities> {
    with_connection(|connection| {
        let raw = connection.inner();
        let mut caps = X11Capabilities {
            connected: true,
            ..Default::default()
        };
        caps.shm = probe_extension!(
            caps,
            "MIT-SHM",
            raw.shm_query_version(),
            |reply: x11rb::protocol::shm::QueryVersionReply| (
                reply.major_version,
                reply.minor_version
            )
        );
        caps.xtest = probe_extension!(
            caps,
            "XTest",
            raw.xtest_get_version(2, 2),
            |reply: x11rb::protocol::xtest::GetVersionReply| (
                reply.major_version,
                reply.minor_version
            )
        );
        caps.xfixes = probe_extension!(
            caps,
            "XFixes",
            raw.xfixes_query_version(6, 0),
            |reply: x11rb::protocol::xfixes::QueryVersionReply| (
                reply.major_version,
                reply.minor_version
            )
        );
        caps.composite = probe_extension!(
            caps,
            "XComposite",
            raw.composite_query_version(0, 4),
            |reply: x11rb::protocol::composite::QueryVersionReply| (
                reply.major_version,
                reply.minor_version
            )
        );
        caps.window_manager = crate::x11::window::window_manager_window(connection).ok().flatten();
        caps
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connecting_outside_an_x_session_is_an_error_not_a_panic() {
        // A helper started on Wayland, or from a service without DISPLAY, must come
        // back with an honest error instead of aborting the process.
        match X11Connection::connect() {
            Ok(connection) => assert_ne!(connection.root(), 0),
            Err(error) => assert!(!error.to_string().is_empty()),
        }
    }

    #[test]
    fn the_connection_cache_is_keyed_on_the_display() {
        // The invariant that matters: a different DISPLAY must not reuse a connection
        // opened for another one. Observed through the cache rather than by connecting,
        // so the test does not need a live server.
        let previous = std::env::var("DISPLAY").ok();
        std::env::set_var("DISPLAY", format!(":{}", 4001));
        let first = current_display();
        std::env::set_var("DISPLAY", format!(":{}", 4002));
        let second = current_display();
        assert_ne!(first, second, "the display name must drive the cache key");
        match previous {
            Some(value) => std::env::set_var("DISPLAY", value),
            None => std::env::remove_var("DISPLAY"),
        }
    }

    #[test]
    fn invalidating_drops_the_cache() {
        invalidate();
        assert!(!is_connected());
    }

    #[test]
    fn the_error_enum_explains_itself() {
        let no_session = X11Error::NoSession("DISPLAY is unset".to_string());
        assert!(no_session.to_string().contains("DISPLAY is unset"));
        let missing = X11Error::MissingExtension("XComposite".to_string());
        assert!(missing.to_string().contains("XComposite"));
    }
}