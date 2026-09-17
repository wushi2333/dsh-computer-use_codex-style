## Linux P2 window2 API Reference (Codex Parity Surface)

Use this as the supported Linux window2 Computer Use API surface exposed by `helper-linux` in P2.
This surface provides **13-tool Codex parity** on Linux desktop sessions, matching the Windows window2 contract defined in `references/api.md`.

```ts
import { sky } from "@oai/sky";

const apps = await sky.list_apps();
const candidate_windows = apps.flatMap((app) => app.windows);
// Select target app and window before acting.
// Each action targets a specific Window object.

interface LinuxWindow2ComputerUseClient {
  list_windows(): Promise<Array<Window>>; // Enumerate open targetable windows (EWMH on X11).
  get_window(input: GetWindowInput): Promise<Window>; // Rehydrate a known window by id.
  list_apps(): Promise<Array<ListAppsApp>>; // List installed applications and their open windows.
  launch_app(input: LaunchAppInput): Promise<void>; // Launch application by identifier or desktop entry.
  get_window_state(input: GetWindowStateInput): Promise<WindowState>; // Capture screenshot and AT-SPI accessibility tree.
  click(input: ClickInput): Promise<void>; // Click an indexed accessibility element or window coordinates.
  press_key(input: PressKeyInput): Promise<void>; // Press a key or '+'-separated chord (keysym format).
  type_text(input: TypeTextInput): Promise<void>; // Type UTF-8 text into focused element.
  scroll(input: ScrollInput): Promise<void>; // Scroll by delta from specific coordinates.
  set_value(input: SetValueInput): Promise<void>; // Replace value of an indexed editable element.
  drag(input: DragInput): Promise<void>; // Drag between window-relative coordinates.
  perform_secondary_action(input: PerformSecondaryActionInput): Promise<void>; // Invoke secondary action on indexed element.
  activate_window(input: ActivateWindowInput): Promise<void>; // Bring open window to foreground.
  target: "linux";
}

type Window = {
  app: AppIdentifier; // Desktop app identifier (e.g. "org.gnome.TextEditor", "firefox")
  id: number; // Numeric window identifier (X11 Window XID or Wayland target ID)
  title?: string; // User-visible window title
};

type GetWindowInput = {
  app?: AppIdentifier;
  id: number;
};

type ListAppsApp = {
  displayName?: string;
  id: AppIdentifier;
  isRunning?: boolean;
  lastUsedDate?: string;
  useCount?: number;
  windows: Array<Window>;
};

type LaunchAppInput = {
  app: AppIdentifier;
};

type GetWindowStateInput = {
  include_screenshot?: boolean; // Defaults to true
  include_text?: boolean; // Defaults to false; set true to request AT-SPI accessibility tree
  window: Window;
};

type WindowState = {
  accessibility: AccessibilityState | null;
  screenshots: Array<Screenshot>;
  window: Window;
};

type AccessibilityState = {
  tree?: string; // Tree representation with numbered element indexes
  focused_element?: number;
  selected_text?: string;
  selected_elements?: Array<number>;
  document_text?: string;
};

type Screenshot = {
  id: string; // Screenshot identifier cached for coordinate actions
  url?: string; // Data URL when serialized
  zIndex?: number;
  originX?: number;
  originY?: number;
  width?: number;
  height?: number;
};

type MouseButton = "left" | "right" | "middle";

type ClickInput = {
  click_count?: number;
  element_index?: number; // AT-SPI element index from latest get_window_state
  mouse_button?: MouseButton;
  screenshotId?: string;
  window: Window;
  x?: number;
  y?: number;
};

type PressKeyInput = {
  key: string; // Keysym name or chord, e.g. "Return", "Tab", "Control_L+c", "Alt_L+F4"
  window: Window;
};

type TypeTextInput = {
  text: string;
  window: Window;
};

type ScrollInput = {
  screenshotId?: string;
  scrollX: number;
  scrollY: number;
  window: Window;
  x: number;
  y: number;
};

type SetValueInput = {
  element_index: number;
  value: string;
  window: Window;
};

type DragInput = {
  from_x: number;
  from_y: number;
  screenshotId?: string;
  to_x: number;
  to_y: number;
  window: Window;
};

type PerformSecondaryActionInput = {
  action: string; // e.g. "Raise", "Scroll Up", "Scroll Down", "Expand", "Collapse"
  element_index: number;
  window: Window;
};

type ActivateWindowInput = {
  window: Window;
};

type AppIdentifier = string;
```

---

## Parity Goals (Codex Parity on Linux)

