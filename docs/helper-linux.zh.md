# helper-linux 架构与集成指南

`helper-linux` 是 DeepSeek Harness 插件 `dsh-computer-use` 在 Linux 平台上的原生桌面自动化辅助进程（daemon），负责打通 DSH Agent 与 Linux 桌面显示服务及无障碍服务。

## 1. 架构定位

- **上游与来源**：Fork 自 MIT 协议开源项目 `ilysenko/codex-desktop-linux` 的 `computer-use-linux` crate。
- **进程模型**：作为独立的子进程由插件 Host / Sidecar 拉起。
- **通信协议**：采用标准 **stdio JSONL**（逐行 JSON 数据流）协议：
  - 请求：`{"id": 1, "method": "list_apps", "params": {}}`
  - 响应：`{"id": 1, "ok": true, "result": [...]}` 或 `{"id": 1, "ok": false, "error": "..."}`

## 2. 构建 helper-linux

helper-linux 源码位于仓库顶层 `helper-linux/` 目录。

```bash
# 构建 Release 版本（推荐生产使用）
cargo build -p helper-linux --release

# 或构建 Debug 版本（本地联调）
cargo build -p helper-linux
```

### 产物候选路径

构建产物默认生成在：
- `helper-linux/target/release/dsh-computer-use`
- `helper-linux/target/debug/dsh-computer-use`

插件运行时通过 `src/paths.js` 动态查找可用二进制候选：
1. 环境变量：`DSH_COMPUTER_USE_HELPER`（若配置则最高优先）
2. 本地 Cargo 构建产物（`helper-linux/target/release/dsh-computer-use` 等）
3. 预编译发布包产物（如 `helper-rs/bin/linux-<arch>/dsh-computer-use`）
4. 工作区根目录兜底（`./dsh-computer-use`）

*(具体候选路径与发现策略以 `src/paths.js` 实现为准)*

## 3. P1 工具能力面（sky.window 风格 7 工具）

P1 阶段 `helper-linux` 对外暴露 7 个核心工具：

| 工具名 | 用途 | 核心参数 |
|---|---|---|
| `list_apps` | 列出可启动应用及当前可交互的窗口 | `{}` |
| `get_app_state` | 获取指定窗口的截图与 AT-SPI 无障碍文本结构 | `{ app, disableDiff? }` |
| `screenshot` | 单独截取指定窗口/应用的屏幕画面 | `{ app }` |
| `click` | 在窗口相对坐标处执行点击 | `{ app, x, y, click_count?, mouse_button? }` |
| `scroll` | 在指定坐标或窗口视口内滚动 | `{ app, direction, pages?, x?, y? }` |
| `press_key` | 发送单个按键或组合快捷键（keysym 格式） | `{ app, key }` |
| `type_text` | 向当前获焦元素键入 UTF-8 文本 | `{ app, text }` |

### 定位语义（Targeting）
所有操作通过字符串标识符 `app` 指定目标：
- 应用名 / Desktop Entry ID（例如 `"gedit"`, `"firefox"`）
- 显式窗口标识符字符串（格式为 `"linux-window:<id>"`，例如 `"linux-window:12345"`）

## 4. 运行环境与系统权限要求

1. **Wayland 与 X11 支持**：
   - Wayland 环境下，截图与远程输入注入基于 **XDG Desktop Portal**（`org.freedesktop.portal.ScreenCast` 与 `org.freedesktop.portal.RemoteDesktop`）。
   - X11 环境下使用原生 X11 协议 / XTest 扩展。
2. **Portal 首次交互授权弹窗**：
   - 在 Wayland 桌面（如 GNOME, KDE Plasma）中，**首次调用** `screenshot` 或输入工具时，系统桌面会弹出原生授权确认框（询问是否允许屏幕共享与远程控制）。
   - 首次运行时需提示用户在图形界面中点击“允许/共享”。若用户拒绝，调用将返回权限拒绝错误。
3. **AT-SPI 无障碍服务要求（强烈建议开启）**：
   - `get_app_state` / window2 的 `get_window_state` 通过 AT-SPI2 D-Bus 接口提取应用控件树；开启后元素索引（element_index）定位、文本抽取、`set_value` 全部可用，操控明显更快更准。不开也能用，但 Agent 只能退化为"截图 + 坐标点击"，慢且依赖视觉模型。
   - 开启方法（按桌面环境）：
     - **KDE Plasma**：「系统设置 → 辅助功能」中启用屏幕阅读器支持（会拉起 AT-SPI 总线）；Qt 应用若仍不暴露控件树，设置环境变量 `QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1`（可写入 `~/.config/plasma-workspace/env/` 下的 .sh）。
     - **GNOME**：`gsettings set org.gnome.desktop.interface toolkit-accessibility true`。
     - 确保已安装 `at-spi2-core`（绝大多数发行版默认自带）。
   - Electron 应用（QQ、VS Code 等）即使总线已开也常不上报控件树，需带 `--force-renderer-accessibility` 启动才会暴露；不上报时按降级策略走视觉路径，属正常现象。

