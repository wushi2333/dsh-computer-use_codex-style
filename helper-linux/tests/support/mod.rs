//! The real-desktop safety gate every gated Xvfb suite runs.
//!
//! A gated suite must not be able to damage the operator's session, and "we read the code and
//! it sets `XDG_RUNTIME_DIR`" is not a check -- it is a hypothesis that stays true only until
//! someone edits a fixture. This source, compiled next to each suite, asserts the property
//! from the outside instead: the environment a fixture presents must not point anything at
//! the desktop's accessibility socket, and that socket must still accept new connections when
//! the suite is done.
//!
//! # What it can and cannot prove
//!
//! `observe_env` compares the runtime directory the **test process** presents against the
//! desktop's. A service activated on a session bus inherits the *bus's* environment, not the
//! test's, so a fixture that isolated only its own process but started a session bus with the
//! desktop's `XDG_RUNTIME_DIR` would pass that check and still bind the desktop's socket.
//! That was the bug. It is impossible by construction now -- the `common` module builds the
//! private session bus itself, so the bus's environment *is* the private one -- and the check
//! that actually closes the hole is `socket_accepts_connections`, which fails if anything, by
//! any route, left the desktop's socket unusable.

#![cfg(target_os = "linux")]
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// A real-desktop safety gate over one gated suite.
///
/// Construct it at the top of a test, call `observe_env` once the fixture has installed its
/// environment, and `assert_intact` at the end.
pub struct DesktopSafety {
    label: &'static str,
    socket: Option<PathBuf>,
    violations: Vec<String>,
    /// Whether a violation is a broken test or a warning worth printing.
    fatal: bool,
}

impl DesktopSafety {
    /// Guard the desktop, treating damage as a **test failure**.
    ///
    /// Use this wherever the fixtures are meant to be hermetic -- which is the point of
    /// running them on Xvfb at all.
    pub fn fatal(label: &'static str) -> Self {
        Self::new(label, true)
    }

    /// Guard the desktop, treating damage as a warning.
    ///
    /// Use this in a test that is *supposed* to act on the live desktop, where the socket
    /// going away is the test doing its job rather than a defect.
    pub fn warning(label: &'static str) -> Self {
        Self::new(label, false)
    }

    fn new(label: &'static str, fatal: bool) -> Self {
        Self {
            label,
            socket: crate::common::desktop_a11y_socket(),
            violations: Vec::new(),
            fatal,
        }
    }

    /// The desktop socket this gate is guarding, when the run has one.
    pub fn socket(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    /// Record the environment the fixture presents to its children.
    ///
    /// Call it **after** the fixture finished setting its environment, so the values are the
    /// ones the helper will actually see.
    pub fn observe_env(&mut self) {
        let Some(socket) = self.socket.clone() else {
            return;
        };
        let pointer = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        let desktop_runtime = socket.parent().and_then(Path::parent).map(Path::to_path_buf);
        let isolated = match (pointer.as_deref(), desktop_runtime.as_deref()) {
            (Some(runtime), Some(desktop)) => runtime != desktop,
            // No XDG_RUNTIME_DIR is not isolation either: at-spi-bus-launcher falls back to
            // /run/user/<uid>, which is the desktop's own runtime directory.
            _ => false,
        };
        if !isolated {
            self.violations.push(format!(
                "XDG_RUNTIME_DIR={} leaves the desktop accessibility socket {} reachable from this fixture",
                pointer
                    .as_deref()
                    .map(|value| value.display().to_string())
                    .unwrap_or_else(|| "<unset>".to_string()),
                socket.display()
            ));
        }
    }

    /// Assert everything this gate has observed: panic when fatal, warn otherwise.
    pub fn assert_intact(&mut self) {
        if let Some(socket) = &self.socket {
            let present = socket.exists();
            if present && crate::common::socket_accepts_connections(socket) {
                // The desktop's AT-SPI survived this suite: this is the property that
                // matters, and the one whose absence used to be invisible.
            } else {
                self.violations.push(format!(
                    "the desktop accessibility socket {} is {} after this suite, so the desktop's \
                     AT-SPI is down for every new client (established connections keep working, \
                     which is why it goes unnoticed)",
                    socket.display(),
                    if present { "present but refusing connections" } else { "gone" }
                ));
            }
        }

        if self.violations.is_empty() {
            return;
        }
        let report = format!(
            "[desktop-safety:{}] the gated suite damaged the session it ran on:\n  - {}",
            self.label,
            self.violations.join("\n  - ")
        );
        if self.fatal {
            self.violations.clear();
            panic!("{report}");
        }
        eprintln!("WARNING {report}");
        self.violations.clear();
    }

    /// The observed violations, for a caller that wants to decide for itself.
    pub fn failures(&self) -> &[String] {
        &self.violations
    }
}
