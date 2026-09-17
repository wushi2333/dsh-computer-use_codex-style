//! Input injection through the XTest extension.
//!
//! Every window2 input verb takes **window-relative** coordinates, and XTest only
//! addresses the screen, so each call crosses one translation boundary
//! ([`crate::x11::window::to_root_coordinates`]). Clicks and scrolls also need the
//! pointer to actually be over the target, which is why the pointer is warped as part
//! of the same call rather than relying on wherever the user left it.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::ConnectionExt as _;
use xkeysym::Keysym;
use x11rb::protocol::xtest::ConnectionExt as _;

use super::connection::{with_connection, with_connection_flat};
use super::window;

/// Which pointer button a window2 click means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

impl MouseButton {
    /// Accept the window2 spellings, including the one-letter aliases.
    ///
    /// Matching is case-insensitive for the same reason the key-chord matcher is: the
    /// official parameter is a free-form string and "R" is the obvious way for a caller
    /// to spell the documented "r".
    pub fn parse(value: Option<&str>) -> Result<Self> {
        let lowered = value.map(|value| value.trim().to_ascii_lowercase());
        match lowered.as_deref() {
            None | Some("") | Some("left") | Some("l") => Ok(MouseButton::Left),
            Some("middle") | Some("m") => Ok(MouseButton::Middle),
            Some("right") | Some("r") => Ok(MouseButton::Right),
            Some(other) => bail!(
                "unsupported mouse button {other:?}: expected left, right, middle, l, r or m"
            ),
        }
    }

    /// The X button number, which is what XTest presses.
    pub fn code(self) -> u8 {
        match self {
            MouseButton::Left => 1,
            MouseButton::Middle => 2,
            MouseButton::Right => 3,
        }
    }
}

/// A key chord parsed into the keysyms that must be held and pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChord {
    pub modifiers: Vec<Keysym>,
    pub key: Keysym,
}