## 5. 能力边界与降级策略

- **P1 与 P2 window2 全表面的边界**：
  - P1 专注于 7 个核心 sky.window 工具的稳定落地。
  - Windows window2 中的元素索引定位（`element_index`）、拖拽（`drag`）、直接赋值（`set_value`）、全屏遮罩/物理 Esc 拦截 overlay、Win32 HWND 句柄快照缓存等高级能力不在 P1 范围内，规划在 P2 落地。
- **无障碍服务降级**：
  - 若系统未开启 AT-SPI 或目标应用不支持无障碍协议，`get_app_state` 中的 `text` 为空，但截图依然正常生成。Agent 应平滑降级为根据视觉截图执行坐标点击。
- **Portal 权限拒绝处理**：
  - 用户拒绝 Portal 授权时，helper 快速失败并返回明晰错误信息，避免静默卡死。

### 进阶：Electron 应用的 CDP 通道（参考）

- **原理与接口暴露**：基于 Electron 的桌面应用（如 QQ、VS Code 等）若在启动时附加 `--remote-debugging-port=<端口>` 参数，其渲染界面本质上作为 Chromium 页面暴露标准 CDP（Chrome DevTools Protocol）调试端口与 DOM 接口。
- **配合浏览器通道操控**：此时可配合本插件的浏览器通道（加载 `computer-use-browser` 技能）进行 DOM 级精准操控，跳过视觉截图传输与像素坐标估算，操作速度与稳定性提升一个量级。
- **定位为可选参考路径**：此方案定位为**可选的高级参考路径**，不作为本插件的默认实现。它需要用户自行携带调试参数重启目标应用；本插件默认主路径始终为原生桌面 Computer Use 工具。
- **安全提醒**：应用带远程调试端口启动后暴露面变大（本机任意进程均可通过该端口完全操控应用界面与上下文），**仅限在本机可信环境中使用**，切勿在生产环境或开放网络中暴露该端口。

### 进阶：截图采集性能参考

- **本插件的默认路径已是零 fork 高效采集**：X11 下 helper 直接通过 XShm 共享内存抓帧（C 级 API，进程内完成），不开新进程、不写临时文件，通常已经比外部截图工具更快。绝大多数用户无需任何额外操作。
- **X11 外部参考**：若你在插件之外需要自己截图（脚本、调试），优先用基于 XGetImage C 绑定的库（如 Python 的 `mss`、`python-xlib`），避免反复 fork `import`/`scrot` 这类外部进程（每次启动进程的开销远大于抓帧本身）。
- **Wayland 参考**：Wayland 下优先基于 DMA-BUF / PipeWire 的方案（本插件走 XDG Desktop Portal），或临时用 `grim -t jpeg -q 75` 直接输出中低质量 JPEG——JPEG 编码比 PNG 快且小，对"画面只要看得清"的场景足够。
- **真正的耗时大头不在抓帧**：实测一次模型往返（带图思考）约 9 秒，而抓帧只有几十毫秒。想提速请优先用 AT-SPI 文本树观察与 `wait_for` 等待原语（见上文），而不是抠截图毫秒数。

## 6. P2 进展：window2 全表面支持（X11 Parity）

P2 阶段将 `helper-linux` 从 P1 的 7 工具 sky.window 子集扩展至与 Windows 对齐的 **13 工具 window2 全表面**（Codex Parity）。

### P2 核心能力
1. **13 工具完整表面**：全面覆盖 `list_windows`、`get_window`、`list_apps`、`launch_app`、`get_window_state`、`click`、`press_key`、`type_text`、`scroll`、`set_value`、`drag`、`perform_secondary_action`、`activate_window`。
2. **纯 Rust x11rb 路线**：全部基于纯 Rust `x11rb-protocol` Wire 协议扩展（XTest、XShm、XFixes、XInput2），无需宿主机安装 C 语言 X11 开发包（如 `libxcb-dev`）。
3. **结构化 Window 对象定位**：全面对齐 Windows 的 `Window { id, app, title }` 句柄体系，基于 EWMH 实现稳定窗口枚举与再水化。
4. **AT-SPI 元素索引支持**：提取结构化无障碍控件树，提供基于 1-based 序号的元素定位（`element_index`），支持元素级点击、`set_value` 赋值及次级操作。
5. **体验与安全保障层**：
   - 基于 Override-Redirect 窗口的自动化操作药丸提示（Overlay Pill）；
   - 基于 XFixes 的原生光标隐藏与合成光标跟随绘制；
   - 基于 XInput2 原始事件的新鲜度租约检测（防止并发冲突）；
   - 基于 X11 全局 keygrab 的物理 `Escape` 按键即时中断。
6. **分级显示服务架构**：
   - **X11 完整模式**：提供 13 方法全部能力，支持 MIT-SHM 遮挡窗口抓取、药丸遮罩、合成光标及全局 Esc 捕获；
   - **Wayland 降级模式**：降级为基于 XDG Desktop Portal 与 AT-SPI 运行，药丸与全局快捷键平滑退避，通过 `computer_use_health` 透明上报降级状态。