The P2 window2 surface achieves full functional parity with the Windows Codex window2 surface:
1. **Identical 13-tool surface**: Direct parity in method names, parameter structures, and response shapes.
2. **Structured Window targeting**: Passing explicit `Window { id, app, title }` rather than loose string identifiers.
3. **Element index addressing**: Rich AT-SPI accessibility tree extraction with stable 1-based element indexes (`element_index`), enabling direct clicking, value replacement (`set_value`), and secondary action invocation.
4. **Visual overlay pill**: High-visibility status pill displayed during automation to notify human operators of active robot actions.
5. **Synthetic cursor**: Cursor movement visualization without stealing or corrupting host hardware cursor state.
6. **Freshness lease**: Tracking observation staleness and invalidating action caches on unexpected desktop state changes.
7. **Physical Escape interrupt**: Global Esc key grab halting automation immediately and cleaning up resources.

---

## Display Server Tiered Architecture

Linux desktop environments vary between X11 and Wayland. `helper-linux` uses a tiered capability architecture:

### 1. X11 Full Mode (Complete Parity)
- **Engine**: Built with `x11rb` (pure Rust X11 protocol implementation), eliminating external C library dependencies (no `libxcb-dev` needed).
- **Window Enumeration**: EWMH (`_NET_CLIENT_LIST`, `_NET_WM_NAME`, `_NET_WM_PID`) provides stable window IDs and process correlation.
- **Occluded Capture**: MIT-SHM (`XShmGetImage`) captures target windows even when partially obscured.
- **Input Injection**: XTest protocol extension reliably injects pointer and keyboard events.
- **Overlay & Cursor**: Override-redirect windows host the status pill; XFixes extension handles cursor rendering.
- **Interrupts**: Passive keygrab on `Escape` captures cancellation even while background applications hold focus.

### 2. Wayland Degraded Mode (Graceful Fallback)
- **Engine**: Bridges through XDG Desktop Portal (`org.freedesktop.portal.ScreenCast` and `RemoteDesktop`) alongside AT-SPI.
- **Capture Boundaries**: Wayland compositors prohibit direct out-of-process window frame capture. Screenshots capture desktop streams via Portal, mapping window bounding boxes where available.
- **Overlay & Grab Boundaries**: Wayland protocols strictly forbid arbitrary screen overlay pills and global keygrabs. The helper disables the overlay pill gracefully and relies on harness-level signal cancellation rather than global key intercepts.
- **Health Reporting**: `computer_use_health` transparently reports current display server mode (`x11` vs `wayland`) and degraded capability flags.

---

## Name Collisions Between the Two Surfaces

Five method names exist on **both** Linux surfaces and mean different things:
`list_apps`, `click`, `press_key`, `type_text`, `scroll`.

* On the window2 surface each acts on an explicit `Window { id, app, title }`, plus
  `element_index` where an accessibility element is addressed; `scroll` takes
  `x`/`y`/`scrollX`/`scrollY`.
* On the P1 `sky.window` surface they take the crate's own shape (an `app` string,
  absolute `x`/`y`, a P1 `direction`/`pages` scroll), which is what a P1 host sends.

Because a name alone cannot say which handler to use, a window2 turn tags the `call`
request with `surface` (a plain request parameter, not a new protocol method):

```jsonc
// window2 turn: reaches the native X11 window2 handler
{"id":1,"method":"call","params":{"name":"click","surface":"computer",
  "arguments":{"window":{"id":2097164,"app":"XTerm"},"element_index":3}}}

// P1 turn: no surface tag, keeps the sky.window handler and parameter shape
{"id":2,"method":"call","params":{"name":"click",
  "arguments":{"app":"linux-window:101","x":250,"y":350}}}
```

The eight window2-only methods (`list_windows`, `get_window`, `launch_app`,
`get_window_state`, `set_value`, `drag`, `perform_secondary_action`, `activate_window`)
have no P1 handler and always reach the window2 dispatcher, tagged or not.
An untagged `call` is therefore bit-for-bit the P1 behaviour it always was: an existing
P1 caller needs no change.

---

## Element Index Scoping and Stability

1. **Observation-bound Lifetime**:
   Element indexes (`element_index`) returned in `accessibility.tree` are valid **only** for the exact observation that produced them. The tree carries the `generation` it was captured as, so the observation is a value you can hold on to rather than a claim you have to remember.
2. **The helper enforces it**: pass `element_generation` (the `generation` of the observation you read the index out of) with an indexed `click`, `set_value` or `perform_secondary_action`. An index whose generation no longer matches the tree the window holds is **refused**, naming both numbers, instead of being resolved against the newer tree where that position may be a different control. The field is optional -- omit it and the index resolves against the latest tree, which is what every existing caller did -- but only an index that names its tree can be refused.
3. **The Two-Cell Loop**:
   Always follow the canonical two-cell loop when using element indexing:
   - **Observe**: Call `get_window_state({ window, include_text: true })` to read visible controls, fresh element indexes and the tree's `generation`.
   - **Act**: Perform exactly one indexed action (e.g. `click({ window, element_index: 4, element_generation: <generation> })` or `set_value({ window, element_index: 4, element_generation: <generation>, value: "text" })`).
   - **Refresh**: Call `get_window_state` again before taking the next action. A refusal that names two generations means exactly this: refresh, then act on an index from the new tree.

