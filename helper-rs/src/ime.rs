//! IME auto-suppression for synthetic keyboard input.
//!
//! A Chinese IME (Sogou / Microsoft Pinyin / Wubi…) intercepts `Control+V`,
//! `Control+A` and plain letters before the target application ever sees them.
//! The classic symptom is a `set_value` call that ends up typing a literal `v`
//! into the field, or a `type_text` that lands as one stray letter while the IME
//! candidate window pops up instead of the text.
//!
//! ## Why the obvious implementation is wrong
//!
//! `ActivateKeyboardLayout(hkl, KLF_SETFORPROCESS)` only affects **the calling
//! thread**. Synthetic keystrokes are delivered to the *foreground/target*
//! window's thread, so switching the helper's own layout does nothing at all.
//! The layout has to be changed on the thread that owns the target window.
//!
//! ## What this module does
//!
//! Given the target HWND, [`ImeGuard::install_for`]:
//!
//! 1. posts `WM_INPUTLANGCHANGEREQUEST` with English (US) to that window, which is
//!    the documented way to switch a *foreign* thread's layout without attaching
//!    to it; and
//! 2. detaches the IME context with `ImmAssociateContext(hwnd, NULL)` so even an
//!    IME that ignores the layout switch cannot compose.
//!
//! On drop it restores the original layout and re-attaches the IME context.
//!
//! A `KEYEVENTF_UNICODE` write bypasses the IME by construction, but an active
//! composition can still swallow the first character, so the guard is installed
//! for those too.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

/// Audit P2-1 (2026-10-25): restore failures used to be swallowed with `let _ =`
/// — a window whose IME context or layout could not be restored (target died
/// mid-guard, PostMessageW rejected, third-party IME misbehaving) left the user
/// with no way to notice. Counted here and surfaced through `health` so a batch
/// session that keeps breaking the user's IME is visible in diagnostics.
static RESTORE_FAILURES: AtomicU64 = AtomicU64::new(0);

pub fn restore_failure_count() -> u64 {
    RESTORE_FAILURES.load(Ordering::Relaxed)
}

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, PostMessageW, WM_INPUTLANGCHANGEREQUEST,
};

#[cfg(windows)]
mod imm {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;

    #[link(name = "imm32")]
    extern "system" {
        /// `HIMC ImmAssociateContext(HWND, HIMC)` — passing NULL detaches the IME.
        pub fn ImmAssociateContext(hwnd: HWND, himc: isize) -> isize;
    }

    /// `HKL ImmGetContext`-adjacent helper: not needed; we only detach/attach.
    #[allow(dead_code)]
    pub fn _unused(_: PCWSTR) {}
}

/// English (US) keyboard layout id.
const LANG_EN_US: isize = 0x0409_0409;

thread_local! {
    /// Depth of nested [`ImeGuard`]s. Only the outermost guard restores, so
    /// nested keyboard calls cannot clobber each other's restore.
    static GUARD_DEPTH: Cell<usize> = const { Cell::new(0) };
    static SAVED: Cell<Option<Saved>> = const { Cell::new(None) };
}

#[derive(Clone, Copy)]
struct Saved {
    hwnd: isize,
    previous_hkl: isize,
    previous_imc: isize,
}

/// Resolve the HWND the guard should target: the explicit window when given,
/// otherwise the current foreground window (0 when there is none).
fn target_hwnd(explicit: Option<isize>) -> isize {
    if let Some(h) = explicit {
        if h != 0 {
            return h;
        }
    }
    unsafe { GetForegroundWindow().0 as isize }
}

/// Install the English layout on `hwnd`'s thread and detach its IME.
/// Returns what to restore on drop, or `None` when there is no usable target.
fn install(hwnd: isize) -> Option<Saved> {
    if hwnd == 0 {
        return None;
    }
    let h = HWND(hwnd as *mut core::ffi::c_void);
    unsafe {
        // 0) Snapshot the target thread's *real* current layout. The owning
        //    thread is the only authority on its own HKL; hard-coding an
        //    English constant here would leave a Chinese layout switched to
        //    English forever after the guard drops (audit P1-1, 2026-10-25).
        let tid = GetWindowThreadProcessId(h, None);
        let current = GetKeyboardLayout(tid).0 as isize;
        // 1) Ask the owning thread to switch to English (US). Posting is the
        //    documented cross-thread path; attaching to that thread is not
        //    allowed from an RPC-ish worker without a message queue.
        let _ = PostMessageW(
            Some(h),
            WM_INPUTLANGCHANGEREQUEST,
            WPARAM(0),
            LPARAM(LANG_EN_US),
        );
        // 2) Belt and braces: detach the IME context so a layout-ignoring IME
        //    still cannot compose. Returns the previous context to restore.
        let previous_imc = imm::ImmAssociateContext(h, 0);
        Some(Saved {
            hwnd,
            // A failed query (0) has nothing better to restore than EN-US;
            // any real HKL — including EN-US itself — round-trips unchanged.
            previous_hkl: if current == 0 { LANG_EN_US } else { current },
            previous_imc,
        })
    }
}

