//! EWMH/ICCCM window enumeration and the window handle model.
//!
//! The handle is the X window id itself (an opaque 32-bit XID widened to `u64`). It is
//! stable for exactly as long as the window exists, which is the same guarantee the
//! Windows helper gets from an HWND and exactly what the window2 contract asks for:
//! `get_window` "rehydrates a currently open window by id", so the id only has to
//! survive while the window does. Nothing here invents a hash: a derived id would
//! change whenever a title changed, which would break `get_window` for no benefit.
//!
//! Enumeration follows EWMH first (`_NET_CLIENT_LIST`) and falls back to a tree walk
//! when no window manager answers. The fallback is not a nicety: without a WM there is
//! no `_NET_CLIENT_LIST` at all, and the headless test session has no WM.

use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, bail, Result};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageData, ClientMessageEvent, ConnectionExt as _, EventMask,
    PropMode, Window,
};
use x11rb::rust_connection::RustConnection;

use super::connection::{with_connection, X11Connection};

/// A targetable X11 window in the shape window2 asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X11Window {
    /// The X window id, which is the window2 `Window.id`.
    pub id: u64,
    /// The window2 `Window.app`: the WM_CLASS class name when the client sets one.
    pub app: String,
    pub title: Option<String>,
    pub pid: Option<u32>,
    pub wm_class: Option<String>,
    pub wm_instance: Option<String>,
    pub workspace: Option<i64>,
    pub focused: bool,
    pub hidden: bool,
    pub override_redirect: bool,
    pub window_type: Option<String>,
}

/// Position and size, plus the root-space origin used for input translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u16,
    pub height: u16,
}

/// Frame extents a reparenting window manager adds around the client area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameExtents {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
}

/// The EWMH/ICCCM atoms this module reads, interned once per process.
#[derive(Debug, Clone, Copy)]
pub struct Atoms {
    pub net_client_list: Atom,
    pub net_client_list_stacking: Atom,
    pub net_supporting_wm_check: Atom,
    pub net_wm_name: Atom,
    pub net_wm_pid: Atom,
    pub net_wm_desktop: Atom,
    pub net_wm_window_type: Atom,
    pub net_wm_window_type_desktop: Atom,
    pub net_wm_window_type_dock: Atom,
    pub net_active_window: Atom,
    pub net_frame_extents: Atom,
    pub net_current_desktop: Atom,
    pub utf8_string: Atom,
    pub wm_name: Atom,
    pub wm_class: Atom,
    pub wm_state: Atom,
}

impl Atoms {
    fn intern(raw: &RustConnection) -> Result<Self> {
        let intern = |name: &str| -> Result<Atom> {
            Ok(raw
                .intern_atom(false, name.as_bytes())
                .map_err(|error| anyhow!("intern_atom({name}) failed: {error}"))?
                .reply()
                .map_err(|error| anyhow!("intern_atom({name}) was refused: {error}"))?
                .atom)
        };
        Ok(Self {
            net_client_list: intern("_NET_CLIENT_LIST")?,
            net_client_list_stacking: intern("_NET_CLIENT_LIST_STACKING")?,
            net_supporting_wm_check: intern("_NET_SUPPORTING_WM_CHECK")?,
            net_wm_name: intern("_NET_WM_NAME")?,
            net_wm_pid: intern("_NET_WM_PID")?,
            net_wm_desktop: intern("_NET_WM_DESKTOP")?,
            net_wm_window_type: intern("_NET_WM_WINDOW_TYPE")?,
            net_wm_window_type_desktop: intern("_NET_WM_WINDOW_TYPE_DESKTOP")?,
            net_wm_window_type_dock: intern("_NET_WM_WINDOW_TYPE_DOCK")?,
            net_active_window: intern("_NET_ACTIVE_WINDOW")?,
            net_frame_extents: intern("_NET_FRAME_EXTENTS")?,
            net_current_desktop: intern("_NET_CURRENT_DESKTOP")?,
            utf8_string: intern("UTF8_STRING")?,
            wm_name: intern("WM_NAME")?,
            wm_class: intern("WM_CLASS")?,
            wm_state: intern("WM_STATE")?,
        })
    }
}

