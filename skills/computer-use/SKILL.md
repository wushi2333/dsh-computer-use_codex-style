---
name: computer-use
description: Drive Windows and Linux desktop apps from DeepSeek Harness with native UI automation tools. Use for clicking, typing, screenshots, and desktop app interaction. For Chromium tabs, load computer-use-browser first.
---

# Computer Use

Use this skill to automate the UI of desktop apps across Microsoft Windows and Linux.
On Windows, it uses SendInput, UI Automation, and Windows.Graphics.Capture (window2 13-tool surface).
On Linux (P1), it uses `helper-linux` exposing the 7-tool sky.window surface (via XDG Desktop Portal and AT-SPI).

If these tools are available, read this entire `SKILL.md` once before Windows automation work, before
saying Computer Use is unavailable, and before falling back to other Windows automation.

Start with the directions below. Read these bundled Markdown files relative to this `SKILL.md` when you
need the topic they cover:

- `references/guidance.md`: core runtime behaviour, target-window workflow, screenshot handling, and
  recovery guidance. You MUST read this before controlling Windows apps.
- `references/api.md`: the Windows window2 13-tool surface with every parameter, default, and doc comment.
- `references/api-linux.md`: the Linux P1 sky.window 7-tool surface (`list_apps`, `get_app_state`, `screenshot`, `click`, `scroll`, `press_key`, `type_text`) with string-based `app` and `linux-window:<id>` targeting.
- `references/api-linux-window2.md`: the Linux P2 window2 13-tool surface (Codex parity on X11, complete 13 methods, element indexing, overlay pill, synthetic cursor, and Wayland degradation mode).
- `references/confirmations.md`: you MUST read this before deciding whether a Windows UI action needs
  confirmation.
- `references/dsh-header.md`: the session contract and the non-negotiable Windows Automation Safety block
  (also always in your system prompt).

## Linux Platform Surface (P1 sky.window)

When running on Linux, Computer Use operates via `helper-linux`. It provides a focused **7-tool surface**:
`list_apps`, `get_app_state`, `screenshot`, `click`, `scroll`, `press_key`, `type_text`.

Key characteristics on Linux:
1. **Targeting by identifier**: Tools accept `app` as either a canonical application identifier (e.g. `"firefox"`, `"gedit"`) or a specific window target formatted as `"linux-window:<id>"`.
2. **Coordinate-based input (no element_index)**: All click and scroll actions operate on window-relative coordinates `{ x, y }`. Accessibility element indexes (`element_index`), `drag`, and direct `set_value` are Windows window2 capabilities not present in Linux P1.
3. **Session permissions (Portal)**: Under Wayland, the first call to `screenshot` or input tools will display an OS-level XDG Desktop Portal permission prompt. The user must grant screen capture / remote desktop access.
4. **AT-SPI accessibility**: `get_app_state` reads AT-SPI accessibility trees. If disabled in the desktop session, `text` will be omitted or empty, and actions should rely on visual screenshots.
5. **Electron applications (QQ, WeChat, VS Code, etc.)**: On a desktop with AT-SPI enabled, Electron apps usually DO expose usable trees (verified on QQ: hundreds of named nodes) — always TRY the tree first with `include_text: true`. Never conclude "this app has no accessibility tree" from a call that didn't request text, and expect many unnamed `panel` nodes (normal for Electron; look for named buttons/statics). Only when the requested tree genuinely comes back empty is the visual screenshot + coordinate-click path the expected fallback.

For full type definitions and examples, see `references/api-linux.md`.

## If the tools are missing

Loading this skill does **not** register the tools. They come from the **Computer Use** agent preset
(`dsh-computer-use/tool`), not from `standard`.

If `list_windows` / `launch_app` / `computer_use_health` are not in your function list:

1. Stop. Do **not** drive the desktop via `pwsh`, `Start-Process`, SendKeys, or screenshot scripts.
2. Tell the user in their language: start a **new** session and choose preset **Computer Use** (not standard). Then repeat the request.
3. Do not invent tool names or retry unknown-tool calls.

## Initialize