/// Common names the official matcher accepts that are not spelled like X keysyms.
///
/// The window2 contract says chords use "X Window System keysym-style names", and
/// lists `Control`, `Ctrl`, `Alt`, `Shift`, `period`, `greater` and `Numpad_0` as
/// accepted aliases. Matching is case-insensitive, as it is in the official helper.
fn canonical_key_name(name: &str) -> Option<Keysym> {
    let lowered = name.trim().to_ascii_lowercase();
    let direct = match lowered.as_str() {
        "control" | "ctrl" | "control_l" | "ctrl_l" => Some(Keysym::new(xkeysym::key::Control_L)),
        "control_r" | "ctrl_r" => Some(Keysym::new(xkeysym::key::Control_R)),
        "shift" | "shift_l" => Some(Keysym::new(xkeysym::key::Shift_L)),
        "shift_r" => Some(Keysym::new(xkeysym::key::Shift_R)),
        "alt" | "alt_l" | "option" | "option_l" => Some(Keysym::new(xkeysym::key::Alt_L)),
        "alt_r" | "option_r" => Some(Keysym::new(xkeysym::key::Alt_R)),
        "super" | "super_l" | "meta" | "meta_l" | "cmd" | "cmd_l" | "command"
        | "command_l" | "win" => Some(Keysym::new(xkeysym::key::Super_L)),
        "super_r" | "meta_r" | "cmd_r" | "win_r" => Some(Keysym::new(xkeysym::key::Super_R)),
        "enter" | "return" => Some(Keysym::new(xkeysym::key::Return)),
        "tab" => Some(Keysym::new(xkeysym::key::Tab)),
        "space" => Some(Keysym::new(xkeysym::key::space)),
        "esc" | "escape" => Some(Keysym::new(xkeysym::key::Escape)),
        "backspace" => Some(Keysym::new(xkeysym::key::BackSpace)),
        "delete" | "del" => Some(Keysym::new(xkeysym::key::Delete)),
        "insert" => Some(Keysym::new(xkeysym::key::Insert)),
        "home" => Some(Keysym::new(xkeysym::key::Home)),
        "end" => Some(Keysym::new(xkeysym::key::End)),
        "pageup" | "page_up" | "prior" => Some(Keysym::new(xkeysym::key::Prior)),
        "pagedown" | "page_down" | "next" => Some(Keysym::new(xkeysym::key::Next)),
        "up" => Some(Keysym::new(xkeysym::key::Up)),
        "down" => Some(Keysym::new(xkeysym::key::Down)),
        "left" => Some(Keysym::new(xkeysym::key::Left)),
        "right" => Some(Keysym::new(xkeysym::key::Right)),
        "period" | "dot" | "full_stop" => Some(Keysym::new(xkeysym::key::period)),
        "greater" => Some(Keysym::new(xkeysym::key::greater)),
        "comma" => Some(Keysym::new(xkeysym::key::comma)),
        "less" => Some(Keysym::new(xkeysym::key::less)),
        "slash" => Some(Keysym::new(xkeysym::key::slash)),
        "question" => Some(Keysym::new(xkeysym::key::question)),
        "minus" | "hyphen" => Some(Keysym::new(xkeysym::key::minus)),
        "underscore" => Some(Keysym::new(xkeysym::key::underscore)),
        "plus" => Some(Keysym::new(xkeysym::key::plus)),
        "equal" | "equals" => Some(Keysym::new(xkeysym::key::equal)),
        "semicolon" => Some(Keysym::new(xkeysym::key::semicolon)),
        "colon" => Some(Keysym::new(xkeysym::key::colon)),
        "apostrophe" | "quote" | "single_quote" => Some(Keysym::new(xkeysym::key::apostrophe)),
        "grave" | "backtick" => Some(Keysym::new(xkeysym::key::grave)),
        "tilde" => Some(Keysym::new(xkeysym::key::asciitilde)),
        "bracketleft" | "left_bracket" => Some(Keysym::new(xkeysym::key::bracketleft)),
        "bracketright" | "right_bracket" => Some(Keysym::new(xkeysym::key::bracketright)),
        "backslash" => Some(Keysym::new(xkeysym::key::backslash)),
        "bar" | "pipe" => Some(Keysym::new(xkeysym::key::bar)),
        _ => None,
    };
    if direct.is_some() {
        return direct;
    }
    if let Some(rest) = lowered
        .strip_prefix("kp_")
        .or_else(|| lowered.strip_prefix("numpad_"))
    {
        let keypad = match rest {
            "0" => Some(Keysym::new(xkeysym::key::KP_0)),
            "1" => Some(Keysym::new(xkeysym::key::KP_1)),
            "2" => Some(Keysym::new(xkeysym::key::KP_2)),
            "3" => Some(Keysym::new(xkeysym::key::KP_3)),
            "4" => Some(Keysym::new(xkeysym::key::KP_4)),
            "5" => Some(Keysym::new(xkeysym::key::KP_5)),
            "6" => Some(Keysym::new(xkeysym::key::KP_6)),
            "7" => Some(Keysym::new(xkeysym::key::KP_7)),
            "8" => Some(Keysym::new(xkeysym::key::KP_8)),
            "9" => Some(Keysym::new(xkeysym::key::KP_9)),
            "enter" => Some(Keysym::new(xkeysym::key::KP_Enter)),
            "add" | "plus" => Some(Keysym::new(xkeysym::key::KP_Add)),
            "subtract" | "minus" => Some(Keysym::new(xkeysym::key::KP_Subtract)),
            "multiply" | "star" => Some(Keysym::new(xkeysym::key::KP_Multiply)),
            "divide" | "slash" => Some(Keysym::new(xkeysym::key::KP_Divide)),
            "decimal" | "period" | "dot" => Some(Keysym::new(xkeysym::key::KP_Decimal)),
            _ => None,
        };
        if keypad.is_some() {
            return keypad;
        }
    }
    // A bare letter or digit is its own keysym.
    let characters: Vec<char> = lowered.chars().collect();
    if characters.len() == 1 && characters[0].is_ascii_alphanumeric() {
        return Some(Keysym::from_char(characters[0]));
    }
    // Finally accept a function-key name such as F5.
    keysym_from_name(name.trim())
}

/// Resolve the few names that are not spelled like keysyms by construction.
fn keysym_from_name(name: &str) -> Option<Keysym> {
    let upper = name.to_ascii_uppercase();
    let rest = upper.strip_prefix('F')?;
    let number: u32 = rest.parse().ok()?;
    if (1..=35).contains(&number) {
        return Some(Keysym::new(xkeysym::key::F1 + number - 1));
    }
    None
}

