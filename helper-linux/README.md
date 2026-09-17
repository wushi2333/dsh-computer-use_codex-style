# helper-linux — DSH Computer Use, Linux backend

`dsh-computer-use` is the Linux desktop driver for the `dsh-computer-use` plugin. It is a
**fork** of [`ilysenko/codex-desktop-linux`](https://github.com/ilysenko/codex-desktop-linux)
(MIT), specifically its `computer-use-linux` crate, re-pointed at this plugin's stdio JSONL
helper protocol. The desktop backends — AT-SPI accessibility, XDG Desktop Portal screenshot
and input, and the per-compositor window backends (KWin, GNOME Shell, Hyprland, Niri, i3,
COSMIC, X11) — are upstream code and are kept as close to upstream as the protocol change
allows.

```
fork source   https://github.com/ilysenko/codex-desktop-linux
upstream crate computer-use-linux 0.4.9-linux-alpha1
upstream commit 5f7310d71dd02e6e0131deec6fa89d26c8bcaf9c
license        MIT (see LICENSE, copied from the upstream repository)
```

## Layout

| path | what it is |
| --- | --- |
| `src/protocol.rs` | the JSONL request/response envelope, and the split of a screenshot into its own image part |
| `src/helper.rs` | the dispatcher: `health` / `tools` / `call` / `interrupt` / `shutdown` / `prompt` / `end_turn`, and the seven-tool surface |
| `src/server.rs` | upstream MCP server, unchanged in its handlers; the fork only adds the two hooks the dispatcher drives |
| `src/x11/` | the native X11 window2 backend: EWMH enumeration, SHM/XComposite capture, XTest input, AT-SPI element indexes |
| `src/main.rs` | entry point: no arguments (or `helper`) speaks JSONL, `mcp` keeps the original MCP server, the rest are upstream diagnostics subcommands |
| `scripts/smoke-jsonl.sh` | protocol-level smoke test; `scripts/smoke_assert.py` holds its assertions |
| `gnome-shell-extension/` | upstream optional window-targeting extension |

## Build

```sh
cargo build --release            # -> target/release/dsh-computer-use
cargo test                       # upstream suite plus the protocol tests
bash scripts/smoke-jsonl.sh      # drives the built binary over a pipe
```

The X11 window2 backend is pure Rust through `x11rb` and needs **no** X11 development
package: `x11rb`'s default features never link libxcb, and SHM/XTest/XFixes/XComposite come
from `x11rb-protocol`'s wire definitions. There is no `pkg-config` step and no system X
library at build time.

### Integration tests

`tests/xvfb_window2.rs` drives the backend against a real X server that the test binary
starts and stops itself. They are ignored by default because they need `Xvfb`:

```sh
DSH_CUA_XVFB_TEST=1 cargo test --test xvfb_window2 -- --ignored --test-threads=1
```

Without the environment variable the whole file is skipped, so `cargo test` stays green on
a machine with no X server. The tests use their own displays (`:98`-`:134`) and never
touch the `DISPLAY` the developer is working in.

`launch_app` is covered there end to end: the launcher resolves a desktop entry the test
writes into a private `XDG_DATA_HOME`, starts the app detached, waits for its window, and a
second launch raises that window instead of starting another instance. Those tests start a
real `xterm` and kill it again.

### Real-session launch checks

`tests/real_launch.rs` runs the same launcher against the operator's own X server. It is
gated a second time because it acts on the live desktop (it may raise a window and take the
focus for a moment), and it only ever starts an `xterm` of its own, which it kills:

```sh
DSH_CUA_REAL_LAUNCH=1 cargo test --test real_launch -- --ignored --test-threads=1 --nocapture
```

Three cases are marked ignored for a reason that is more than "needs Xvfb":

- **EWMH activation** and **window-manager frame extents** need a running window manager.
  None is installed here, so the no-WM path is what is verified.
- **Element indexes end to end** need a real toolkit app registered on an accessibility bus;
  the Xvfb fixture is a raw X window with no accessibility tree. The index cache's own
  behaviour is covered by unit tests in `src/x11/element.rs`.
- **A redirection owned by another client** (what a running compositor looks like) cannot be
  reproduced on Xvfb, which does not enforce cross-client redirection ownership. The fallback
  is covered deterministically by a unit test in `src/x11/capture.rs`; the real-session case
  stays unverified until it is run on a session with a compositor.

## Protocol

One JSON object per line in, one per line out.

```json
-> {"id":1,"method":"call","params":{"name":"list_apps","arguments":{}},"meta":{"x-oai-cua-request-budget-ms":15000}}
<- {"id":1,"ok":true,"result":{"ok":true,"name":"list_apps","value":{...},"images":[]}}
```

Methods: `health`, `tools`, `call`, `interrupt`, `shutdown`, `prompt`, `end_turn`.

- `health` reports what the helper is and what actually works **on this machine**, taken from
  the crate's own diagnostics probes. Failed checks are listed verbatim under `degraded`;
  nothing is reported as working that did not prove it.
- `health.experience` keeps **availability** and **activation** apart, because they are
  different facts: `state` is `"off"` (this session has no layer), `"available"` (the layer
  exists and nothing is armed) or `"armed"` (a turn is live), and `armed`/`escapeGrab` report
  the specifics. A layer that negotiated every extension but was never armed must not be
  able to call itself `"on"`.
- `tools` lists exactly the seven exposed tools, with the crate's own JSON Schemas.
- `call` returns `{ok, name, value, images}`. Screenshot pixels are never inlined into `value`:
  each image becomes its own part as `{mimeType, data, name}` with the `data:` URL prefix
  stripped, and the caption stays in `value`.
- `interrupt` stops the call in progress (not merely the next one) and latches the helper
  stopped until `end_turn` clears it.
- `end_turn` is safe to call repeatedly and never strands the helper: the host reuses one
  process across turns.
- `shutdown` is terminal; calls the host already sent are answered as abandoned rather than
  run to completion.

Unknown methods and unadvertised tools are both refused as `unsupported method: <name>`, which
is the upstream helper's own wording.

## Tool surface

Exactly seven tools, under their upstream Linux names and parameter shapes:
`list_apps`, `get_app_state`, `screenshot`, `click`, `scroll`, `press_key`, `type_text`.

The crate advertises more tools over MCP (`doctor`, `setup_accessibility`,
`setup_window_targeting`, `list_windows`, `focused_window`, `activate_window`,
`perform_action`, `set_value`, `drag`, `move_window`, `resize_window`). They are neither listed
by `tools` nor routable through `call`.

`get_app_state` accepts `app_name_or_bundle_identifier` as well as the window selectors
(`window_id`, `pid`, `app_id`, `wm_class`, `title`); `window_id` is the numeric window id from
`list_apps`, so no separate `linux-window:<id>` string form is needed.

### window2 surface (X11)

When `tools` is asked with `surface: "window2"` (or `computer`), the helper answers with the
official 13-method window2 table instead: `list_windows`, `get_window`, `list_apps`,
`launch_app`, `get_window_state`, `click`, `press_key`, `type_text`, `scroll`, `set_value`,
`drag`, `perform_secondary_action`, `activate_window`. The seven sky.window tools above are
unchanged and stay reachable; where a name exists on both surfaces it keeps its original
sky.window handler, because the host tags only `tools` with a surface, never `call`.

The window2 backend is native X11 (`src/x11/`), not the `wmctrl`+`xprop` window backend: it
talks to the server directly through `x11rb`, so it needs no external binaries.

- **Window handles** are X window ids (an opaque XID widened to `u64`). They are stable for as
  long as the window exists, which is what `get_window` promises. Windows are enumerated from
  `_NET_CLIENT_LIST` when a window manager is running and from the root window tree otherwise;
  the fallback skips children with neither `WM_CLASS` nor a title, which have nothing a caller
  could act on.
- **Input** goes through XTest. Every window2 coordinate is window-relative, so each call
  translates it to root coordinates with `translate_coordinates` (which already accounts for a
  reparenting window manager). `press_key` and `type_text` focus the target window first.
- **Capture** uses `ShmGetImage` on the window drawable, preferring an XComposite redirect so the
  image is the window's own pixels rather than whatever overlaps it. See the accuracy note below.
- **Element indexes** come from AT-SPI and are valid only against the tree of the
  `get_window_state` call that produced them. An index used without a snapshot, or one outside
  the captured tree, is refused with a message saying to re-observe rather than being resolved
  against a tree the caller never saw.
- **`launch_app` is refused**, structurally and by design: X11 has no application registry to
  resolve an app id against, and executing a caller-supplied string as a program would be an
  injection hole. The refusal names the method, the reason and what to do instead.

#### Accuracy note: occlusion and capture

`get_window_state` captures the window's **own pixels** and never an overlapping window's. It
does **not** deliver live pixels of a window that is completely covered and not painting: each
capture redirects the window afresh, and a fresh off-screen buffer starts at the window's
background, so such a window yields its background rather than its last painted content.
Content painted while the redirection is in effect is captured, even under an opaque cover.

Windows' DWM keeps a backing bitmap per window and can do better in both cases; plain X11
cannot. `health.window2.occlusionNote` states this, and `get_window_state` reports `degraded`
whenever it had to fall back to a direct read (a missing Composite extension, or a compositor
that already owns the window's redirection).

All of this is verified in this repository under Xvfb, without a window manager; the
compositor-conflict and window-manager cases are listed as unverified in the integration-test
section above, not asserted.

## Experience layer (X11)

On an X11 session the helper also runs the window2 experience layer: an override-redirect
status pill, a synthesized pointer with the real one suppressed through XFixes, a freshness
lease fed by XInput2 raw events, and Escape as a global interrupt.

**It arms on observation, whichever surface the call arrived on.** A `get_window_state`,
`get_app_state` or `screenshot` call arms the turn: the pill comes up, the synthesized pointer
takes over, the Escape grab is installed and the lease starts watching for human input. A call
that changes the desktop (`click`, `press_key`, `type_text`, `scroll`, `set_value`, `drag`,
`perform_secondary_action`, `activate_window`) shows the pill in its working state while it runs.
Calls that only read a table (`list_windows`, `get_window`, `list_apps`) do not arm anything, and
`launch_app` is not implemented on X11. `end_turn`, `interrupt` and `shutdown` hand the desktop
back: the overlays are unmapped, the real pointer is restored, the lease is flushed and the
grab is released.

Two things about that are deliberate:

- **The layer is per X session, not per surface.** The plugin's default Linux surface is the
  P1 sky.window one, so arming only window2 faces would leave the pill, the synthesized
  cursor and the Escape interrupt absent on the default path — the layer would exist and
  never run. Both surfaces arm the same single layer, and arming never changes what a call
  returns.
- **This differs from the Windows helper, on purpose.** `helper-rs` arms from its overlay's
  `show()`, which only its input methods call, so a Windows observation does not arm;
  here observation does. That matches this crate's own rule ("observation begins for this
  turn") and is what the pill is for — telling the operator that the machine is being looked
  at.

**The overlays are click-through.** An override-redirect window that is merely drawn on top
still wins hit-testing, so a sprite that follows the pointer would swallow the very clicks the
helper is synthesizing — and a pill parked in a corner would swallow whatever the operator
clicks there. Both overlays therefore carry an empty XFixes input region (`ShapeInput`, the
X11 equivalent of the Windows overlay's `WS_EX_TRANSPARENT`), and
`health.experience.overlayClickThrough` reports whether the server accepted the request.
This was a real regression: arming the layer made the synthesized cursor intercept clicks, and
the headless window2 end-to-end run failed with `[FAIL] xterm clicks land inside the
target window  under pointer=0x400003 family=None`.

**If click-through cannot be granted, the overlays are not drawn at all.** A server without
usable XFixes (or one that refuses the request) gets no pill and no synthesized pointer, the
real pointer is left visible, and `health.experience.degraded` says so. Hiding the real
pointer without drawing a replacement would leave the operator with no pointer, and drawing an
overlay that eats clicks breaks every synthesized click — both are worse than a missing pill, so
the layer refuses rather than warns. `health.experience.overlayClickThrough` reports the
decision. The branch is covered against a real Xvfb started with `-extension XFIXES`,
not a stub.

**A refused Escape grab is a normal desktop, not a failure.** The bare Escape combination is
routinely held by the compositor's own global shortcut (KWin holds it on the machine this was
verified on), and a second client asking for it gets `BadAccess`. The layer stays fully
functional: XInput2 raw key events are delivered whether or not the grab was granted, so an
armed Escape is still an interrupt (measured: 147 ms from the keystroke to the desktop being
handed back). `health.experience.escapeGrab.state` then reads `"refused"` and
`health.experience.degraded` says so in words — never `"installed"`.

## Environment notes

- **Wayland:** screenshot and input go through `xdg-desktop-portal`. The portal must have a
  backend matching the session (for example `xdg-desktop-portal-kde` on Plasma); the first
  screenshot may raise a system permission dialog the user has to accept. When the portal is
  missing or unauthorised, `health.degraded` and the tool result say so — the helper never
  reports a capture that did not happen.
- **AT-SPI:** element-targeted actions need accessibility enabled in the session. If it is off,
  `get_app_state` returns `accessibility_error` and an empty tree rather than a partial one
  presented as complete.
- **Window control** is per compositor; `health.windowing.backends` reports which backends
  answered here. The shipped `gnome-shell-extension/` is only used on GNOME.