---

## `computer_use_wait_for` (DSH extension, not an official method)

The official thirteen are unchanged. This is a **DSH addition** that rides the same Linux
window2 dispatcher: helper method `wait_for`, exposed to the model as `computer_use_wait_for`.
It never appears in the official 13-method table (`window2::WINDOW2_TOOLS`), and the parity
assertions that pin that table still hold.

### Why it exists

The observation cadence, not the driving, is what a desktop task costs. "Act, sleep,
screenshot, look, act" spends one image-carrying model round trip per step, and each of those
is seconds. When the next action only depends on **whether** something appeared, the model does
not need to look at all -- it needs the helper to watch. `computer_use_wait_for` does the
watching in the helper and answers once.

### Parameters

```ts
interface WaitForInput {
  window: Window;           // same shape as get_window_state: { app, id }
  text_substring?: string;  // wait until this text appears in the tree (case-insensitive)
  element_name?: string;    // wait until an element whose name contains this appears
  gone?: string;            // wait until this text is no longer in the tree
  timeout_ms?: number;      // default 5000, hard ceiling 20000
  poll_ms?: number;         // default 250, floor 50
}

interface WaitForResult {
  ok: true;
  matched: boolean;         // false on timeout, which is NOT an error
  elapsedMs: number;
  polls: number;
  condition: { kind: "text_substring" | "element_name" | "gone"; value: string };
  timeoutMs: number;
  pollMs: number;
  timeoutClamped?: true;    // present when timeout_ms exceeded the ceiling
  maxTimeoutMs?: number;
  observedPresent?: boolean; // `gone` only: whether the text was ever seen
  match?: { index: number; role: string; name?: string };
  nodes?: number;
  treeGeneration?: number;
  emptyTree?: true;         // every poll saw an empty tree (no AT-SPI bridge)
  source?: string;          // the AT-SPI app the tree was read from, e.g. "pid 4194308"
  warning?: string;
  note?: string;
  window: Window;
  backend: "x11-native";
}
```

**Exactly one** of `text_substring`, `element_name` or `gone` must be given. Zero or more than
one is refused structurally before any polling, so a malformed call costs nothing.

```json
{ "id": 7, "method": "call",
  "params": { "name": "wait_for", "surface": "computer",
    "arguments": { "window": { "app": "org.gnome.TextEditor", "id": 4194308 },
                   "text_substring": "Save", "timeout_ms": 8000 } } }
```

### Semantics that matter

1. **A timeout is not an error.** The call returns `ok: true` with `matched: false` and a `note`.
   A condition that never held is a fact about the UI; the caller decides what to do next.
2. **`gone` is two-phase.** The wait first checks whether the text is *present* (`observedPresent`).
   If it is, it waits for it to leave -- `matched: true`, `observedPresent: true`. If the text was
   never there during the presence grace (`min(poll_ms, 1000)` ms), the call answers immediately
   with `matched: true` and `observedPresent: false`: the condition already holds, and the caller
   is told it never watched the text go. It does not sit out the whole budget.
3. **`timeout_ms` is clamped to 20000**, and `timeoutClamped: true` plus `maxTimeoutMs` are
   reported. The ceiling is a contract: DSH aborts a tool call at its own ~25 s budget, so a
   longer promise could not be kept and the clamp is surfaced rather than hidden.
4. **The poll does not consume element indexes.** The waiter reads the accessibility tree
   without advancing the cached generation, so element indexes from your last
   `get_window_state` stay valid across a wait.
5. **A window with no accessibility tree is reported, not silently timed out.** If every poll
   returns an empty tree, the result carries `emptyTree: true` and a `warning` -- the app has no
   AT-SPI bridge, so no text or element condition can ever match.
6. **A tree that cannot be read at all is refused structurally**, not reported as `matched: false`.
   A false negative would send the caller down the wrong path.

### When to use it

Use it right after any action whose effect is not immediate: a launch, a save, a search, a dialog
that must open or close. Prefer it over `sleep` followed by another `get_window_state`: it costs
one call and no screenshot instead of one image round trip per step.

```ts
await click({ window, element_index: 4 });        // "Save"
const saved = await computer_use_wait_for({
  window, gone: "Saving...", timeout_ms: 10000,
});
if (!saved.matched) { /* still saving: observe and decide */ }
```

Only the Linux native helper serves this method. On the Windows backend the tool is not
registered, and the Linux P1 `linux` surface does not advertise it either.