Call `computer_use_health` first when you need the backend, the allow list, or whether browser tools
are unlocked. Then select exactly one target window:

1. **Enumerate before acting (先枚举再行动)**: Always call `list_windows` (and/or `list_apps`) before interacting with any application.
   - If the target app is already running, pick its window and call `activate_window({window})` to bring it forward and reuse the existing instance. **Never launch a second instance or restart an already-running app.**
2. Pick **exactly one** returned window object. Never invent `app` / `id`, and never reconstruct a window
   from guessed fields. `get_window({id, app})` rehydrates a binding you already hold.
3. **Launch only when absent**: Call `launch_app({app})` only after enumeration confirms that no open window exists for the app. Then refresh `list_apps` / `list_windows` and select a returned window.
   - **Never launch GUI applications via bash/shell commands** (e.g. `qq &`, `nohup ...`, `code`): running desktop apps through the shell bypasses window tracking and risks spawning duplicate instances or corrupted login states.
4. `activate_window({window})`, then `get_window_state({window})`.

`get_window_state({window})` defaults to screenshot on and `accessibility: null`. Set
`include_text: true` when you need element indexes, and request both only when the next decision needs both.

## Act and refresh

Use a two-cell loop for state-derived inputs: observe and stop, inspect the result, then perform exactly
one action and refresh immediately. Element indexes, screenshot IDs, and coordinates are valid only for
the observation that produced them; interleaving or retry requires re-observation.

```
get_window_state({window, include_text: true, include_screenshot: false})   // cell 1: read accessibility.tree
click({window, element_index: 12})                                          // cell 2: one action
get_window_state({window, include_screenshot: true, include_text: true})     // cell 2: refresh
```

Coordinate path: pass the matching `screenshotId` together with `x`/`y` (`from_x`/`from_y` for `drag`).
Use window-relative screenshot coordinates when accessibility elements are unavailable.

Typing: observe focus first and read `accessibility.focused_element`, then `type_text({window, text})` and
refresh. Use `press_key` for Return/Tab/arrows/Escape and keyboard chords instead of embedding control
characters in a typed string.

`batch_actions` runs a list of actions and then one `get_window_state` in a single call. Use it only for
actions that do **not** depend on `element_index` from the current observation, because the first action
invalidates every index: batch coordinate actions that carry the same `screenshotId`, or keyboard actions
(`press_key` / `type_text`) that target the current focus. For an action that consumes an `element_index`,
perform that one action and refresh, as described above.

## Reading screenshots

Screenshots returned by `get_window_state` are displayed automatically. Inspect them directly and use the
returned screenshot ID for coordinate actions. Do not decode, save, print, or re-emit screenshot payloads
again solely for inspection.

## Guidelines

