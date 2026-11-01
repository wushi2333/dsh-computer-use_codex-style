---
name: computer-use
description: Drive Windows desktop apps from DeepSeek Harness with Codex-parity window2 tools. Use for clicking, typing, screenshots, and native UI automation. For Chromium tabs, load computer-use-browser first.
---

# Computer Use

Use this skill to automate the UI of Microsoft Windows apps. It uses SendInput, UI Automation, and
Windows.Graphics.Capture screenshots that work even when windows are occluded.

If these tools are available, read this entire `SKILL.md` once before Windows automation work, before
saying Computer Use is unavailable, and before falling back to other Windows automation.

Start with the directions below. Read these bundled Markdown files relative to this `SKILL.md` when you
need the topic they cover:

- `references/guidance.md`: core runtime behaviour, target-window workflow, screenshot handling, and
  recovery guidance. You MUST read this before controlling Windows apps.
- `references/api.md`: the full tool surface with every parameter, default and doc comment. Read this when
  you need a signature or an object shape.
- `references/confirmations.md`: you MUST read this before deciding whether a Windows UI action needs
  confirmation.
- `references/dsh-header.md`: the session contract and the non-negotiable Windows Automation Safety block
  (also always in your system prompt).

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

1. `list_apps` (installed apps plus their open targetable windows) and/or `list_windows` (currently open windows).
2. Pick **exactly one** returned window object. Never invent `app` / `id`, and never reconstruct a window
   from guessed fields. `get_window({id, app})` rehydrates a binding you already hold.
3. If the target app has no open window, call `launch_app({app})`, refresh `list_apps`, then select a returned window.
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

- Prefer `element_index` or `text` over guessed coordinates whenever an accessibility element is available.
- `click({window, text: "按钮名称"})` will automatically match the target element from the latest accessibility tree.
- Coordinates are window-relative **logical** pixels; a screenshot's `originX`/`originY` are **screen** coordinates. Do not mix them.
- Do not run `pwsh`, OCR, or PrintWindow to \"see\" the UI: the screenshot is an image part you can already read.

## Experience & Machine-Local Knowledge

Machine-specific quirks, pitfalls, and workaround guidance belong in dedicated Skills
(e.g. `computer-use-local` or app-specific skills) tracked in version control, rather than
ephemeral runtime experience stores.

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

Your system prompt carries the always-on session contract, the two-cell loop, recovery, and the complete
Windows Automation Safety block. The three reference documents above hold everything else — read them from
the skill directory rather than guessing.