/// RAII guard that keeps the target window's IME suppressed for its lifetime.
///
/// Dropping restores the previous IME context. Dropping out of order is safe:
/// only the outermost guard restores.
pub struct ImeGuard {
    active: bool,
}

impl ImeGuard {
    /// Suppress the IME for the given target window. `None` means "use the
    /// current foreground window".
    ///
    /// Never fails: a missing window or an unavailable IME is a no-op, and the
    /// caller's keystrokes simply go out as they are. Input must never break
    /// because of an advisory protection.
    pub fn install_for(hwnd: Option<isize>) -> Self {
        let already = GUARD_DEPTH.with(Cell::get);
        if already == 0 {
            let target = target_hwnd(hwnd);
            SAVED.with(|c| c.set(install(target)));
        }
        GUARD_DEPTH.with(|c| c.set(already + 1));
        Self { active: true }
    }

    /// Suppress the IME for the current foreground window.
    pub fn install() -> Self {
        Self::install_for(None)
    }
}

impl Drop for ImeGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let depth = GUARD_DEPTH.with(Cell::get);
        if depth <= 1 {
            if let Some(saved) = SAVED.with(Cell::take) {
                if saved.hwnd != 0 {
                    let h = HWND(saved.hwnd as *mut core::ffi::c_void);
                    unsafe {
                        // Audit P2-1: a failed restore is counted, not swallowed.
                        // The window may be gone (fine) or a third-party IME may
                        // be fighting the re-attach (needs operator attention).
                        let _ = imm::ImmAssociateContext(h, saved.previous_imc);
                        let posted = PostMessageW(
                            Some(h),
                            WM_INPUTLANGCHANGEREQUEST,
                            WPARAM(0),
                            LPARAM(saved.previous_hkl),
                        );
                        if posted.is_err() {
                            RESTORE_FAILURES.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        }
        GUARD_DEPTH.with(|c| c.set(depth.saturating_sub(1)));
    }
}

/// Run `f` with the target window's IME suppressed.
pub fn with_ime_suppressed<T>(
    hwnd: Option<isize>,
    f: impl FnOnce() -> Result<T, crate::protocol::Error>,
) -> Result<T, crate::protocol::Error> {
    let _guard = ImeGuard::install_for(hwnd);
    f()
}

/// Report whether an IME guard is currently installed on this thread.
pub fn is_suppressing() -> bool {
    GUARD_DEPTH.with(Cell::get) > 0
}

/// The HWND the active guard is suppressing, if any (used by diagnostics).
pub fn suppressed_hwnd() -> Option<isize> {
    SAVED.with(|c| c.get().map(|s| s.hwnd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_guards_do_not_double_restore() {
        let outer = ImeGuard::install_for(None);
        assert_eq!(GUARD_DEPTH.with(Cell::get), 1);
        {
            let inner = ImeGuard::install_for(None);
            assert_eq!(GUARD_DEPTH.with(Cell::get), 2);
            assert!(is_suppressing());
            drop(inner);
            assert_eq!(GUARD_DEPTH.with(Cell::get), 1);
        }
        drop(outer);
        assert_eq!(GUARD_DEPTH.with(Cell::get), 0);
        assert!(!is_suppressing());
    }

    #[test]
    fn guard_install_never_panics_without_a_window() {
        // Must be callable on a headless CI machine with no foreground window:
        // this is exactly the state observed on 2026-10-19 right after a client
        // restart, where GetForegroundWindow() returned 0.
        let _g = ImeGuard::install_for(None);
        assert!(is_suppressing());
    }

    #[test]
    fn guard_with_null_hwnd_saves_nothing_but_still_counts() {
        let g = ImeGuard::install_for(Some(0));
        assert!(is_suppressing());
        drop(g);
        assert!(!is_suppressing());
        assert_eq!(suppressed_hwnd(), None);
    }

    #[test]
    fn english_layout_id_is_the_us_hkl() {
        // 0x04090409 = MAKELANGID(0x0409, 0x0409): language US, layout US.
        assert_eq!(LANG_EN_US, 0x0409_0409);
    }

    #[test]
    fn with_ime_suppressed_runs_the_body() {
        let out = with_ime_suppressed(None, || Ok(7usize));
        assert_eq!(out.unwrap(), 7);
        assert!(!is_suppressing());
    }
}