static ATOMS: OnceLock<Mutex<Option<CachedAtoms>>> = OnceLock::new();

struct CachedAtoms {
    /// The connection generation these atoms were interned on.
    generation: u64,
    atoms: Atoms,
}

fn atom_slot() -> &'static Mutex<Option<CachedAtoms>> {
    ATOMS.get_or_init(|| Mutex::new(None))
}

/// Intern the atom set for this connection, reusing it across calls.
///
/// Atom values are per-connection, so the cache carries the connection generation it
/// was built on and is rebuilt whenever the connection is replaced. Caching without
/// that key is a real bug, not a theoretical one: after a session switch the helper
/// would keep asking the new server about atom ids that only meant something on the old
/// one, and every property read would come back `BadAtom`.
pub fn atoms_on(connection: &X11Connection) -> Result<Atoms> {
    let current = super::connection::generation();
    let mut guard = atom_slot()
        .lock()
        .map_err(|_| anyhow!("the X11 atom cache lock was poisoned"))?;
    let stale = guard
        .as_ref()
        .is_some_and(|cached| cached.generation != current);
    if stale {
        *guard = None;
    }
    if guard.is_none() {
        *guard = Some(CachedAtoms {
            generation: current,
            atoms: Atoms::intern(connection.inner())?,
        });
    }
    guard
        .as_ref()
        .map(|cached| cached.atoms)
        .ok_or_else(|| anyhow!("the X11 atom cache could not be initialized"))
}

/// Read a 32-bit property, returning `None` when it is absent or another type.
pub fn property_u32(
    raw: &RustConnection,
    window: Window,
    property: Atom,
    type_: Atom,
) -> Result<Option<Vec<u32>>> {
    let reply = raw
        .get_property(false, window, property, type_, 0, 4096)
        .map_err(|error| anyhow!("get_property failed: {error}"))?
        .reply()
        .map_err(|error| anyhow!("get_property was refused: {error}"))?;
    if reply.format != 32 || reply.value.is_empty() {
        return Ok(None);
    }
    Ok(Some(reply.value32().map(|values| values.collect()).unwrap_or_default()))
}

/// Read a byte-string property of any type.
pub fn property_bytes(
    raw: &RustConnection,
    window: Window,
    property: Atom,
) -> Result<Option<Vec<u8>>> {
    let reply = raw
        .get_property(false, window, property, AtomEnum::ANY, 0, 4096)
        .map_err(|error| anyhow!("get_property failed: {error}"))?
        .reply()
        .map_err(|error| anyhow!("get_property was refused: {error}"))?;
    if reply.value.is_empty() {
        return Ok(None);
    }
    Ok(Some(reply.value))
}

/// Read a property as text: UTF8_STRING first, then any type, then WM_NAME.
///
/// `WM_NAME` is not always UTF-8 (ICCCM has no encoding guarantee), and
/// `_NET_WM_NAME` is defined as UTF8_STRING, so the two are read in that order and
/// lossily decoded rather than rejected: a title with one bad byte is still a title.
pub fn property_text(
    raw: &RustConnection,
    window: Window,
    utf8_property: Atom,
    utf8_type: Atom,
    legacy_property: Atom,
) -> Result<Option<String>> {
    for (property, type_) in [(utf8_property, utf8_type), (utf8_property, AtomEnum::ANY.into())] {
        let reply = raw
            .get_property(false, window, property, type_, 0, 4096)
            .map_err(|error| anyhow!("get_property failed: {error}"))?
            .reply()
            .map_err(|error| anyhow!("get_property was refused: {error}"))?;
        if !reply.value.is_empty() {
            return Ok(Some(String::from_utf8_lossy(&reply.value).to_string()));
        }
    }
    if let Some(bytes) = property_bytes(raw, window, legacy_property)? {
        return Ok(Some(String::from_utf8_lossy(&bytes).to_string()));
    }
    Ok(None)
}

/// The window manager's own window, or `None` when no EWMH WM is running.
pub fn window_manager_window(connection: &X11Connection) -> Result<Option<Window>> {
    let raw = connection.inner();
    let atoms = atoms_on(connection)?;
    let Some(values) = property_u32(
        raw,
        connection.root(),
        atoms.net_supporting_wm_check,
        AtomEnum::WINDOW.into(),
    )?
    else {
        return Ok(None);
    };
    Ok(values.first().copied())
}