- **Locating a specific target (查找特定对象)**: When asked to find a specific object inside an application (such as a contact in QQ/WeChat, a file in a file manager, or a setting), read [Efficiency tactics (高效战术)](#efficiency-tactics-高效战术) first. Never default to visual scrolling when search mechanisms exist.
- **Enumerate before acting**: Always check `list_windows` before attempting to open any app. If the target window already exists, activate and reuse it via `activate_window`; never attempt to launch it again.
- **Never launch GUI apps through the shell**: Do not use bash/shell commands to launch GUI applications; use `launch_app` only when `list_windows` confirms the app is not already running.
- **Electron applications (e.g. QQ, WeChat, VS Code)**: try the accessibility tree first (`include_text: true`) — it is often richer than expected on AT-SPI-enabled desktops. Visual screenshot inspection + coordinate clicking (`click({window, screenshotId, x, y})`) is the fallback for when the tree is genuinely empty, not the default assumption.
- Treat `get_window_state` as an expensive point-in-time snapshot. Batch related inputs, then capture a new state when you need to verify progress or when focus, layout, modality, or element indexes may have changed.
- Element indexes are valid only for the accessibility state that produced them. Refresh accessibility state after any action that may change the visible element tree.
- By default `get_window_state({window})` captures and displays a screenshot and returns `accessibility: null`. This is the best default for desktop apps with weak accessibility trees.
- If an input call reports that the point is over a non-target window, call `activate_window({window: state.window})`, refresh screenshot-backed state, and retry the intended input once with the refreshed `state.window`.
- If you expect a modal in the target app but `get_window_state` does not show it, call `list_windows()` to find the modal or owned secondary window, then capture that returned window.
- Every `get_window_state` returns the complete accessibility tree for that moment. Re-read it instead of assuming the previous tree still holds, and do not repeat the call without an intervening action or a real reason to expect a change.
- If state capture or window activation fails, stop using prior coordinates or element indexes. Refresh the app/window selection and retry once; report the exact error if recovery fails.
- If a stored window stops working, recover with `list_windows()`, `get_window({id, app})`, `activate_window({window})`, then `get_window_state({window})`.
- Prefer X Window System keysym-style names for key input, especially `KP_0` through `KP_9` for apps that distinguish numpad keys. Aliases such as `Control`, `Ctrl`, `Alt`, `Shift`, `period`, `greater`, `Numpad_0`, `Numpad_Enter` are accepted; for shifted punctuation shortcuts include `Shift`, e.g. `Control_L+Shift_L+period`.
- `scroll` takes window-relative coordinates plus `scrollX`/`scrollY` (negative `scrollY` scrolls up). If a pane needs focus, click it first with coordinates, then scroll from inside that pane.
- Use keyboard navigation when it is faster than hunting UI pixels.
- For text entry into a document, slide, sheet, editor, or canvas, click a stable point inside the editable work surface, refresh to verify focus, then type.
- For drawing, handwriting, canvas, or 3D viewport manipulation, use `drag` strokes directly on the canvas.
- For browser work prefer the `computer-use-browser` skill over pixels.
- **Backup observation channel (备选观察通道)**: the desktop OCR/vision tools (e.g. `mcp__nuphus-mcp__desktop_perceive` / `desktop_vision`) may be used as a SECONDARY way to read the screen when the `get_window_state` screenshot channel is malfunctioning (empty or blank images), or when tiny elements in an Electron app are unreadable in a downscaled screenshot. Reading only — every action still goes through Computer Use tools, and coordinate actions need the `screenshotId` from a real CU observation, so re-observe with `get_window_state` before clicking. A broken screenshot channel is a bug: report it instead of settling into OCR mode.

## Efficiency tactics (高效战术)

When automating tasks that involve locating a specific object or driving desktop apps (e.g. QQ, WeChat, file managers, settings), apply these efficiency tactics instead of blindly scanning or clicking:

### Search & text input (搜索与输入)
- **Search before scroll (先搜索再滚动)**: When the target is searchable, click the search box, `type_text` the name or keyword, and press `Return`. Screen-by-screen visual scanning is strictly prohibited when a search field exists.
- **App search shortcuts (应用级搜索快捷键)**: In desktop apps like QQ/WeChat, press `Ctrl+F` directly to focus the search box instead of hunting for search icons across UI pixels.
- **Clear-before-type (键入前清空残留)**: Send `Ctrl+A` followed by `Backspace` before typing into any search or text field to clear stale input.
- **First-result Return (回车直达首选结果)**: Press `Return` immediately after typing a search query to activate the first match without an extra observation round-trip to pick from dropdowns.
- **Clipboard paste for CJK/long text (剪贴板粘贴长文本与CJK)**: For Chinese, emoji, or strings >20 characters, prefer writing to the clipboard and sending `Ctrl+V` to prevent IME composition desync.

### Keyboard & micro-targets (键盘导航与微小目标)
- **Keyboard beats pixels (快捷键优于像素查找)**: Always prioritize application hotkeys and standard navigation (`Ctrl+F`, `Ctrl+S`, `Tab`, arrows) over hunting UI coordinates with clicks.
- **Sub-16px targets via keyboard (微小目标用键盘操作)**: For micro-targets (<16px, e.g. close buttons, expand chevrons), use `Tab` / `Shift+Tab` to move focus and `Space` / `Return` to trigger rather than pixel clicking.
- **Type-ahead list navigation (焦点前缀键入直达)**: In list views (contacts, file pickers), focus the list and type initial characters to jump directly to matching entries instead of scrolling visually.

### Scrolling discipline (滚动纪律)
- **Scroll only as fallback (仅在无搜索手段时回退滚动)**: Fall back to scrolling only when no search, filter, or keyboard navigation exists; use large strides (~pane height) and stop immediately once visible.
- **Pointer anchoring before scroll (滚动前指针锚定)**: Move the mouse pointer inside the target container boundary before issuing `scroll` so wheel events route to the intended pane.
- **Scroll boundary detection (滚动边界探测)**: Compare consecutive screenshots; when container content stops changing across scrolls, the boundary is reached—halt scrolling immediately.

### Cadence & batching (执行节奏与批处理)
- **Text-tree observation first (文本树观察优先)**: When the AT-SPI accessibility tree is available (non-empty `accessibility` / `text` in `get_window_state`), prioritize text-tree observation (`include_text: true, include_screenshot: false` — text is opt-in, without `include_text` the tree comes back null) and locate targets via `element_index`. Capture screenshots only when the text tree is absent or ambiguous, or when visual verification is required (e.g. verifying the chat header before sending a message). Text-only observation round-trips are an order of magnitude smaller and faster than screenshots, making this the primary speedup technique.
- **Batch deterministic sequences (全确定序列单回合批处理)**: Combine fully deterministic sequences (e.g. click search -> clear -> paste -> Return) into one `batch_actions` call; capture screenshots only at visual decision boundaries.
- **Settle async rendering (异步渲染等待沉淀)**: After triggering network queries or async UI updates (e.g. Electron search results), wait 0.5–1.5s (`waitMs` in `batch_actions`) before the next observation.

### Verification & IM safety (视觉核验与 IM 纪律)
- **Visual diff verification (行动后视觉差分核实)**: Confirm expected visual changes (cursor focus, tab selection, modal dismissal) in the post-action screenshot; if the screen did not change, the action failed—never assume success.
- **Tree-diff verification (树差分核验)**: When the accessibility tree is available, verify action outcomes by diffing two text-tree observations instead of capturing a screenshot — which nodes appeared/changed/disappeared is cheaper and less ambiguous than comparing pixels. Reserve screenshot verification for visual-only signals (colors, images, layout) and the IM recipient check.
- **Wait, don't poll (等待而非轮询)**: When waiting for a UI state (a window opening, search results loading, a contact's chat appearing), prefer the helper-side `computer_use_wait_for` primitive (Linux; waits up to 20s for text/element to appear or disappear without model round-trips) or a single settled re-observation after a `waitMs` pause over repeated screenshot polling — every poll is a model round-trip with an image attached.
- **IM send discipline (IM 发送与换行纪律)**: In IM clients (QQ/WeChat), `Return` sends the message while `Shift+Return` inserts a line break.
- **IM recipient double-check (IM 发送前双重核对)**: Electron IM trees can be partial, so before sending always verify the active chat header matches the intended recipient — via the tree when it names the header, otherwise a screenshot.