/// Parse a `+`-separated chord such as `Control_L+Shift_L+period`.
pub fn parse_chord(chord: &str) -> Result<KeyChord> {
    let parts: Vec<&str> = chord
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        bail!("key is required");
    }
    let mut keysyms = Vec::with_capacity(parts.len());
    for part in parts {
        let keysym = canonical_key_name(part)
            .ok_or_else(|| anyhow!("unsupported key {part:?}"))?;
        keysyms.push(keysym);
    }
    let key = keysyms.pop().expect("at least one keysym");
    Ok(KeyChord {
        modifiers: keysyms,
        key,
    })
}

/// The keycode the current keymap assigns to a keysym.
fn keycode_for(raw: &x11rb::rust_connection::RustConnection, keysym: Keysym) -> Result<u8> {
    let setup = raw.setup();
    let min = setup.min_keycode;
    let count = setup.max_keycode - min + 1;
    let reply = raw
        .get_keyboard_mapping(min, count)
        .map_err(|error| anyhow!("get_keyboard_mapping could not be sent: {error}"))?
        .reply()
        .map_err(|error| anyhow!("get_keyboard_mapping was refused: {error:?}"))?;
    let per_keycode = usize::from(reply.keysyms_per_keycode.max(1));
    for (index, chunk) in reply.keysyms.chunks(per_keycode).enumerate() {
        if chunk.iter().any(|candidate| *candidate == keysym.raw()) {
            return Ok(min + index as u8);
        }
    }
    bail!(
        "the current keymap has no keycode for keysym {:#x}; the chord cannot be typed on this layout",
        keysym.raw()
    )
}

/// Warp the pointer to a root-space point with a synthetic motion event.
pub fn move_pointer(root_x: i16, root_y: i16) -> Result<()> {
    with_connection(|connection| {
        let raw = connection.inner();
        synthetic_motion(raw, root_x, root_y)?;
        raw.flush()
            .map_err(|error| anyhow!("flush after pointer motion failed: {error}"))?;
        Ok(())
    })?
}

fn synthetic_motion(
    raw: &x11rb::rust_connection::RustConnection,
    root_x: i16,
    root_y: i16,
) -> Result<()> {
    // The root window is the destination so the server routes the motion to whatever
    // is actually under the pointer, exactly as a real move would.
    let root = raw.setup().roots[0].root;
    raw.xtest_fake_input(6, 0, 0, root, root_x, root_y, 0)
        .map_err(|error| anyhow!("xtest_fake_input(motion) could not be sent: {error}"))?
        .check()
        .map_err(|error| anyhow!("xtest_fake_input(motion) was refused: {error:?}"))?;
    Ok(())
}

/// Click at a window-relative point.
pub fn click(id: u64, x: i32, y: i32, button: MouseButton, count: u32) -> Result<String> {
    if count == 0 {
        bail!("click_count must be at least 1");
    }
    let (root_x, root_y) = clamp_to_screen(window::to_root_coordinates(id, x, y)?)?;
    with_connection(|connection| {
        let raw = connection.inner();
        let root = connection.root();
        synthetic_motion(raw, root_x, root_y)?;
        raw.flush()
            .map_err(|error| anyhow!("flush after pointer motion failed: {error}"))?;
        for _ in 0..count {
            press_button(raw, root, button.code(), root_x, root_y)?;
            release_button(raw, root, button.code(), root_x, root_y)?;
        }
        raw.flush()
            .map_err(|error| anyhow!("flush after click failed: {error}"))?;
        Ok(format!(
            "clicked {:?} button {count} time(s) at root ({root_x}, {root_y})",
            button
        ))
    })?
}

fn press_button(
    raw: &x11rb::rust_connection::RustConnection,
    root: u32,
    code: u8,
    x: i16,
    y: i16,
) -> Result<()> {
    raw.xtest_fake_input(4, code, 0, root, x, y, 0)
        .map_err(|error| anyhow!("xtest_fake_input(button press) could not be sent: {error}"))?
        .check()
        .map_err(|error| anyhow!("xtest_fake_input(button press) was refused: {error:?}"))?;
    Ok(())
}

fn release_button(
    raw: &x11rb::rust_connection::RustConnection,
    root: u32,
    code: u8,
    x: i16,
    y: i16,
) -> Result<()> {
    raw.xtest_fake_input(5, code, 0, root, x, y, 0)
        .map_err(|error| anyhow!("xtest_fake_input(button release) could not be sent: {error}"))?
        .check()
        .map_err(|error| anyhow!("xtest_fake_input(button release) was refused: {error:?}"))?;
    Ok(())
}