/// The `_NET_ACTIVE_WINDOW` client, when the WM publishes one.
pub fn active_window(connection: &X11Connection) -> Result<Option<Window>> {
    let atoms = atoms_on(connection)?;
    let values = property_u32(
        connection.inner(),
        connection.root(),
        atoms.net_active_window,
        AtomEnum::WINDOW.into(),
    )?;
    Ok(values.and_then(|values| values.first().copied()).filter(|id| *id != 0))
}

/// Enumerate the windows a window2 caller can target.
pub fn list_windows() -> Result<Vec<X11Window>> {
    with_connection(|connection| {
        let raw = connection.inner();
        let atoms = atoms_on(connection)?;
        let active = active_window(connection).unwrap_or(None);
        let ids = client_window_ids(connection, &atoms)?;
        let mut windows = Vec::with_capacity(ids.len());
        for id in ids {
            match describe_window(raw, &atoms, id, active, connection) {
                Ok(Some(window)) => windows.push(window),
                Ok(None) => {}
                // A window can vanish between listing and describing it; that is the
                // desktop changing under us, not an error worth failing the whole call.
                Err(_) => {}
            }
        }
        Ok(windows)
    })?
}

/// The id list, EWMH first and a tree walk when no WM is present.
fn client_window_ids(connection: &X11Connection, atoms: &Atoms) -> Result<Vec<Window>> {
    let raw = connection.inner();
    if let Some(values) = property_u32(
        raw,
        connection.root(),
        atoms.net_client_list,
        AtomEnum::WINDOW.into(),
    )? {
        if !values.is_empty() {
            return Ok(values);
        }
    }
    // No WM: walk the root's children. A compositor-less Xvfb session lands here, and
    // so does a bare X session where the WM died.
    let tree = raw
        .query_tree(connection.root())
        .map_err(|error| anyhow!("query_tree failed: {error}"))?
        .reply()
        .map_err(|error| anyhow!("query_tree was refused: {error}"))?;
    Ok(tree.children)
}

fn describe_window(
    raw: &RustConnection,
    atoms: &Atoms,
    id: Window,
    active: Option<Window>,
    connection: &X11Connection,
) -> Result<Option<X11Window>> {
    let attributes = raw
        .get_window_attributes(id)
        .map_err(|error| anyhow!("get_window_attributes failed: {error}"))?
        .reply()
        .map_err(|error| anyhow!("get_window_attributes was refused: {error}"))?;
    if attributes.class == x11rb::protocol::xproto::WindowClass::INPUT_ONLY {
        return Ok(None);
    }

    let (wm_instance, wm_class) = read_wm_class(raw, atoms, id)?;
    let title = read_title(raw, atoms, id)?;
    // Without a WM there is no _NET_CLIENT_LIST, so the tree walk sees every child of
    // the root: helper windows, menus and unmapped scaffolding included. A window with
    // no WM_CLASS and no title is not something a caller can reason about, so it is
    // not offered as a target.
    if wm_class.is_none() && wm_instance.is_none() && title.is_none() {
        return Ok(None);
    }
    if attributes.override_redirect && wm_class.is_none() && title.is_none() {
        return Ok(None);
    }

    let window_type = read_window_type(raw, atoms, id)?;
    let is_desktop_or_dock = window_type
        .as_deref()
        .is_some_and(|kind| kind == "desktop" || kind == "dock");
    if is_desktop_or_dock {
        return Ok(None);
    }

    let desktop = property_u32(raw, id, atoms.net_wm_desktop, AtomEnum::CARDINAL.into())?
        .and_then(|values| values.first().copied())
        .map(i64::from);
    let pid = property_u32(raw, id, atoms.net_wm_pid, AtomEnum::CARDINAL.into())?
        .and_then(|values| values.first().copied());
    let hidden = read_hidden(raw, atoms, id, &attributes)?;
    let app = wm_class
        .clone()
        .or_else(|| wm_instance.clone())
        .unwrap_or_else(|| format!("x11:0x{id:x}"));

    let _ = connection;
    Ok(Some(X11Window {
        id: u64::from(id),
        app,
        title,
        pid,
        wm_class,
        wm_instance,
        workspace: desktop,
        focused: active == Some(id),
        hidden,
        override_redirect: attributes.override_redirect,
        window_type,
    }))
}

