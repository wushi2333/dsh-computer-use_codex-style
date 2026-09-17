## Linux P1 API Reference (sky.window surface)

Use this as the supported Linux P1 Computer Use API surface exposed by `helper-linux` via the stdio JSONL protocol.

Linux P1 implements the lightweight **sky.window** 7-tool surface. Window targeting uses string-based app identifiers or window references (`app` or `linux-window:<id>`), rather than the structured Windows window2 `Window` objects.

```ts
import { sky } from "@oai/sky";

// Example workflow: list apps, inspect state, and interact
const apps = await sky.list_apps();
const state = await sky.get_app_state({ app: "gedit" });
await sky.click({ app: "gedit", x: 150, y: 80 });
await sky.type_text({ app: "gedit", text: "Hello from DeepSeek Harness on Linux" });

interface LinuxComputerUseClient {
  /**
   * List installed apps and running desktop applications that can be targeted.
   */
  list_apps(): Promise<Array<ListAppsApp>>;

  /**
   * Capture the current state (screenshot and AT-SPI accessibility text) for an app window.
   */
  get_app_state(input: GetAppStateInput): Promise<AppState>;

  /**
   * Capture a standalone screenshot for the specified app or window.
   */
  screenshot(input: ScreenshotInput): Promise<ScreenshotResult>;

  /**
   * Click at a specific coordinate within the target app window.
   */
  click(input: ClickInput): Promise<void>;

  /**
   * Scroll at a specific coordinate or within the target app window.
   */
  scroll(input: ScrollInput): Promise<void>;

  /**
   * Press a key or key combination in the target app window.
   */
  press_key(input: PressKeyInput): Promise<void>;

  /**
   * Type text into the currently focused input element of the target app window.
   */
  type_text(input: TypeTextInput): Promise<void>;

  target: "linux";
}

/**
 * App identifier passed to all targetable tool calls.
 * Can be:
 * - A canonical app identifier or desktop file ID (e.g. "gedit", "firefox", "org.gnome.Weather")
 * - A window-specific target string formatted as "linux-window:<id>" (e.g. "linux-window:54321")
 */
type AppIdentifier = string;

type ListAppsApp = {
  displayName?: string; // User-visible app name when available
  id: AppIdentifier; // Canonical app ID or "linux-window:<id>" to pass to subsequent calls
  isRunning?: boolean; // Whether the app currently appears to be running
  lastUsedDate?: string; // ISO 8601 timestamp for recent app usage when available
  useCount?: number; // Usage frequency signal when available
  windows?: Array<{ id: string | number; title?: string }>; // Optional open window information
};

type GetAppStateInput = {
  app: AppIdentifier; // App id, name, or "linux-window:<id>"
  disableDiff?: boolean; // Return full accessibility tree instead of diff when supported
};

type AppState = {
  app: AppIdentifier; // App identifier for the captured window
  screenshot: ScreenshotResult | null; // Captured screenshot image
  text?: string; // Accessibility text hierarchy from AT-SPI (empty if AT-SPI is unavailable)
};

type ScreenshotInput = {
  app: AppIdentifier; // App id, name, or "linux-window:<id>"
};

type ScreenshotResult = {
  url?: string; // Data URL (data:image/png;base64,...) or image payload (subject to helper-linux implementation)
  data?: string; // Base64 raw image bytes when returned directly
  width?: number; // Image width in logical/physical pixels
  height?: number; // Image height in logical/physical pixels
};

type MouseButton = "left" | "right" | "middle" | "l" | "r" | "m";

type ClickInput = {
  app: AppIdentifier; // App id, name, or "linux-window:<id>"
  click_count?: number; // Number of clicks to perform (default: 1; 2 for double-click)
  mouse_button?: MouseButton; // Mouse button to click (default: "left")
  x?: number; // X coordinate relative to the target window/app viewport
  y?: number; // Y coordinate relative to the target window/app viewport
};

type Direction = "up" | "down" | "left" | "right" | "u" | "d" | "l" | "r";

type ScrollInput = {
  app: AppIdentifier; // App id, name, or "linux-window:<id>"
  direction: Direction; // Direction to scroll
  pages?: number; // Number of scroll units or pages (default: 1)
  x?: number; // Optional X coordinate to scroll at
  y?: number; // Optional Y coordinate to scroll at
};

type PressKeyInput = {
  app: AppIdentifier; // App id, name, or "linux-window:<id>"
  key: string; // Key or `+`-separated key chord using keysym names (e.g. "Return", "Tab", "Escape", "Control_L+c", "Alt_L+F4")
};

type TypeTextInput = {
  app: AppIdentifier; // App id, name, or "linux-window:<id>"
  text: string; // UTF-8 text to type into the current focused element
};
```

---

## Comparison: Linux P1 (sky.window) vs. Windows (window2)

| Dimension | Linux P1 (sky.window) | Windows (window2 full surface) |
|---|---|---|
| **Tool count** | **7 tools** (`list_apps`, `get_app_state`, `screenshot`, `click`, `scroll`, `press_key`, `type_text`) | **13 tools** (includes `drag`, `set_value`, `perform_secondary_action`, `activate_window`, etc.) |
| **Window targeting** | String identifier: `app` (app name/id or `linux-window:<id>`) | Structured object: `window: { id: number, app: string, title?: string }` |
| **Element indexing** | **Not supported in P1** (all clicks & scrolls use window-relative `x`/`y` coordinates) | Supported via `element_index` derived from UIA accessibility tree |
| **Direct value replacement** | **Not supported** (use click to focus + keyboard shortcut Ctrl+A/Backspace + `type_text`) | Supported via `set_value({ element_index, value })` |
| **Drag & drop** | **Not supported in P1** | Supported via `drag({ from_x, from_y, to_x, to_y })` |
| **Overlay / Cancellation** | No graphical banner overlay; standard process cancellation | `CodexComputerUseCursorOverlay` with banner and physical Escape key interceptor |
| **Handle / Snapshot lifecycle** | Lightweight stateless / session-based process targeting | Win32 HWND rehydration (`get_window`) and UIA snapshot caching |

---

## Session & Environment Requirements (Linux)

1. **Display Server & Wayland / X11 Compatibility**:
   - Both **Wayland** and **X11** sessions are supported.
   - On Wayland, capture and input injection leverage **XDG Desktop Portal** (`org.freedesktop.portal.ScreenCast` and `org.freedesktop.portal.RemoteDesktop`).
2. **System Authorization Popups (Portal Prompt)**:
   - On Wayland desktop environments (GNOME, KDE Plasma, etc.), the **first invocation** of `screenshot` or input tools (`click`, `type_text`) may trigger a system permission modal asking the user to share the screen or allow remote control.
   - Instruct the user to grant permission when running for the first time in a desktop session.
3. **AT-SPI Accessibility Tree**:
   - `get_app_state` attempts to query the accessibility hierarchy through AT-SPI2 D-Bus interfaces.
   - If accessibility is disabled in the user environment, `text` will be omitted or empty. To enable AT-SPI on GNOME:
     ```bash
     gsettings set org.gnome.desktop.interface toolkit-accessibility true
     ```
   - When AT-SPI text is unavailable, agents should rely on visual screenshots and coordinate-based interactions (`screenshot` + `click({ x, y })`).
