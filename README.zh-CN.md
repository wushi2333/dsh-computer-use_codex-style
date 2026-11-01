<div align="center">

# DeepSeek Harness · Computer Use

**让 DeepSeek Harness 直接操作 Windows 桌面应用与 Chromium 标签页。**
**参考Codex 的 `window2` 工具面**，**在DSH上体验Codex版的Computer Use**

[English](README.md) | 中文

![platform](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011%20x64-0078D4?style=flat-square)
![protocol](https://img.shields.io/badge/DSH-bundle%20%C2%B7%20cordis.patch.yml-4B5563?style=flat-square)
![surface](https://img.shields.io/badge/surface-13%20window2%20%2B%20extensions-2563EB?style=flat-square)
![baseline](https://img.shields.io/badge/Codex%20baseline-26.903.61454-6B7280?style=flat-square)
![license](https://img.shields.io/badge/license-MIT-22C55E?style=flat-square)


*截止到此插件发布前，官方已经在实验性版本更新Computer Use。总体设计思路与本插件有差异。目前本插件已做兼容性处理，可以同时安装两个插件，但不建议同时启用。*

</div>

---

## 目录

- [它能做什么](#它能做什么)
- [快速开始](#快速开始)
- [工作原理](#工作原理)
- [工具面](#工具面)
- [观察—动作—刷新循环](#观察动作刷新循环)
- [安全模型](#安全模型)
- [配置](#配置)
- [浏览器自动化](#浏览器自动化)
- [验证](#验证)
- [从源码构建](#从源码构建)
- [仓库结构](#仓库结构)
- [故障排查](#故障排查)
- [安全与隐私](#安全与隐私)
- [兼容性与已知差距](#兼容性与已知差距)
- [许可与归属](#许可与归属)

---

## 它能做什么

| | |
|---|---|
| **完整的官方工具面** | Codex官方 `window2` 的全部 13 个方法 —— `list_windows`、`get_window`、`list_apps`、`launch_app`、`get_window_state`、`click`、`press_key`、`type_text`、`scroll`、`set_value`、`drag`、`perform_secondary_action`、`activate_window` —— 名称、参数、默认值、返回体与错误串都与Codex一致。 |
| **真实输入与真实截图** | 输入走 `SendInput`，无障碍树走 UI Automation，截图走 `Windows.Graphics.Capture`（窗口被遮挡也能拍）。截图以视觉 image part 送达模型，JSON 里不含 base64。 |
| **可见、可中断的覆盖层** | 状态药丸 + 带弹簧动画的合成光标。药丸只在“截图那一瞬”被隐藏：模型永远读不到自己的状态药丸，而操作者始终看得到它。任何时候按 **Esc** 都能中断当前回合。 |
| **对照真机验证** | 行为对着官方插件与其 helper 做门禁：AX 树语法、覆盖层像素、光标运动、截图新鲜度、传输预算、批准文案、回合生命周期。 |
| **Harness 原生，不是 MCP 套壳** | DSH 工具 + 随包技能，而不是一个 JavaScript REPL。插件本身是一个 DSH **bundle**：`package.json` + `cordis.patch.yml` + 包根目录下的入口模块。 |
| **双平面、单 sidecar** | 桌面是进程级资源：指针、覆盖层、截图与批准流放在 HOST 平面；模型可见的工具放在 agent preset，普通编码会话不会拿到鼠标。 |

---

## 快速开始

### 环境要求

| | |
|---|---|
| 操作系统 | Windows 10 1809+ 或 Windows 11，**x64** |
| Harness | DeepSeek Harness（源码 checkout 或已安装的 `dsh` CLI） |
| Node | 20 或更高 |
| Python | 3.10+ —— 仅浏览器目录需要；13 个桌面工具是 Rust + Node |
| Rust | 仅在你重新编译 helper 时需要（`helper-rs/bin/win32-x64/` 已随包提供 release 二进制） |

### 1. 安装 bundle

```sh
# 从 GitHub 安装（无构建步骤、无安装期脚本）
dsh plugin --profile web add github:wushi2333/dsh-computer-use_codex-style

# 从本地 checkout 安装
dsh plugin --profile web add /path/to/dsh-computer-use_codex-style

# 从打包好的 tarball 安装
dsh plugin --profile web add ./dsh-computer-use-0.1.0.tgz
```

包内声明了 `dsh.bundle.patch`，所以 `dsh plugin` 会把 HOST 平面的行加进 profile 并链接该包。插件是纯 ESM JavaScript 加一个预编译 helper，**没有 `prepare` / `postinstall` 脚本**，pnpm 不会要求你额外允许构建。

### 2. 把工具行加进 agent preset

桌面工具**故意**放在 **agent preset** 里，而不是 host 组合里。挑一个你愿意交出鼠标的 preset，在它的 `agent.cordis.yml` 里加一行（preset 位于 `$DSH_HOME/.agent-presets/<name>/`）：

```yaml
# Computer Use 工具消费 HOST 的 dshComputerUse sidecar，本身不发布任何服务，
# 因此像 tool-bash 一样松散放置。请先在 profile 里安装 dsh-computer-use
# bundle，否则这一行会一直等待服务。
- id: tool-computer-use
  name: dsh-computer-use/tool
  config:
    surfaces:
      - computer
    browserSkill: computer-use-browser
```

建议给 Computer Use 单独留一个 preset：用 `standard` 启动的会话保留原有工具集，永远不会碰到你的鼠标。

### 3. 投递技能

两个技能随包提供，需要复制到 harness 读取的技能根目录：

```sh
# 在 profile 目录下执行
node node_modules/dsh-computer-use/scripts/sync-skills.mjs --write --defaults

# 之后随时核对副本（只报告漂移，不写文件）
node node_modules/dsh-computer-use/scripts/sync-skills.mjs --check --defaults
```

`--defaults` 指向 `$DSH_HOME/.agent-presets/computer-use/skills` 与 `$DSH_HOME/skills`；其他根目录用 `--target <dir>`。

### 4. 重启并开跑

重启对应 profile（插件面改动在启动时读取），用你的 Computer Use preset 新建会话，然后给它一个具体任务：

> 打开记事本，把这几条要点整理成一份干净的草稿。

第一次工具调用会拉起 helper，药丸出现，模型开始它的「观察 → 动作 → 刷新」循环。

### 卸载

```sh
dsh plugin --profile web remove dsh-computer-use
```

如果你手工加过 preset 行和技能副本，一并删除。

---

## 工作原理

```mermaid
flowchart TB
  subgraph DSH["DSH profile"]
    direction TB
    subgraph HOST["HOST 平面 — 进程级"]
      S["dsh-computer-use<br/>服务 `dshComputerUse`<br/>指针 · 覆盖层 · 截图 · 批准<br/>回合生命周期"]
    end
    subgraph PRESET["PRESET 平面 — 每个 agent preset"]
      T["dsh-computer-use/tool<br/>13 个 window2 工具<br/>+ harness 扩展"]
      K["skills<br/>computer-use · computer-use-browser"]
    end
  end
  S <-->|"stdio 上的 JSON-RPC，一行一个 JSON"| H["dsh-computer-use.exe<br/>Rust helper"]
  H -->|SendInput| APP["目标 Windows 应用"]
  H -->|UI Automation| APP
  H -->|Windows.Graphics.Capture| APP
  T --> S
  P["Python 引擎（可选）<br/>CDP · Playwright · 浏览器目录"] <--> S
```

**一个进程一个 helper，同一时刻一个回合。** sidecar 惰性拉起 `dsh-computer-use.exe` 并常驻；每个请求都带会话 + 回合身份，回合作用域变化时发送 `end_turn` —— 它会冲刷观察租约、隐藏覆盖层并恢复系统光标。这与官方一致：官方的 `Stop` / `Interrupt` / `SubagentStop` 钩子都调用 `turn_ended`。

**新鲜度靠身份，不靠时间。** 没有 TTL：窗口身份、包围盒与人机输入监视器一致时观察才有效。只要有人碰了被观察的窗口，下一次输入就会以 `user input was detected in this window; call get_window_state before continuing` 被拒绝。

**没有任何动作会被静默批准。** `launch_app` 与音频录制会暂停等待 harness 批准 UI，批准按应用记忆，被拒绝时按官方文案报错而不是重试。唯一的例外是**权限预设已经替你回答了问题**：完全访问会话（或批准策略为「从不询问」的会话）会直接放行应用门禁，`computer_use_health` 里会记录原因。

---

## 工具面

### 13 个Codex官方 `window2` 方法

| 工具 | 用途 |
|---|---|
| `list_windows` | 当前可定位的窗口 |
| `get_window` | 重新水合一个你已持有的窗口绑定 |
| `list_apps` | 已安装应用及其打开的可定位窗口 |
| `launch_app` | 启动应用（可能暂停等待批准） |
| `get_window_state` | 某个窗口的截图 + 完整无障碍树 |
| `click` | 按元素索引或截图坐标点击 |
| `press_key` | 按键与组合键（X keysym 风格名称） |
| `type_text` | 向当前焦点输入字面文本 |
| `scroll` | 从窗口相对坐标滚动 |
| `set_value` | 直接设置可访问元素的值 |
| `drag` | 从一个点拖拽/绘制到另一个点 |
| `perform_secondary_action` | 触发元素的次要动作 |
| `activate_window` | 把窗口带到前台 |

### Harness 扩展

| 工具 | 默认 | 用途 |
|---|---|---|
| `batch_actions` | 开启 | 一次调用执行一串确定性动作，然后做一次 `get_window_state` |
| `computer_use_health` | 始终 | 后端、允许列表、文档门禁、环境审计 |
| `computer_use_uia_act` | 开启 | 按**元素名**用 UIA pattern（`SelectionItem`/`Invoke`/`LegacyIAccessible`）操作控件：不需要截图、不需要坐标，因此在 WGC 截不到的静态窗口上也能用；用 `"A、选项|下一题"` 链式多步，返回动作后的文本树。**自带两步验证**：①`verify`（默认开）比对动作前后页面文本，报告 `applied`/`unchanged`——页面忽略的点击不再被当成成功；②`checkDialog`（默认开）在动作名含 交卷/提交 时探测确认弹窗，返回 `{dialog, body, buttons, confirm, nextStep}`，第二步不当成理所当然。**注意**：裸 `Text` 节点（小程序选项）无任何 pattern，兜底点击只代表"点发出去了"，需另行验证 |
| `computer_use_wait` | 开启 | 固定等待（可选顺带重新观察），不占一次 helper 往返 |
| `computer_use_wait_until` | 开启 | 轮询到文本出现 / 截图变化 / 超时为止 |
| `computer_use_scroll_to` | 开启 | 用多次顺序滚轮脉冲滚到底/顶/约百分比（脉冲间 70ms，因为应用会合并快速脉冲） |
| `computer_use_reach_last_page` | 开启 | 用更大的脉冲预算一次滚到最后一页——分页阅读器/PDF 查看器的首选 |
| `computer_use_documents` | 开启 | 导出活动文档 URL（`Document` 角色的 ValuePattern），PDF/查看器课件可直接取内容而不必逐页翻 |
| `computer_use_cdp_probe` | 开启 | 只读探测：窗口是否为 Chromium 宿主、CDP 端点是否可达——用于判定"能不能走 DOM 自动化"（微信 WMPF/Radium 不行） |
| `click_element`、`scroll_element` | 需显式开启 | 旧版 window-v1 别名 |
| `session_note`、`session_state`、`diagnostic_state`、`end_turn` | 诊断 | 会话草稿、状态转储与回合控制 |

想确认到底挂上了什么，最快的办法是 `computer_use_health`：后端、暴露的工具目录、批准策略、注入了哪些环境变量、文档门禁，以及覆盖层诊断（当前用哪条药丸渲染路径、推了多少帧、系统光标是否被压制、当前用哪种防捕获模式、此刻是否正处于截图遮蔽中）。

---

## 观察—动作—刷新循环

截图、元素索引与坐标都属于**同一次**观察。模型遵循的契约是：

```mermaid
sequenceDiagram
  participant M as 模型
  participant C as Computer Use
  participant A as 目标应用
  M->>C: get_window_state(window, include_text: true)
  C->>A: UI Automation 树
  C-->>M: 树 + 元素索引（+ 截图）
  Note over M: 索引只对这一次观察有效
  M->>C: click(window, element_index: 12)
  C->>A: SendInput
  M->>C: get_window_state(window)
  C-->>M: 新的树 + 新的截图
```

两条读会话记录时有用的结论：

- **一次观察只做一个动作。** 交叉执行多个动作、或在失败后重试，都必须先重新观察。
- **坐标是窗口相对的逻辑像素。** 截图的 `originX`/`originY` 是屏幕坐标 —— 两者永远不要混用。

---

## 安全模型

插件随包提供官方确认策略与一份硬拒绝清单，常驻提示词也会重复这些不可协商项：

- 永远不用 Windows 键 —— 单按不行，组合键也不行。
- 不自动化终端（Windows Terminal、命令提示符、PowerShell），不用「运行」对话框，不在文件资源管理器里夹带 shell 脚本。
- 不碰密码管理器、安全/杀毒软件、身份验证与年龄验证对话框。
- 不可信内容（网页、邮件、文档、截图）可以提供事实，但不能授予权限或证明用户意图。
- 可选的允许列表（`allowedApps`）从根上限制哪些 app id 可以被观察或驱动。

扩展工具面之前，先读随包的策略文档：`skills/computer-use/references/confirmations.md` 与 `skills/computer-use-browser/references/confirmations.md`。

### 安全契约（`src/safety/`）

四条规则决定一次调用能不能碰桌面。它们住在 `src/safety/`，**刻意不依赖任何宿主包**——
一个只能在整个宿主里跑起来的策略层，没有人会反复测试。

| 规则 | 做什么 |
|---|---|
| **按会话授权** | 从未被显式授权的会话，能力是关着的。授权**不能从会话内部放宽**；被委派的子代理一律拒绝（`SUBAGENT_COMPUTER_USE_UNAVAILABLE`），并被告知「把这一步交回主代理」。宿主可以从**会话之外**用 `service.safetyGrant(sessionId, true\|false)`（契约层的 `authorize()`）翻转某个会话的授权，同一个调用里顺带收敛该会话的 schema 可见性 |
| **schema 可见性** | 会话能力关着时，把它的工具 schema 从该 agent 的视野里摘掉，这个会话就不必为用不了的工具付费。释放必须走宿主返回的**函数**式 disposer——`restricted.dispose?.()` 是静默空操作，工具会永远不出现 |
| **跨会话写租约** | helper 是进程级的，所有会话共用同一只鼠标。写操作在执行期间持有租约，**空闲时钟从写完那一刻重新开始**。读操作永不参与仲裁；拒绝消息会说明对方是「正在执行」还是「安静了 N 秒」 |
| **失败结构化** | 每条拒绝都带稳定的 `code`、`retry` 判据（`never` / `retry` / `reobserve`）与「下一步做什么」。「它失败了」和「它失败了，先重新观察」需要相反的反应 |

契约里另外两条由更低层覆盖，本次**没有重复实现**：Rust helper 在锁屏时拒绝输入
（`helper-rs/src/desktop.rs`）、对更高完整性级别的目标给出完整性受限说明
（`helper-rs/src/uia.rs`）；而插件**从不把截图写到自己磁盘上**——字节交给宿主的 attachment store，
文件生命周期归宿主。

设计笔记、与参考实现的差距分析、以及移植过程中自己发现并修正的三个问题，见
[`docs/design-essence-dsh-computer-use.md`](docs/design-essence-dsh-computer-use.md)。

---

## 配置

### HOST 平面（`cordis.patch.yml` 的 `computer-use` 行）

| 键 | 默认 | 含义 |
|---|---|---|
| `backend` | `windows` | `windows`（原生 helper）或 `fake`（脚本化桌面，用于测试） |
| `surface` | `computer` | 向 helper 请求的工具目录 |
| `stealFocus` | `true` | 输入方法会自动激活目标窗口 |
| `maxImageEdge` | `0` | DSH 扩展：`0` 保持官方行为（不缩放）；设为 `1280` 可用画质换 token |
| `approveLaunch` | `true` | `launch_app` 前询问 |
| `timeoutMs` / `launchAppTimeoutMs` / `listAppsTimeoutMs` | `10000` / `15000` / `20000` | 请求预算；超时会拒绝在途请求（官方传输语义下还会杀掉 helper）。`list_apps` 是唯一在作答前要构建「已安装应用目录」的方法，所以它有自己的 DSH 预算，而这条预算必须**低于 harness 的 25 秒工具调用预算**：否则先被 harness 中止，而中止会以「取消」的形式到达 helper（旧版本会把这一回合latch成「用户已停止」）。`list_apps` 超时不再杀 helper（`keepAlive`），热目录缓存得以保留 |
| `startupTimeoutMs` | `15000` | helper 启动预算 |
| `preserveHelperOnTimeout` | `false` | 超时后保留 helper 以便排查 |
| `allowedApps` | `[]` | 非空时只有这些 app id 可被观察或驱动 |
| `envAllowlist` | `[]` | 在随包允许列表之外额外转发给 helper 的环境变量名 |
| `overlayCaptureExclusion` | `mask` | 状态药丸如何避开模型看到的截图：`mask`（默认）在单次捕获期间用合成器隐藏药丸，操作者始终能看到；`wda` 改用 `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`；`off` 不排除，药丸会出现在截图里 |
| `fallbackLatchAfterWedge` | `true` | 某应用第一次楔死后，本会话余下时间直接用 .NET UIA 兜底供文本，不再每次都等 helper 的 15s dump 超时 |
| `safetyDefaultEnabled` | `true` | 从未被显式授权或收回的会话，能力默认是否开启。`false` 则默认关闭并摘掉它的工具 schema |
| `safetyDenySubagents` | `true` | 是否一律拒绝子代理驱动桌面：子代理的权限范围在启动时就固定，不能从会话内部放宽 |
| `safetyHideToolSchemasWhenOff` | `true` | 能力关闭时是否摘掉工具 schema（已注册工具的 schema 随该会话的每个请求一起发出去） |
| `safetyControllerLease` | `true` | 是否在会话之间仲裁写操作：helper 是进程级的，所有会话共用同一只鼠标和键盘 |
| `safetyControllerLeaseIdleMs` | `10000` | 写租约在没有写操作时存活多久。空闲时钟从写**结束**那一刻重新开始，而不是从被接受那一刻 |

### PRESET 平面（`dsh-computer-use/tool` 行）

| 键 | 默认 | 含义 |
|---|---|---|
| `surfaces` | `[computer]` | 暴露哪些目录（`computer`、`browser`、`mac`、`all`） |
| `browserSkill` | `computer-use-browser` | 解锁浏览器目录的技能名 |
| `approvalDefault` | `prompt` | helper 因批准而拒绝时的行为：`prompt`、`allow`、`deny`。完全访问会话（或批准策略为「从不询问」的会话）直接放行且不询问；显式 `deny` 仍然失败关闭 |
| `approvalTools` | `{}` | 按工具覆盖上面的策略 |
| `enabledTools` | `[batch_actions]` | 哪些 harness 扩展进入模型可见目录 |

---

## 浏览器自动化

同一个 sidecar 还提供 Chromium 面：标签页、CDP 调用与事件、DOM 与无障碍快照、Playwright 风格交互、下载，以及通过随包 Chrome 扩展认领已登录标签页，共 `95` 个工具。

它是**技能门控**的：模型加载 `computer-use-browser` 技能前这些工具不会注册，普通会话的目录不会被淹没。浏览器路径是 Python 3.10+（`computer_use/`），通过 CDP 连接浏览器，不经过桌面 helper。

---

## 验证

一切都能在本仓库复现。插件面门禁完全不需要桌面：

```sh
# 在 DeepSeek Harness checkout 下运行，以便解析 peer 依赖
node --import tsx/esm --test tests/contracts.test.mjs tests/interactive_summary.test.mjs

# helper（Rust）
cargo test --release --manifest-path helper-rs/Cargo.toml

# 引擎（Python）
python -m pytest -q

# fake 后端冒烟：不需要桌面，也不会碰鼠标
node src/smoke.mjs
```

当前状态（Windows 11 x64）：**插件契约门禁 25/25**、**Rust 231 通过**、**Python 326 通过 / 4 skipped**，fake-backend 冒烟绿。

parity 套件会操作真实桌面，把本插件与官方实现逐项对比 —— 覆盖层像素、光标运动、截图新鲜度、window-state 用例、传输预算、AX 树字节、插件生命周期：

```powershell
pwsh -File parity/verify-all.ps1
```

它需要一个准备好的桌面（一个 `Parity Target` 窗口，AX 对比还需要 Word 与资源管理器），缺少夹具时会报 SKIP 而不是给出虚假的 PASS。开发证据 —— 逆向报告、差距登记册与验证日志 —— 刻意不放进公开源码树。

---

## 从源码构建

```sh
# JavaScript 插件：无需构建，纯 ESM
node src/exports-check.mjs

# Rust helper：构建、测试，并发布插件随包的二进制
pwsh -File scripts/ship-helper.ps1

# Python 引擎（可选浏览器面）
python -m pip install -e .
```

`scripts/ship-helper.ps1` 会跑 `cargo build --release`、helper 测试套件，把 `dsh-computer-use.exe` 复制到 `helper-rs/bin/<platform>-<arch>/` 并打印 SHA-256。因为该二进制已被跟踪，全新 checkout 无需 Rust 工具链也能直接运行。

想在开发时直接指向 checkout 而不是安装副本：

```sh
dsh plugin --profile web add /path/to/dsh-computer-use_codex-style
```

---

## 仓库结构

```text
package.json            DSH bundle 清单（dsh.bundle.patch + dsh.skills）
cordis.patch.yml        本 bundle 插入的 HOST 平面层
dsh-plugin.json         Codex 风格的插件清单（身份、界面、工具列表）
src/
  index.js              HOST 服务：sidecar 生命周期、回合生命周期、health
  tool.js               PRESET 行：工具注册、批准、文档门禁
  sidecar.js            helper 传输：启动、JSON-RPC、预算、路由
  prompt.js             常驻提示词段 + 按需参考文档清单
  paths.js              helper / python 发现逻辑
skills/
  computer-use/         桌面技能及其参考文档
  computer-use-browser/ 浏览器技能及其参考文档
helper-rs/              Rust helper（输入、UIA、截图、覆盖层、光标）
  bin/<platform>-<arch>/  随包发布的 release 二进制
helper-swift/           macOS helper 源码（尚未提供二进制）
computer_use/           Python 引擎（CDP、Playwright、浏览器目录）
parity/                 验证套件与夹具
tests/                  插件与引擎测试
docs/                   插件说明与图片资源
```

---

## 故障排查

| 现象 | 含义 |
|---|---|
| 函数列表里没有 Computer Use 工具 | 会话没有用挂载 `dsh-computer-use/tool` 的 preset，或者安装 bundle 后没有重启 profile。 |
| `The Windows Computer Use helper may have failed` | helper 没在 `startupTimeoutMs` 内启动。检查允许列表与环境允许列表后重试；`computer_use_health` 会报告实际注入了什么。 |
| `coordinate input target is unavailable` | 观察已过期或窗口已变化。重新 `get_window_state` 并对新结果操作。 |
| `user input was detected in this window` | 有人用过这个窗口。下一次输入前必须重新观察。 |
| 运行过程中焦点跳动 | `stealFocus: true` 的预期行为：输入方法会激活目标窗口。 |
| 桌面被锁定 | Computer Use 会停下并要求你解锁，它绝不操作 `LockApp.exe`。 |
| 屏幕上出现两个光标 | 覆盖层会画一个合成光标并把系统光标置空。真出现两个时，读 `diagnostic_state` → `overlayState.systemCursorFailures`：计数非零说明压制失败，那是设计上唯一会通向「双指针」的路径。 |
| 状态药丸一直不出现 | 读 `computer_use_health` → `overlay.captureExclusion`。默认的 `mask` 永远让药丸留在屏幕上。官方风格的 `wda` 亲和性会在某些 Windows/DWM/GPU 组合下让 DWM **连屏幕上都不再合成**药丸的 DirectComposition 内容（操作者只看到合成光标、看不到药丸，而所有覆盖层 API 仍然报 `visible=true`），所以它是可选项。遮蔽卡住会表现为 `overlay.captureMasked = true`，而 helper 会在五秒后自行解除。 |
| `list_apps` 超时 | 已安装应用目录是唯一在作答前真的要做重活的方法。两份能留下来的记录：`computer_use_health` → `overlay`/`appCatalog` 计时，以及 helper 自己写的 `%LOCALAPPDATA%\computer-use-app-catalog\slow-requests.log`（记录超过 `DSH_CU_SLOW_REQUEST_MS`（默认 1 秒）的每个请求，含目录与重建阶段耗时）。传输层超时会杀掉 helper，所以进程内诊断在你来得及问之前就已经消失；如果这台机器上目录确实慢，调大 `listAppsTimeoutMs` |
| 没按 Esc 却报 “Computer Use was stopped by the user with the physical Escape key” | 工具调用被 harness 自己的预算中止，旧版本的 helper 会把该回合 latch 成「已停止」。现在中止只发 `cancel`（仅隐藏覆盖层）；若仍出现，说明 helper 是旧版本（下一回合会拉起新的 helper） |

---

## 安全与隐私

- **Agent 控制的是真实的鼠标、键盘与屏幕。** 请只在你能接受这一点的会话里使用 Computer Use，并保持拒绝清单完整。
- **批准是显式的。** `launch_app` 与音频录制会暂停等待 harness 批准 UI；没有任何动作会被静默批准，被拒绝的请求也绝不重试。完全访问（或批准策略为「从不询问」）的会话除外：那是用户自己的选择，插件按 `allow` 放行并留下审计记录。
- **截图是视觉 part。** 它们以图片附件形式送达模型，JSON 工具结果里不含 base64。
- **不可信内容始终不可信。** 网页、文档与邮件可以给模型提供信息，但无权授权任何动作。

---

## 兼容性与已知差距

本插件对标的基线是 Codex Computer Use 插件 `26.903.61454`：相同的 13 个方法、相同的参数名与默认值、相同的返回体、相同的错误串、相同的 AX 树语法、相同的预算、相同的覆盖层语义。

DSH官方的Computer Use采用设计思路不大一致。如果希望使用Codex风格的功能，可以使用本插件。

### 截图需要窗口"有变化"（静态窗口）

`Windows.Graphics.Capture` 只在目标窗口**发生重绘**时给帧。静态页面——考试答题页、设置对话框、停住的文档——会直接 `window capture timed out`；没有帧就拿不到坐标输入。现在遇到这种情况会**按下面顺序降级**，而不是整单失败：

1. **抖一下再重试**：`scripts/gdi-capture.ps1 -NudgeOnly` 做一次 1 像素尺寸回弹（外加 `InvalidateRect`/`UpdateWindow`）逼窗口重绘，然后**重取一次** helper 的捕获。这一帧落在 helper **自己的**捕获会话里，所以 `screenshotId`、坐标点击、观察租约全都照常可用。需要抖动的窗口会被记住，**下一次捕获前先抖**——15 秒超时每个窗口只付一次，而不是每次观察都付。
2. **PrintWindow（GDI）**：`PrintWindow(PW_RENDERFULLCONTENT)` 完全不需要新的 DWM 帧。抓到的图按普通截图注入，带 `source: "gdi-printwindow"`：读页面、算 x/y 都够用，但 helper 手里没有这一帧，所以优先用元素名（`computer_use_uia_act`），坐标点击前会自动补一次观察。
3. **陈旧帧**：GDI 也失败时，返回该窗口**上一次成功**的帧，标 `stale: true` 并附年龄。
4. **纯文本**：只有到这一步才退回原来的 `captureFailed: true` + UIA 文本兜底。

`value.capture` 会说明走了哪条路（`path`/`stale`/`recovered`/`oversizeWindow`/`guidance`），`computer_use_health.captureRecovery` 则给出计数（`wgc`、`wgc-after-nudge`、`gdi-printwindow`、`stale-frame`、`gdi-failed`）。Chromium 宿主上"只取文本"的观察依旧完全不走捕获通道（.NET UIA 兜底 + 有效租约）；`computer_use_uia_act` 也依旧按元素名操作，既不需要帧也不需要坐标。

同一轮还有两个配套行为：

- `batch_actions` 现在由插件**逐步执行**，而不是一整个丢给 helper：每一步的 `shot-N` 截图 id 都像顶层调用一样翻译；连续两个坐标动作之间会补一次"只带截图"的观察；遇到 `user input was detected in this window` 会先补观察再重试这一步，而不是整批中止。
- **微信小程序（WMPF）页面既不吃鼠标也不吃触摸注入**。实测 40 题安全考试：`SendInput` 点击事件确实送达（返回 2、光标回读正确、窗口在前台、`WindowFromPoint` 命中页面），但页面完全无反应；`InjectTouchInput` 全部 `err=87`，因为本机 `IsTouchWindow=false`。**真正有效的是两件事**：`scripts/uia-post.ps1`（`PostMessage` 直接把 `WM_MOUSEMOVE`/`WM_LBUTTONDOWN`/`WM_LBUTTONUP` 投给渲染子窗口，`lParam = x|y<<16`）与 `scripts/uia-ticks.ps1`（**按像素判红**读勾选框 ON/OFF）——这些选项是裸 `Text`、没有任何 `SelectionItem`/`Toggle` pattern，像素是唯一可信的回读手段。
- **不要用"算出来的坐标"去点 WMPF 多选题**。选项标签的 `y` 不是勾选框的 `y`（实测某 5 选项题差 **+48**，且 4 选项与 5 选项行距不同），而且**每勾一次页面就会重排**，所以"读一次坐标再连点一批"必然错位。可靠姿势是：**人工勾选 + `uia-ticks.ps1` 像素复核**（先扫 200–440 竖带找红区定位行，再 `-Points "119,<y>;..."`）。
- 两步确认交互用 `computer_use_uia_act`：点第一个控件（如 `交卷`），`checkDialog` 会报告确认弹窗与真正提交的按钮；**最后一步提交刻意保持为独立的显式动作**。
- 交卷可以稳定用 `computer_use_uia_act`：点 `交卷` → 确认弹窗点 `提交`。已端到端验证（40/40，100 分）。
- `computer_use_uia_act` 的点击兜底（给没有任何 UIA pattern 的节点）在注入事件上打了 helper 的 `SYNTHETIC_TAG`（`dwExtraInfo`），所以它自己的点击不会再被当成"人工输入"。
- `computer_use_scroll_to` / `computer_use_reach_last_page` 不再只信滚轮。阶梯是：滚轮 → `Page_Down` → `Right` → `Next` → **插值 swipe**（`scripts/uia-swipe.ps1`：14 个中间移动、350ms、带 `SYNTHETIC_TAG`、**不需要 helper 捕获会话**；helper 自带的 `drag` 只有 4 个事件、中间没有移动，网页阅读器不认）。**是否真的翻动**用三信号判定，最硬的先判：页码「当前/总页」→ UIA 树文本 diff（`documentSnapshot()`，还能识别 `最后一页了` → `lastPage`）→ GDI 帧哈希；**帧相同只会降级为 `unknown`**，因为 `PrintWindow` 对 Chromium 表面常返回陈旧帧。返回体给出 `page`、`pageProbe`、`swipe:{ok,from:'document-rect'|'window-rect'}`、`moved:'text'|'frame'|'unknown'`、`lastPage`、`guidance`。树里第一个 `Document` 常常是**别的后台课程**，所以文档矩形要过“能塞进当前窗口”的检查，不合格就退回窗口矩形，保证 swipe 落在可见文档里。已证伪的一条路：**UIA ScrollPattern**——第一个可滚动节点是隐藏文档（`VerticalScrollPercent` 始终 `-1`），按“可见 + VerticallyScrollable”过滤后，81 个元素的树里一个都没有。

另外，**比桌面还大**的窗口永远合成不了。捕获脚本会同时报出屏幕与窗口矩形，所以提示会**直接说明**这件事，并把窗口缩到屏幕内即可。

被**别的窗口遮住**的目标也是同一类失败（helper 不肯把"遮挡窗口的像素"当成交付内容）。走的是同一条阶梯：先 `activate_window` 再重取；仍然被遮就用 `PrintWindow` 抓目标**自己**的表面（实测遮挡状态下 782×735 窗口仍能出 71 KB PNG），所以结果会是 `path:"wgc-after-activate"` 或 `"gdi-printwindow"`，而不是直接失败。


---

## 许可与归属

MIT —— 见 [LICENSE](LICENSE)。

这是面向 DeepSeek Harness 的独立重实现，与 OpenAI **没有**隶属、背书或赞助关系。Codex 是 OpenAI 的商标，此处提及仅用于描述行为兼容性。

仓库中包含逐字复制的第三方材料，它们保留各自的条款：

- `helper-rs/assets/prompts/` 与各技能的 `references/` 目录包含官方 Computer Use 的 guidance、API 参考与确认策略文档，复制它们是为了让模型读到与官方插件相同的文字。
- `parity/golden-ax/` 与 `parity/ax-rich/` 包含官方 helper 产出的无障碍树样本，作为 parity 门禁的对比基线。

如果你要再分发本项目，请按你获取这些文件时所依据的条款复核它们。