### Recovery & convergence (脱困与收敛)
- **Escape from traps (Escape 键脱困)**: Press `Escape` first whenever unexpected popups, autocomplete dropdowns, or context menus trap keyboard focus.
- **Loop-breaking circuit breaker (防死循环断路器)**: If consecutive screenshots show no progress, do not repeat the same call or coordinates; immediately switch modalities (click -> keyboard, search -> enumeration).
- **Fast convergence on absence (快速收敛不可行)**: If a target remains unfound after one focused search and one bounded scroll, conclude it does not exist and report back honestly—never click randomly or hallucinate.

## Reading the accessibility tree

The tree is line-oriented. Element lines follow the helper's grammar:

`<TABs><index> <role>[(<state>)] [<name>][<suffixes>]`

- exactly one TAB per depth level, and the root line already carries one TAB (its children two, ...);
- the element index is **bare** (`12 button OK`), never bracketed or quoted;
- the role is the element's localized UIA control type, printed as the helper reports it. With the
  `official` annotation profile (`DSH_CU_AX_ANNOTATIONS=official`) its spelling and case are preserved
  verbatim (`SplitButton`, and even a trailing space); the default profile trims and lower-cases it
  (`splitbutton`);
- the primary state list, when present, is parenthesised and sits **between the role and the name**:
  `11 SplitButton (disabled) 无法撤消 ID: Undo`, `1 窗格 (disabled) DropShadowTop`. It never trails the
  name;
