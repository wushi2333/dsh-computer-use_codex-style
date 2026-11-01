# dsh-computer-use — 架构说明（给后续 agent / 维护者）

> 这份文档是**读代码前先读**的入口。它描述的是"为什么长这样"，不是逐行 API——
> 单个工具的入参/出参见 `README.md` 与 `README.zh-CN.md` 的工具表。

---

## 1. 它是什么

一个 DeepSeek Harness 插件：把"操作本机 Windows 桌面"变成一组模型可调用的工具。
模型看到的是 `computer_use_*`（本插件自定义）以及裸名的 window2 官方方法（`click` /
`get_window_state` / `list_windows` …）。真正干活的是两个外部进程：

- **Rust helper**（`helper-rs/`）：Win32 / UIA / WGC / 输入注入的原生实现，通过
  `sidecar.js` 的 JSON-RPC 桥暴露 `ctx.dshComputerUse.call(name, args)`。
- **PowerShell 脚本**（`scripts/*.ps1`）：零散但必要的补充手段（`uia-post` 的
  PostMessage 点击、`uiahost` 常驻 UIA 服务、`gdi-capture` 抓图、`uia-swipe` 插值手势）。

---

## 2. 目录与职责

```
plugins/dsh-computer-use/
├─ src/
│  ├─ index.js           (520L) 插件入口：Service、Config(Schema)、apply()、turn 生命周期
│  ├─ tool.js            (2919L) ★上帝文件：装配 + 全部本机工具 + 捕获/UIA/探测/错误
│  ├─ sidecar.js         (794L) 与 Rust helper 的 JSON-RPC 子进程桥
│  ├─ prompt.js          (159L) 从 helper-rs/assets/prompts 读系统提示词资产（单一源）
│  ├─ uiahost-client.js  (113L) 常驻 PowerShell UIA 服务的客户端（换行 JSON 协议）
│  └─ paths.js           (63L)  路径解析
├─ helper-rs/            Rust 原生 helper（~19k L）：uia.rs / capture.rs / input.rs / …
├─ scripts/              PowerShell 辅助脚本（13 个 .ps1）
├─ skills/               随插件分发的技能文档（computer-use 等）
├─ tests/                49 个测试文件（JS node:test + Python）
├─ parity/               与官方 @oai/sky 的对照证据（不是运行时代码）
└─ computer_use/         官方 Python 侧遗留 + prompt 资产副本
```

**代码体量分布（2026-11 实测）**：JS 4568 行，其中 `tool.js` 占 **64%**；Rust ~19k 行；
`uia.rs` 单文件 4277 行。**测试 ~5000 行**——测试投入是扎实的。

---

## 3. 工具面分层（关键：不要把层次搞混）

| 层 | 内容 | 谁定义 | 能否改 |
| --- | --- | --- | --- |
| **官方 window2 核心 13** | `list_windows` `get_window` `list_apps` `launch_app` `get_window_state` `click` `press_key` `type_text` `scroll` `set_value` `drag` `perform_secondary_action` `activate_window` | `helper-rs/src/tools.rs` 的 `WINDOW2_CORE` | **契约固定，不改**（与 `@oai/sky` 对齐） |
| **harness 扩展** | `batch_actions` `fill_form` `scroll_element` `session_note` `session_state` `end_turn` `diagnostic_state`（+ 可选 audio） | `helper-rs/src/tools.rs` | 改要走 `harness` surface |
| **本机自定义** | `health` `wait` `wait_until` `scroll_to` `documents` `uia_act` | `src/tool.js` | 自由增删 |
| **browser**（延迟） | `tab_*` 等 | Python sidecar | 需加载 `computer-use-browser` 技能 |

> 分层是**硬边界**：`tools_for_surface()` 按 surface 名返回不同集合，`WINDOW2_CORE`
> 保证官方 13 方法一个不多一个不少。新增本机工具**不得**混进官方 surface。

---

## 4. 三条执行通道（P0 正在统一它们）

同一个"动作"历史上走了三条路，目前**没有统一抽象**：

1. `ctx.dshComputerUse.call(name, args)` → Rust helper（主力，33 处调用点）
2. `execFileSync('pwsh', ['-File', script, ...])` → PowerShell 脚本（9 处）
3. `uiahost-client.js` → 常驻 PowerShell 进程（换行 JSON：`ping/tree/read/set/post/act/dialog`）

**降级链**（存在但散落）：
- 观察：WGC 抓帧 → GDI `PrintWindow` → 陈旧帧标记（`observeWithLadder`）
- 翻页：滚轮 → `Page_Down`/`Right`/`Next` → 插值 swipe，用三信号判定是否真翻动
- 死树：UIA 树全匿名容器 → 转 `.NET ControlView` fallback → 标记 `semantic.dead`

---

## 5. 关键硬事实（血泪教训，务必保留）

这些是**实测**结论，不是理论。改动相关区域前先读：

1. **WMPF（微信小程序）页面不吃 `SendInput` 鼠标，也不吃 `InjectTouchInput`**（`err=87`，
   本机 `IsTouchWindow=false`）。唯一有效的坐标点击是 `scripts/uia-post.ps1` 的
   `PostMessage` 到渲染子窗口（`lParam = x|y<<16`）。