/// Scroll at a window-relative point.
///
/// A wheel notch is button 4/5 (vertical) and 6/7 (horizontal) on every X server;
/// XTest has no separate scroll request, so notches are emitted as press/release
/// pairs. A fractional delta is rounded toward the nearest whole notch and never to
/// zero, so a caller asking to scroll a little still gets a visible scroll.
pub fn scroll(id: u64, x: i32, y: i32, scroll_x: i32, scroll_y: i32) -> Result<String> {
    let (root_x, root_y) = clamp_to_screen(window::to_root_coordinates(id, x, y)?)?;
    let vertical = notch_count(scroll_y);
    let horizontal = notch_count(scroll_x);
    check_notch_budget(vertical, horizontal)?;
    let vertical_button = if scroll_y < 0 { 4 } else { 5 };
    let horizontal_button = if scroll_x < 0 { 6 } else { 7 };
    with_connection(|connection| {
        let raw = connection.inner();
        let root = connection.root();
        synthetic_motion(raw, root_x, root_y)?;
        for _ in 0..vertical {
            press_button(raw, root, vertical_button, root_x, root_y)?;
            release_button(raw, root, vertical_button, root_x, root_y)?;
        }
        for _ in 0..horizontal {
            press_button(raw, root, horizontal_button, root_x, root_y)?;
            release_button(raw, root, horizontal_button, root_x, root_y)?;
        }
        raw.flush()
            .map_err(|error| anyhow!("flush after scroll failed: {error}"))?;
        Ok(format!(
            "scrolled {vertical} vertical and {horizontal} horizontal notch(es) at root ({root_x}, {root_y})"
        ))
    })?
}

/// How many wheel notches one call may emit, in total across both axes.
///
/// Each notch is two `xtest_fake_input` requests, and the helper serves one JSONL request
/// at a time, so this is also a bound on how long one call can monopolise the session: 100
/// notches is a couple of seconds, while the i32::MIN a caller can send would ask for 21
/// million of them and block every later call -- including `interrupt` -- for minutes.
const MAX_SCROLL_NOTCHES: u32 = 100;

/// The same ceiling in the units the caller sends (`scrollX`/`scrollY`).
const MAX_SCROLL_NOTCH_UNITS: u32 = MAX_SCROLL_NOTCHES * 100;


/// Refuse a scroll whose notch count would monopolise the helper.
///
/// Split out from [`scroll`] so the ceiling itself is testable without an X server -- a
/// test that only asserts the constant would keep passing if this check were dropped.
fn check_notch_budget(vertical: u32, horizontal: u32) -> Result<()> {
    let total = vertical + horizontal;
    if total > MAX_SCROLL_NOTCHES {
        bail!(
            "scrollX/scrollY ask for {total} wheel notch(es); the ceiling is {MAX_SCROLL_NOTCHES} per \
             call. One notch is 100 units, so {MAX_SCROLL_NOTCHES} is {MAX_SCROLL_NOTCH_UNITS} \
             units. Each notch is two X requests, and this helper answers one request at a \
             time, so a larger value would block every other call for minutes. Scroll in \
             several calls instead."
        );
    }
    Ok(())
}

/// Wheel notches for one axis delta.
///
/// One notch is 100 units, and a delta is truncated rather than rounded: 150 units is one
/// notch, not two. The floor of one keeps a small-but-nonzero delta visible, which is what
/// a caller asking to "scroll a little" means.
fn notch_count(delta: i32) -> u32 {
    if delta == 0 {
        return 0;
    }
    (delta.unsigned_abs() / 100).max(1)
}

fn clamp_to_screen(point: (i32, i32)) -> Result<(i16, i16)> {
    let (width, height) = with_connection(|connection| connection.screen_size())?;
    let x = point.0.clamp(0, i32::from(width.saturating_sub(1)));
    let y = point.1.clamp(0, i32::from(height.saturating_sub(1)));
    Ok((x as i16, y as i16))
}