fn read_hidden(
    raw: &RustConnection,
    atoms: &Atoms,
    id: Window,
    attributes: &x11rb::protocol::xproto::GetWindowAttributesReply,
) -> Result<bool> {
    if attributes.map_state != x11rb::protocol::xproto::MapState::VIEWABLE {
        return Ok(true);
    }
    // WM_STATE IconicState (3) is how ICCCM reports a minimized window.
    if let Ok(Some(values)) = property_u32(raw, id, atoms.wm_state, AtomEnum::ANY.into()) {
        if values.first().copied() == Some(3) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_wm_class(
    raw: &RustConnection,
    atoms: &Atoms,
    id: Window,
) -> Result<(Option<String>, Option<String>)> {
    let Some(bytes) = property_bytes(raw, id, atoms.wm_class)? else {
        return Ok((None, None));
    };
    // WM_CLASS is two NUL-terminated strings: instance first, then class.
    let mut parts = bytes.split(|byte| *byte == 0);
    let instance = parts.next().unwrap_or_default();
    let class = parts.next().unwrap_or_default();
    let clean = |slice: &[u8]| {
        let text = String::from_utf8_lossy(slice).trim().to_string();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    };
    Ok((clean(instance), clean(class)))
}

fn read_title(
    raw: &RustConnection,
    atoms: &Atoms,
    id: Window,
) -> Result<Option<String>> {
    let title = property_text(raw, id, atoms.net_wm_name, atoms.utf8_string, atoms.wm_name)?;
    Ok(title.filter(|text| !text.trim().is_empty()))
}

fn read_window_type(
    raw: &RustConnection,
    atoms: &Atoms,
    id: Window,
) -> Result<Option<String>> {
    let Some(values) = property_u32(raw, id, atoms.net_wm_window_type, AtomEnum::ATOM.into())?
    else {
        return Ok(None);
    };
    let Some(atom) = values.first().copied() else {
        return Ok(None);
    };
    let name = raw
        .get_atom_name(atom)
        .map_err(|error| anyhow!("get_atom_name failed: {error}"))?
        .reply()
        .map_err(|error| anyhow!("get_atom_name was refused: {error}"))?;
    let text = String::from_utf8_lossy(&name.name).to_string();
    Ok(Some(
        text.strip_prefix("_NET_WM_WINDOW_TYPE_")
            .unwrap_or(&text)
            .to_ascii_lowercase(),
    ))
}

/// Rehydrate one window by its handle.
pub fn get_window(id: u64) -> Result<X11Window> {
    let window = Window::try_from(id)
        .map_err(|_| anyhow!("window id {id} does not fit in an X11 window id"))?;
    with_connection(|connection| {
        let raw = connection.inner();
        let atoms = atoms_on(connection)?;
        let active = active_window(connection).unwrap_or(None);
        match describe_window(raw, &atoms, window, active, connection) {
            Ok(Some(window)) => Ok(window),
            Ok(None) => bail!(
                "window 0x{id:x} exists but is not a targetable window (no WM_CLASS or title)"
            ),
            Err(error) => Err(error),
        }
    })?
}

/// The client area's origin in root coordinates, plus its size.
pub fn window_geometry(id: u64) -> Result<WindowGeometry> {
    let window = Window::try_from(id)
        .map_err(|_| anyhow!("window id {id} does not fit in an X11 window id"))?;
    with_connection(|connection| {
        let raw = connection.inner();
        let geometry = raw
            .get_geometry(window)
            .map_err(|error| anyhow!("get_geometry failed: {error}"))?
            .reply()
            .map_err(|error| anyhow!("get_geometry was refused for 0x{id:x}: {error}"))?;
        // get_geometry is relative to the parent, and a reparenting WM makes that the
        // frame. translate_coordinates asks the server for the same information in
        // root space, which is what input injection needs.
        let translated = raw
            .translate_coordinates(window, connection.root(), 0, 0)
            .map_err(|error| anyhow!("translate_coordinates failed: {error}"))?
            .reply()
            .map_err(|error| anyhow!("translate_coordinates was refused for 0x{id:x}: {error}"))?;
        Ok(WindowGeometry {
            x: i32::from(translated.dst_x),
            y: i32::from(translated.dst_y),
            width: geometry.width,
            height: geometry.height,
        })
    })?
}

/// The window manager's frame extents, when it publishes them.
pub fn frame_extents(id: u64) -> Result<FrameExtents> {
    let window = Window::try_from(id)
        .map_err(|_| anyhow!("window id {id} does not fit in an X11 window id"))?;
    with_connection(|connection| {
        let atoms = atoms_on(connection)?;
        let values = property_u32(
            connection.inner(),
            window,
            atoms.net_frame_extents,
            AtomEnum::CARDINAL.into(),
        )?;
        Ok(match values.as_deref() {
            Some([left, right, top, bottom, ..]) => FrameExtents {
                left: *left,
                right: *right,
                top: *top,
                bottom: *bottom,
            },
            _ => FrameExtents::default(),
        })
    })?
}

/// Map a window-relative point to root coordinates.
///
/// Input injection always addresses the root, so every click/scroll coordinate has to
/// cross this boundary. The client origin already accounts for a reparenting WM
/// (`translate_coordinates` reports where the client area actually is), so frame
/// extents are *not* added on top: adding them would double-count the frame and put
/// every click off by the title bar height.
pub fn to_root_coordinates(
    id: u64,
    x: i32,
    y: i32,
) -> Result<(i32, i32)> {
    let geometry = window_geometry(id)?;
    Ok((
        geometry.x.saturating_add(x),
        geometry.y.saturating_add(y),
    ))
}

/// The EWMH _NET_ACTIVE_WINDOW source indication this helper sends.
///
/// 1 is "a normal application" and 2 is "a pager". KWin honours a pager request from a
/// client that cannot prove a user gesture and refuses an application one, so the pager
/// value is the one that actually raises a window on the operator's behalf. Kept as a
/// named constant so the unit test below pins the wire value rather than a copy of it.
pub const ACTIVE_WINDOW_SOURCE_PAGER: u32 = 2;

/// The _NET_ACTIVE_WINDOW client message this helper sends, as wire data.
///
/// Split out so the message can be asserted without a window manager or a display: the
/// defect this pins was a single wrong number, and it was invisible to every test that
/// only checked that activate_window returned Ok.
pub fn active_window_message(window: Window, atom: x11rb::protocol::xproto::Atom) -> ClientMessageEvent {
    ClientMessageEvent {
        response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: atom,
        // [source, timestamp, requestor's currently active window, 0, 0]
        data: ClientMessageData::from([ACTIVE_WINDOW_SOURCE_PAGER, 0, 0, 0, 0]),
    }
}

/// Bring a window to the foreground the way EWMH intends.
pub fn activate_window(id: u64) -> Result<String> {
    let window = Window::try_from(id)
        .map_err(|_| anyhow!("window id {id} does not fit in an X11 window id"))?;
    with_connection(|connection| {
        let raw = connection.inner();
        let atoms = atoms_on(connection)?;
        let wm = window_manager_window(connection)?;
        if wm.is_some() {
            // _NET_ACTIVE_WINDOW with source indication 2 (pager) and no timestamp.
            //
            // EWMH says source 1 means "a normal application" and lets the window manager
            // apply focus-stealing prevention; source 2 means "a pager", which is a
            // request the WM is expected to honour for a window it is not certain the
            // user just looked at. KWin implements that distinction strictly and ignores
            // source 1 from a client that has no user-gesture timestamp, which is why a
            // raise that reported success changed nothing on the real desktop (measured:
            // source 1 -> _NET_ACTIVE_WINDOW unchanged; source 2 -> it changes). The
            // helper is acting on the operator's behalf, which is what the pager
            // indication is for; there is no timestamp to supply, so the WM keeps
            // deciding how to focus, it just stops refusing.
            let event = active_window_message(window, atoms.net_active_window);
            raw.send_event(
                false,
                connection.root(),
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )
            .map_err(|error| anyhow!("send_event(_NET_ACTIVE_WINDOW) failed: {error}"))?
            .check()
            .map_err(|error| anyhow!("send_event(_NET_ACTIVE_WINDOW) was refused: {error}"))?;
            return Ok("raised via EWMH _NET_ACTIVE_WINDOW".to_string());
        }
        // No WM to ask, so do the two things the WM would have done.
        raw.map_window(window)
            .map_err(|error| anyhow!("map_window failed: {error}"))?
            .check()
            .map_err(|error| anyhow!("map_window was refused: {error}"))?;
        raw.configure_window(window, &x11rb::protocol::xproto::ConfigureWindowAux::new().stack_mode(x11rb::protocol::xproto::StackMode::ABOVE))
            .map_err(|error| anyhow!("configure_window failed: {error}"))?
            .check()
            .map_err(|error| anyhow!("configure_window was refused: {error}"))?;
        raw.set_input_focus(
            x11rb::protocol::xproto::InputFocus::PARENT,
            window,
            x11rb::CURRENT_TIME,
        )
        .map_err(|error| anyhow!("set_input_focus failed: {error}"))?
        .check()
        .map_err(|error| anyhow!("set_input_focus was refused: {error}"))?;
        raw.flush().ok();
        Ok("raised by mapping, restacking and focusing (no EWMH window manager)".to_string())
    })?
}

/// Set an X property the way a test or a WM would.
pub fn set_window_property(
    window: Window,
    property: &str,
    value: &str,
) -> Result<()> {
    with_connection(|connection| {
        let raw = connection.inner();
        let property = raw
            .intern_atom(false, property.as_bytes())
            .map_err(|error| anyhow!("intern_atom failed: {error}"))?
            .reply()
            .map_err(|error| anyhow!("intern_atom was refused: {error}"))?
            .atom;
        raw.change_property(
            PropMode::REPLACE,
            window,
            property,
            AtomEnum::STRING,
            8,
            value.len() as u32,
            value.as_bytes(),
        )
        .map_err(|error| anyhow!("change_property failed: {error}"))?
        .check()
        .map_err(|error| anyhow!("change_property was refused: {error}"))?;
        Ok(())
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_asks_the_window_manager_as_a_pager_not_as_an_application() {
        // The defect this pins: the message was sent with source 1 (application), which
        // KWin answers with a refusal-by-silence -- send_event succeeds, the window does
        // not come up. EWMH source 2 (pager) is the indication a window manager is
        // expected to honour from a client acting on the operator's behalf.
        let message = active_window_message(0x1234, 0x2a);
        assert_eq!(ACTIVE_WINDOW_SOURCE_PAGER, 2, "EWMH source 2 is the pager indication");
        assert_eq!(message.data.as_data32()[0], 2, "data[0] is the source indication");
        assert_eq!(message.data.as_data32()[1], 0, "no timestamp: the WM keeps deciding");
        assert_eq!(message.data.as_data32()[2], 0, "no requestor window");
        assert_eq!(message.window, 0x1234, "the message is addressed to the target window");
        assert_eq!(message.type_, 0x2a, "the message type is _NET_ACTIVE_WINDOW");
        assert_eq!(message.format, 32);
        assert_eq!(
            message.response_type,
            x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT
        );
    }

    #[test]
    fn a_window_id_that_does_not_fit_a_window_is_rejected() {
        let error = get_window(u64::MAX).unwrap_err();
        assert!(error.to_string().contains("does not fit"));
    }

    #[test]
    fn property_reads_outside_a_session_fail_instead_of_panicking() {
        // Nothing here may panic when DISPLAY is missing; the helper has to be able to
        // answer health with a degraded report on a Wayland session.
        let _ = list_windows();
        let _ = window_manager_window_of_nothing();
    }

    fn window_manager_window_of_nothing() -> Option<Window> {
        None
    }

    #[test]
    fn frame_extents_default_to_zero_without_a_manager() {
        // The default must be a no-op translation, never a silent offset.
        let extents = FrameExtents::default();
        assert_eq!(extents.left + extents.top, 0);
    }
}