2. **`CopyFromScreen` 在 DSH 宿主进程内不可用**（连固定屏幕坐标都报"句柄无效"）——
   宿主没有交互桌面访问权。所以**任何依赖屏幕像素读回**的能力（如"读复选框是否勾选"）
   在插件宿主内都是假的，只能在用户的真实桌面会话里跑。这条导致 `read_options` /
   `set_options` 被删除。
3. **C# `net8.0-windows + UseWPF` 的 `FindAll(Descendants)` 对 WeChat 窗口要 106 秒**，
   而 PowerShell 同一调用只用 42ms。所以常驻 UIA 服务用 **PowerShell**（`uiahost.ps1`），
   不用 C#。慢的从来不是 UIA，是"每次新起进程"（1.5–2s）。
4. **WGC 只在窗口重绘时给帧**；静态页（考试表单、设置对话框）会 `window capture timed out`，
   必须走 GDI / UIA 兜底。
5. **裸 `Text` 节点的 `ok` 只代表"点击发出去了"**，页面可能忽略——`uia_act` 的 `verify`
   （动作前后页面 diff）和 `checkDialog`（两步确认探测）就是为此存在。

---

## 6. 架构债与 P0 进展

| # | 问题 | 状态 |
| --- | --- | --- |
| 1 | `tool.js` 是上帝文件 | **部分解决**：3052 → 2629 行，拆出 ui-tree/capture/probing/errors |
| 2 | 三套执行器无统一 Driver 接口 | **已解决**：`src/drivers/index.js` 为唯一门面 |
| 3 | 能力探测是"事后"的 | **已解决**：`src/capabilities.js` + `health.capabilities` |
| 4 | PowerShell 脚本是二等公民 | 未做（P1：contract-isation / 迁 Rust） |
| 5 | 错误话术耦合会话状态 | **已拆出** `src/errors/index.js`（仍读写 state，但集中一处） |

### P0 已完成（逐提交，行为不变，测试全绿）
- `src/capabilities.js`：`probeCapabilities()` 事前探测 uia/screenCap/psHelper + `requireCapability()` 门禁
- `src/drivers/index.js`：`helper.call` / `runScript` / `runPsCommand` / `requireCapability` / `uia.*`
- `src/ui-tree/index.js`：`parseTreeLine/classify/compress/deadTreeSignal/formatUiaFallback`
- `src/probing/index.js`：`collectDocumentUrls` / `parsePageIndicator`
- `src/capture/frames.js`：`parseJsonLine` / `frameObservation` / `rememberFrame` / `recallFrame`
- `src/errors/index.js`：`captureGuidance` / `noteWedge` / `isWedged` / `friendlyActionError`
- 以上均由 `tool.js` re-export，现有 import 路径不变。

### P0 有意**未**做：`tools/`（每个工具一文件）与降级链搬家
原因：6 个 `*Tool(...)` 工厂都反向依赖 `tool.js` 内的 `observeWithLadder` / `uiaTreeFallback`
/ `runScrollTo` / `WIN_PARAM` / `OUTPUT_SCHEMA`；`observeWithLadder`/`captureFallback` 等降级链
又反向依赖 `ui-tree` / `probing` / `cachedCdpProbe`。硬搬会形成 **capture→tool.js→capture** 这类
循环依赖。正确顺序（后续做）：先把公共底座 `WIN_PARAM` / `OUTPUT_SCHEMA` / `observeWithLadder`
等搬到 `src/shared/`，再把工具体搬到 `src/tools/`。**为了勾清单制造循环依赖是错的**——所以停在这里。

---

## 7. 如何验证 / 调试

```powershell
# 单测（30 个，纯函数级）
npm run test:units
# 工具名 ↔ 文档一致性
node --test tests/tool-names.test.mjs
# 常驻 UIA 服务冒烟（需真实窗口句柄）
node -e "import('./src/uiahost-client.js').then(async m=>{const h=m.getUiaHost();console.log(await h.ping());m.closeUiaHost()})"
```

排障第一入口是 `computer_use_health`（后端、DPI、overlay、lease、approve、capture 统计）。
`captureRecovery` 计数告诉你走了哪条降级路（`wgc` / `wgc-after-nudge` /
`gdi-printwindow` / `stale-frame` / `gdi-failed`）。

---

## 8. 改动守则（给后续 agent）

- **不要新增特化脚本**：任何绑定"某场考试 + 某份答案文件"的东西都是脚手架，不是工具。
- **不要碰官方 13 方法契约**：它们与 `@oai/sky` 对齐，改了会破坏兼容。
- **新增工具前先问**：它是否在 DSH 宿主进程中真能跑通？（见 §5.2——屏幕像素不行）
- **失败必须说话**：任何降级/失败路径都要返回人能读懂的 `guidance`，不要静默。
- **改完跑测试**：`npm run test:units` + `tool-names` 至少这两个，全绿再提交。
- **拆文件不要靠手数行号**：P0 拆 `tool.js` 时，按行号切块把 `cachedCdpProbe` + 两个
  `*_SCRIPT` 常量误搬进了 `ui-tree/`，测试用 `path is not defined` 才抓出来。正确做法：
  先用 grep 定位"下一处 `^function`/`^export`/分隔注释"作为边界，切完立刻
  `node --check` 两边 + 跑单测 + 冒烟 `import('./src/tool.js')` 确认 re-export 闭环。
  纯函数块才搬；带 `ctx`/`state`/`execFileSync`/`path` 的不要搬。