/// Press a chord in the target window.
pub fn press_key(id: u64, chord: &str) -> Result<String> {
    let parsed = parse_chord(chord)?;
    // Focusing the target first is what makes "press a key in this window" mean this
    // window rather than whatever the user last clicked on.
    let activation = window::activate_window(id)?;
    let (modifiers, key) = with_connection_flat(|connection| {
        let raw = connection.inner();
        let modifiers: Vec<u8> = parsed
            .modifiers
            .iter()
            .map(|keysym| keycode_for(raw, *keysym))
            .collect::<Result<Vec<u8>>>()?;
        Ok((modifiers, keycode_for(raw, parsed.key)?))
    })?;
    with_connection(|connection| {
        let raw = connection.inner();
        let held = with_modifiers_held(
            &modifiers,
            &|code| press_key_code(raw, code),
            &|| tap(raw, key, None),
            &|code| release_key(raw, code),
        );
        // The flush is not part of the hold/release bookkeeping: a request that cannot be
        // queued is reported, but it is not a reason to leave a modifier down.
        let flushed = raw
            .flush()
            .map_err(|error| anyhow!("flush after key press failed: {error}"));
        held?;
        flushed?;
        Ok(format!(
            "pressed {chord} ({} modifier(s)) with {activation}",
            modifiers.len()
        ))
    })?
}