- the name is unquoted and omitted entirely when empty, so a title bar renders as `4 标题栏`. The default
  profile preserves the name's internal spacing; the `official` profile collapses runs of whitespace to
  single spaces;
- **no bounding box is printed**; address elements by `element_index`, never by coordinates parsed from
  the tree.

Optional suffixes follow the name, separated by single spaces, in this order: `Description: ...`;
`Value: ...`; toggle states (`off` / `on` / `indeterminate`) and other element states;
`Secondary Actions: ...`; `ID: ...`; and, on a node whose subtree was cut off, a trailing
`(truncated: <budget>, omitted <n> children)`.

The header line is `Window: "<title>", App: <app>.`. The tail may add `The focused UI element is <line>.`,
a `Selected:` list, `Selected text:` / `Document text:` fenced blocks, and the selection note.

## Recommendations

- Prefer `element_index` over guessed coordinates whenever an accessibility element is available.
- Coordinates are window-relative **logical** pixels; a screenshot's `originX`/`originY` are **screen** coordinates. Do not mix them.
- Do not run `pwsh`, OCR, or PrintWindow to \"see\" the UI: the screenshot is an image part you can already read.

## Experience notes (advisory)

Earlier tasks on this machine leave short notes behind, readable with `computer_use_experience`.
When the first observation of an app matches a stored note, that tool result carries an
`Experience notes` text block: read it before choosing an approach.

- Notes are **reference only**. Software updates, DPI or configuration changes can invalidate them;
  a note never blocks, replaces or overrides a call, and the current observation always wins.
- `computer_use_experience({action: "list", app: "blender.exe"})` before driving an app you have not
  driven on this machine.
- Record one short note when a task ends with an app-specific problem you diagnosed or worked around:
  `computer_use_experience({action: "record", app, symptom, context, cause, workaround, outcome, tags})`.
  Record what is reproducible and actionable; a one-off typo or a generic error is not worth a note.
  Never put credentials, typed text or document contents in a note.
- If a stored note no longer applies, say so and retire it:
  `computer_use_experience({action: "update", id, stale: true})`, or point it at the note that
  replaces it with `supersededBy`.

## Approvals

`launch_app` and audio recording may pause for the harness approval UI. Approval is per app and is remembered
when approved persistently. In a session whose permission preset grants full access (or whose approval policy
never prompts) the app gate is granted without asking: call the tool instead of refusing it. If the helper reports a non-empty allow list, only those app ids can be observed
or driven; ask the user to add an app to the HOST plugin `allowedApps` config rather than bypassing it.

## Browser

Browser `tab_*` tools are skill-gated so they do not flood the tool catalog. Load the `computer-use-browser`
skill (`skill` tool with name `computer-use-browser`) before any `tab_*` / `browser_*` work. Until it loads,
those tools are not registered.

## Never

- Do not use the Windows key or any shortcut involving it.
- Do not automate terminals, password managers, security tools, or authentication dialogs.
- Do not OCR or screenshot via Shell. Vision is the `get_window_state` image.
- Do not spawn `codex-computer-use.exe`: Codex is not required and must not be used.
- Do not edit or delete shipped DeepSeek Harness presets (`standard`, `cordis`, `minimal`, `ptc`). User presets live under `$DSH_HOME/.agent-presets/`.
- Do not reconstruct window handles after they may have closed; list again.
- Do not launch or restart an app without checking `list_windows` first; always activate and reuse an existing window if present.
- Do not launch GUI applications via bash/shell/terminal commands; use `launch_app` only after confirming no window exists.

Your system prompt carries the always-on session contract, the two-cell loop, recovery, and the complete
Windows Automation Safety block. The three reference documents above hold everything else — read them from
the skill directory rather than guessing.