/// Hold every modifier down, tap the key, and release what was pressed -- on every path.
///
/// Split out from [`press_key`] so the property that matters can be tested without an X
/// server: a modifier that was pressed is released even when the tap, or a later press,
/// failed. XTest modifiers are server-global state that no later request resets, so one
/// stranded Control_L turns every subsequent keystroke -- the operator's included -- into
/// a chord. The releases are therefore attempted unconditionally, in reverse order, and
/// the *first* failure is what the caller is told about.
fn with_modifiers_held(
    modifiers: &[u8],
    press: &dyn Fn(u8) -> Result<()>,
    tap: &dyn Fn() -> Result<()>,
    release: &dyn Fn(u8) -> Result<()>,
) -> Result<()> {
    let mut held: Vec<u8> = Vec::with_capacity(modifiers.len());
    let mut failure: Option<anyhow::Error> = None;
    for code in modifiers {
        match press(*code) {
            Ok(()) => held.push(*code),
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    if failure.is_none() {
        failure = tap().err();
    }
    for code in held.iter().rev() {
        if let Err(error) = release(*code) {
            if failure.is_none() {
                failure = Some(error);
            }
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Type text into the focused element of a window.
///
/// A keysym exists for every printable character, so typing does not depend on the
/// caller's keyboard layout. A character with no keysym on this layout is reported
/// rather than silently dropped or replaced with a wrong key.
pub fn type_text(id: u64, text: &str) -> Result<String> {
    if text.is_empty() {
        bail!("text must not be empty");
    }
    let activation = window::activate_window(id)?;
    let typed: usize = with_connection_flat(|connection| {
        let raw = connection.inner();
        let shift = keycode_for(raw, Keysym::new(xkeysym::key::Shift_L))?;
        let mut typed = 0usize;
        for character in text.chars() {
            match character {
                '\n' => {
                    tap(raw, keycode_for(raw, Keysym::new(xkeysym::key::Return))?, None)?;
                }
                '\t' => {
                    tap(raw, keycode_for(raw, Keysym::new(xkeysym::key::Tab))?, None)?;
                }
                other => {
                    let keysym = Keysym::from_char(other);
                    let code = keycode_for(raw, keysym)?;
                    let shifted = requires_shift(raw, code, keysym)?;
                    tap(raw, code, if shifted { Some(shift) } else { None })?;
                }
            }
            typed += 1;
        }
        raw.flush()
            .map_err(|error| anyhow!("flush after typing failed: {error}"))?;
        Ok(typed)
    })?;
    Ok(format!(
        "typed {typed} character(s) into the focused element with {activation}"
    ))
}

/// Whether the shifted level of a keycode is what produces this keysym.
///
/// Target keysym requires Shift only when it appears at level >= 1 and is not
/// directly available at level 0 (unshifted).
fn requires_shift(
    raw: &x11rb::rust_connection::RustConnection,
    keycode: u8,
    target: Keysym,
) -> Result<bool> {
    let reply = raw
        .get_keyboard_mapping(keycode, 1)
        .map_err(|error| anyhow!("get_keyboard_mapping could not be sent: {error}"))?
        .reply()
        .map_err(|error| anyhow!("get_keyboard_mapping was refused: {error:?}"))?;
    Ok(keysym_requires_shift(&reply.keysyms, target))
}

fn keysym_requires_shift(levels: &[u32], target: Keysym) -> bool {
    if levels.first() == Some(&target.raw()) {
        return false;
    }
    levels.get(1..).map_or(false, |rest| rest.contains(&target.raw()))
}

fn tap(
    raw: &x11rb::rust_connection::RustConnection,
    keycode: u8,
    modifier: Option<u8>,
) -> Result<()> {
    if let Some(modifier) = modifier {
        press_key_code(raw, modifier)?;
    }
    // The same rule as `press_key`: once the modifier is down, every later step runs,
    // and the first failure is reported after the release has been attempted.
    let pressed = press_key_code(raw, keycode);
    let released = release_key(raw, keycode);
    let modifier_released = match modifier {
        Some(keycode) => release_key(raw, keycode),
        None => Ok(()),
    };
    pressed?;
    released?;
    modifier_released?;
    Ok(())
}


fn press_key_code(raw: &x11rb::rust_connection::RustConnection, keycode: u8) -> Result<()> {
    raw.xtest_fake_input(2, keycode, 0, 0, 0, 0, 0)
        .map_err(|error| anyhow!("xtest_fake_input(key press) could not be sent: {error}"))?
        .check()
        .map_err(|error| anyhow!("xtest_fake_input(key press) was refused: {error:?}"))?;
    Ok(())
}

fn release_key(raw: &x11rb::rust_connection::RustConnection, keycode: u8) -> Result<()> {
    raw.xtest_fake_input(3, keycode, 0, 0, 0, 0, 0)
        .map_err(|error| anyhow!("xtest_fake_input(key release) could not be sent: {error}"))?
        .check()
        .map_err(|error| anyhow!("xtest_fake_input(key release) was refused: {error:?}"))?;
    Ok(())
}

/// Drag from one window-relative point to another.
pub fn drag(id: u64, from_x: i32, from_y: i32, to_x: i32, to_y: i32) -> Result<String> {
    let (from_root_x, from_root_y) = clamp_to_screen(window::to_root_coordinates(id, from_x, from_y)?)?;
    let (to_root_x, to_root_y) = clamp_to_screen(window::to_root_coordinates(id, to_x, to_y)?)?;
    with_connection(|connection| {
        let raw = connection.inner();
        let root = connection.root();
        synthetic_motion(raw, from_root_x, from_root_y)?;
        press_button(raw, root, 1, from_root_x, from_root_y)?;
        raw.flush()
            .map_err(|error| anyhow!("flush after drag press failed: {error}"))?;
        // Move in steps: a single jump is ignored by toolkits that start a drag only
        // after they see motion, and by anything watching for velocity.
        const STEPS: i32 = 12;
        for step in 1..=STEPS {
            let x = i32::from(from_root_x) + (i32::from(to_root_x) - i32::from(from_root_x)) * step / STEPS;
            let y = i32::from(from_root_y) + (i32::from(to_root_y) - i32::from(from_root_y)) * step / STEPS;
            synthetic_motion(raw, x as i16, y as i16)?;
            raw.flush()
                .map_err(|error| anyhow!("flush during drag failed: {error}"))?;
            std::thread::sleep(Duration::from_millis(10));
        }
        release_button(raw, root, 1, to_root_x, to_root_y)?;
        raw.flush()
            .map_err(|error| anyhow!("flush after drag release failed: {error}"))?;
        Ok(format!(
            "dragged from root ({from_root_x}, {from_root_y}) to ({to_root_x}, {to_root_y})"
        ))
    })?
}

/// Whether XTest is usable, for health.
pub fn probe() -> Result<(u8, u16)> {
    with_connection(|connection| {
        let reply = connection
            .inner()
            .xtest_get_version(2, 2)
            .map_err(|error| anyhow!("xtest_get_version could not be sent: {error}"))?
            .reply()
            .map_err(|error| anyhow!("xtest_get_version was refused: {error:?}"))?;
        Ok((reply.major_version, reply.minor_version))
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression 30f35dd fixed on the success path, asserted on the *failure* path:
    /// a modifier that went down is released even when the tap never succeeded.
    #[test]
    fn a_failed_tap_still_releases_every_modifier_that_went_down() {
        use std::cell::RefCell;
        let events: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let failure = with_modifiers_held(
            &[37u8, 50u8],
            &|code| {
                events.borrow_mut().push(format!("press {code}"));
                Ok(())
            },
            &|| {
                events.borrow_mut().push("tap".to_string());
                Err(anyhow!("the tap could not be sent"))
            },
            &|code| {
                events.borrow_mut().push(format!("release {code}"));
                Ok(())
            },
        );
        assert!(failure.is_err(), "the tap failure must reach the caller");
        assert_eq!(
            events.into_inner(),
            vec![
                "press 37",
                "press 50",
                "tap",
                // Reverse order, and both of them: this is the assertion that fails if the
                // releases go back behind a `?`.
                "release 50",
                "release 37",
            ]
        );
    }

    #[test]
    fn a_modifier_that_failed_to_press_is_not_released_and_the_report_is_the_first_error() {
        use std::cell::RefCell;
        let events: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let failure = with_modifiers_held(
            &[37u8, 50u8],
            &|code| {
                events.borrow_mut().push(format!("press {code}"));
                if code == 50 {
                    return Err(anyhow!("the second press was refused"));
                }
                Ok(())
            },
            &|| {
                events.borrow_mut().push("tap".to_string());
                Ok(())
            },
            &|code| {
                events.borrow_mut().push(format!("release {code}"));
                Err(anyhow!("the release could not be sent"))
            },
        );
        let message = failure.unwrap_err().to_string();
        assert!(message.contains("second press"), "{message}");
        let events = events.into_inner();
        // Only what actually went down is released, and the tap never ran.
        assert_eq!(events, vec!["press 37", "press 50", "release 37"]);
    }

    #[test]
    fn a_successful_chord_presses_in_order_and_releases_in_reverse() {
        use std::cell::RefCell;
        let events: RefCell<Vec<String>> = RefCell::new(Vec::new());
        with_modifiers_held(
            &[37u8, 50u8],
            &|code| {
                events.borrow_mut().push(format!("press {code}"));
                Ok(())
            },
            &|| {
                events.borrow_mut().push("tap".to_string());
                Ok(())
            },
            &|code| {
                events.borrow_mut().push(format!("release {code}"));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            events.into_inner(),
            vec!["press 37", "press 50", "tap", "release 50", "release 37"]
        );
    }

    /// The ceiling exists because each notch is two X requests and the helper answers one
    /// request at a time: without it, `scrollY: -2147483648` means 21,474,836 notches and
    /// every later call -- `interrupt` included -- waits behind them.
    #[test]
    fn the_worst_case_scroll_delta_is_a_refusal_not_a_twenty_million_notch_run() {
        assert_eq!(notch_count(i32::MIN), 21_474_836);
        assert!(
            notch_count(i32::MIN) > MAX_SCROLL_NOTCHES,
            "the extreme the guard exists for must exceed the ceiling"
        );
        // The guard itself, not just the constant: this is the assertion that fails if
        // the ceiling stops being applied.
        let error = check_notch_budget(notch_count(i32::MIN), 0).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("21,474,836") || message.contains("21474836"), "{message}");
        assert!(message.contains("ceiling is 100"), "{message}");
        assert!(message.contains("Scroll in several calls"), "{message}");
        // The ceiling is per call across both axes, so two legal axes can still exceed it.
        assert!(check_notch_budget(100, 0).is_ok());
        assert!(check_notch_budget(60, 60).is_err());
        assert!(check_notch_budget(0, 0).is_ok());
    }

    #[test]
    fn a_delta_inside_the_ceiling_still_scrolls() {
        assert_eq!(notch_count(0), 0);
        // Truncated, not rounded: 150 units is one notch. The floor of one keeps a small
        // delta visible, which is what "scroll a little" means.
        assert_eq!(notch_count(150), 1);
        assert_eq!(notch_count(-150), 1);
        assert_eq!(notch_count(1), 1);
        assert_eq!(notch_count(-100), 1);
        assert_eq!(notch_count(10_000), 100);
        assert_eq!(notch_count(-10_000), 100);
        assert!(notch_count(10_000) + notch_count(0) <= MAX_SCROLL_NOTCHES);
    }

    #[test]
    fn a_bare_letter_parses_as_its_own_chord() {
        let chord = parse_chord("a").unwrap();
        assert!(chord.modifiers.is_empty());
        assert_eq!(chord.key, Keysym::from_char('a'));
    }

    #[test]
    fn the_documented_aliases_all_resolve() {
        for alias in [
            "Control",
            "Ctrl",
            "Alt",
            "Shift",
            "period",
            "greater",
            "Numpad_0",
            "space",
            "Return",
            "Tab",
            "Control_L",
            "Shift_L",
            "KP_0",
            "F5",
            "Escape",
        ] {
            assert!(
                canonical_key_name(alias).is_some(),
                "the official contract accepts {alias}"
            );
        }
    }

    #[test]
    fn a_modifier_chord_keeps_the_modifiers_and_the_final_key() {
        let chord = parse_chord("Control_L+Shift_L+period").unwrap();
        assert_eq!(chord.modifiers.len(), 2);
        assert_eq!(chord.modifiers[0], Keysym::new(xkeysym::key::Control_L));
        assert_eq!(chord.modifiers[1], Keysym::new(xkeysym::key::Shift_L));
        assert_eq!(chord.key, Keysym::new(xkeysym::key::period));
    }

    #[test]
    fn whitespace_around_a_chord_is_ignored() {
        let chord = parse_chord(" Control_L + a ").unwrap();
        assert_eq!(chord.modifiers, vec![Keysym::new(xkeysym::key::Control_L)]);
        assert_eq!(chord.key, Keysym::from_char('a'));
    }

    #[test]
    fn an_empty_chord_is_refused() {
        assert!(parse_chord("   ").is_err());
    }

    #[test]
    fn an_unknown_key_is_refused_by_name() {
        let error = parse_chord("NotAKey").unwrap_err();
        assert!(error.to_string().contains("NotAKey"));
    }

    #[test]
    fn mouse_buttons_accept_the_window2_spellings() {
        assert_eq!(MouseButton::parse(None).unwrap(), MouseButton::Left);
        assert_eq!(MouseButton::parse(Some("l")).unwrap(), MouseButton::Left);
        assert_eq!(MouseButton::parse(Some("R")).unwrap(), MouseButton::Right);
        assert_eq!(MouseButton::parse(Some("middle")).unwrap(), MouseButton::Middle);
        assert_eq!(MouseButton::Left.code(), 1);
        assert_eq!(MouseButton::Middle.code(), 2);
        assert_eq!(MouseButton::Right.code(), 3);
        assert!(MouseButton::parse(Some("thumb")).is_err());
    }

    #[test]
    fn scroll_deltas_become_whole_notches_and_never_vanish() {
        assert_eq!(notch_count(0), 0);
        assert_eq!(notch_count(100), 1);
        assert_eq!(notch_count(-250), 2);
        // A small request still scrolls, otherwise "scroll a little" would do nothing.
        assert_eq!(notch_count(1), 1);
        assert_eq!(notch_count(-1), 1);
    }

    #[test]
    fn function_keys_resolve_beyond_the_named_table() {
        assert_eq!(keysym_from_name("F5"), Some(Keysym::new(xkeysym::key::F1 + 4)));
        assert_eq!(keysym_from_name("F36"), None);
        assert_eq!(keysym_from_name("nonsense"), None);
    }

    #[test]
    fn keysym_requires_shift_only_when_not_at_level_0() {
        let sym_a = Keysym::from_char('a');
        let sym_cap_a = Keysym::from_char('A');
        let sym_1 = Keysym::from_char('1');
        let sym_exclam = Keysym::from_char('!');
        let sym_minus = Keysym::from_char('-');
        let sym_underscore = Keysym::from_char('_');

        // [a, A, a, A]
        let key_a = [sym_a.raw(), sym_cap_a.raw(), sym_a.raw(), sym_cap_a.raw()];
        assert!(!keysym_requires_shift(&key_a, sym_a));
        assert!(keysym_requires_shift(&key_a, sym_cap_a));

        // [1, !, 1, !]
        let key_1 = [sym_1.raw(), sym_exclam.raw(), sym_1.raw(), sym_exclam.raw()];
        assert!(!keysym_requires_shift(&key_1, sym_1));
        assert!(keysym_requires_shift(&key_1, sym_exclam));

        // [-, _, -, _]
        let key_minus = [sym_minus.raw(), sym_underscore.raw(), sym_minus.raw(), sym_underscore.raw()];
        assert!(!keysym_requires_shift(&key_minus, sym_minus));
        assert!(keysym_requires_shift(&key_minus, sym_underscore));

        // Key with same keysym at level 0 and 1
        let key_same = [sym_a.raw(), sym_a.raw()];
        assert!(!keysym_requires_shift(&key_same, sym_a));

        // Empty levels or keysym not on this key
        assert!(!keysym_requires_shift(&[], sym_a));
        assert!(!keysym_requires_shift(&key_a, sym_1));
    }
}
