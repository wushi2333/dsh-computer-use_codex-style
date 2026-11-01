# 设计精华总结：源项目 `dsh-computer-use`（v1.40.3）

> **源项目**：`C:\AITask\dsh-client-config\git-repo\dsh-computer-use`（`@local/dsh-computer-use` v1.40.3，MIT）
> **吸收目标**：`C:\AITask\dsh-client-config\plugins\dsh-computer-use`（本项目，DSH 原生三段式实现）
> **成文日期**：2026-10-28　**对应分支**：`feat/safety-contract`

---

## 0. 一句话结论

源项目真正的价值不是「能点鼠标」，而是**把「能点鼠标」这件事变成一份可审计的契约**：
能力默认关闭、按会话授权；关着的时候连 schema 都不占模型的上下文；写操作持租约、空闲 10 秒自动让位；
锁屏与 UAC 明确拒绝而不是静默失败；会话结束自动删掉自己产生的截图。

这五条合起来回答的是同一个问题：**模型能碰到桌面的哪一部分、什么时候能碰、碰到出问题时谁负责说清楚。**
本项目在**架构上完全不同**（三段式 Node→Python→Rust），但在这条契约上，源项目有三个地方比本项目做得更完整，
可以整体吸收：**跨会话的写操作仲裁（SEC-03）**、**能力关闭时的 schema 摘除（SEC-02）**、
**失败的结构化分类学（code + retry + 下一步）**。详见 §4 / §5。

---

## 1. 源项目的架构骨架

源项目是**单进程 Node 插件 + PowerShell 后端**。它把「宿主半边」（工具注册、路由、会话授权）
和「后端半边」（`backend/worker.ps1`，65 万字符、拆成 13 个 part）分开，中间用一份**行为契约**接缝。

| 层 | 文件 | 规模 | 职责 |
| --- | --- | --- | --- |
| 宿主半边 | `index.js` | 271 KB | 工具注册、路由、会话授权、指标、trace |
| 前端芯片 | `client.js` | 28 KB | 输入框工具栏的「电脑操作」开关（授权的 UI） |
| 契约 | `internal/tool-contracts.mjs` | 81 KB | schema / 描述 / 后端分发表的**唯一**一份清单 |
| 失败分类学 | `internal/failures.mjs` | 24 KB | `code` + `retry` 判据 + 「下一步做什么」 |
| 租约 | `internal/lease.mjs` | 6 KB | 跨会话写操作仲裁（空闲 10s 自动释放） |
| schema 可见性 | `internal/schema-visibility.mjs` | 14 KB | 能力关闭时把工具从 agent 视野里摘掉 |
| 安全软件拦截 | `internal/security-block.mjs` | 3 KB | 把 AV/AMSI 拦截与「插件没挂载」分开报 |
| 捕获服务 | `internal/capture.mjs` | 23 KB | 截图落盘、按会话记账、会话结束删除 |
| 后端 | `backend/worker.ps1` + `parts/00..13` | 656 KB | SendInput / GDI / UIA / 批处理 / 等待 / 帧 |
| 诊断 | `doctor.mjs` + `internal/doctor.mjs` | 40 KB | `resolve / activate / backend` 三层诊断 |
| 发行身份 | `release-manifest.json` + `tools/pack-candidate.mjs` | — | 半套升级 / 混装检测 |

**核心结构判断**：源项目把「能力能不能用」「用了会付出什么代价」「失败了怎么办」拆成了**三个互不依赖的模块**
（`grant` / `schema-visibility` / `lease`），它们之间唯一的接口是「一个会话 id」和「一个布尔授权值」。
这个低耦合是它可以被反复打补丁到 1.40.3 还没散架的原因。

---

## 2. 五条安全契约（SAFETY CONTRACT // 5 CLAUSES）

> 对应源项目宣传页的 `SAFETY BY DEFAULT` 一页。以下每条都给出**动机 → 机制 → 代码位置 → 容易被做错的地方**。

### SEC-01　默认关闭，按会话授权

* **动机**：一份会移动鼠标和键盘的能力，不应该在「模型想用」的时候自动可用。
* **机制**：
  * `index.js` 维护一份**按会话**的授权表；工具入口 `requireGrant(exec)` 在**参数校验之前**执行，
    所以关着的能力无论模型传什么参数，回的都是同一条可执行提示
    （`index.js:654-669`，`[NOT_AUTHORIZED] 电脑操作尚未在本会话开启。…`）。
  * 授权的粒度是**会话**，不是部署。`sessionKey()`（`internal/sessions.mjs`）做归一化。
  * **授权表的形状**（`internal/sessions.mjs:25-48`）：`Map<string, boolean>`，
    key = 归一化 sessionKey，value = **显式**授权 `true` / **显式**收回 `false`，
    **没有记录 = 用部署默认值**。查表用 `??` 而不是 `||`——`false || true` 会把
    「显式关闭」错当成「没有记录」。这是整张表最容易写错的一处。
  * **`sessionKey(undefined|null) === ''`**（匿名会话）。匿名会话**仍然走同一套门禁**，
    不是「拿不到 id 就绕过门禁」。
  * **拒绝码有三个，顺序固定**（`index.js:655-668`）：
    `[UNSUPPORTED_PLATFORM]` → `[SUBAGENT_COMPUTER_USE_UNAVAILABLE]` → `[NOT_AUTHORIZED]`。
    子代理判定**在授权检查之前**，所以一个已被授权的子代理拿到的仍然是
    「你不该驱动桌面」而不是「你还没被授权」——前者告诉他该做什么，后者只会让他去点开关。
  * 授权写入路径只有一条：`POST /computer-use/state`（body `{sessionId, enabled}`）；
    读取是 `GET /computer-use/state?sessionId=…`。授权表是**纯内存**的，DSH 重启即回默认值。
  * **子代理一律拒绝**（`index.js:223` `SUBAGENT_REFUSAL`）：`isSubagent(exec)` 读
    `exec.agent.session.header.origin === 'subagent'`。
    理由写得很清楚——子代理的权限范围在**启动时就固定**，不能从会话内部放宽。
    「一个能被绕过的策略层不叫策略层」。
* **容易做错**：把「默认关闭」做成「全局关闭」——那会让所有会话都失去能力，等于把功能删了。
  源项目是**每会话一个开关**，由用户点输入框里的芯片来开。

### SEC-02　关闭时摘掉 schema，不占上下文

* **动机**：已注册工具的 schema 随该会话的**每个请求**发出去。一个「这个会话根本不能调」的工具是纯开销。
  源项目实测：**这一族占了每请求 20,990 个 schema token 中的 15,054**（`internal/schema-visibility.mjs:1-10`）。
* **机制**：`tools.restrict({ deny: names })` 只把工具从**这一个 agent 的视野**里摘掉；
  定义仍然注册着、调用照样被拒，开关一打开立刻恢复。
  摘除用 `agent.ctx.tools.get(name, scope)` 先确认名字在这个组合里存在，再 `restrict`。
* **容易做错（两个真实踩过的坑）**：
  1. **释放姿势**：宿主 `tools.restrict()` 返回的是**函数**（cordis 的 `ctx.effect(fn)` 返回 `Disposable<T> = () => T`），
     身上既没有 `.dispose` 也没有 `[Symbol.dispose]`。所以 `restricted.dispose?.()` 是**静默空操作**——
     限制挂上去就再也摘不下来，而账本已经记成「恢复可见」：**芯片是亮的、工具永远不出现**。
     源项目因此把释放单独做成一个函数 `liftRestriction()`，两种形态都认（`schema-visibility.mjs:84-87`）。
  2. **作用域 key 不是 Context**：`tools.get(name, scope)` 的第二个参数必须是 `dsh-scope` 的 `scopeOf(ctx)`，
     把 `agent.ctx` 本身传进去等于把一个 Context 对象当成 key 用，隐藏与否都会跟着错。
     源项目把 `scopeOf` 的解析放在模块加载时做一次，并且**允许失败**（失败就退回旧调用形态，不会更坏）。

### SEC-03　写操作持租约，空闲 10 秒自动释放

* **动机**：一个宿主进程只有一个 worker，所以**多个会话共用同一只鼠标和键盘**。
  两个会话同时动手，会交织成双方都无法解释的状态。
* **机制**（`internal/lease.mjs`）：
  * 只有**会移动桌面**的调用才需要租约（`movesDesktop(cmd, args)`）。读操作永远不需要，
    `stop` / `release` 这条安全通道也不需要——**能被另一个会话挡住的安全控制不是安全控制**。
  * 租约在**写操作真正执行期间**保持：`beginWrite()` / `finishWrite()` 计数，
    **空闲时钟在写操作结束那一刻重新开始**。
  * `CONTROLLER_IDLE_MS = 10_000`：空闲即释放，而不是要显式交还——会话之间交接只是「停一下」，不需要仪式。
* **容易做错**：
  1. 早期版本租约在**调用开始时**打戳、十秒后自行过期，于是**一次长拖拽或含等待的批次还在跑，
     桌面就可能被交给另一个会话**。所以租约必须由写操作的 begin/end 包住。
  2. 拒绝时要说清是「**正在执行**」还是「**安静了 N 秒**」——这两种情况的应对完全不同：
     前者必须等它跑完，后者基本就是马上（`lease.mjs:66-77`）。

### SEC-04　锁屏与 UAC 明确拒绝，不静默失败

* **动机**：锁屏和 UAC 安全桌面会让合成输入**无声地丢掉**。如果不明确拒绝，模型会看到
  「点了但什么都没发生」，然后反复重试、反复失败。
* **机制**：`internal/failures.mjs` 的 `FAILURE_RULES` 里两条硬判据：
  * `SCREEN_LOCKED`（`retry: 'never'`）——「当前不可交互（锁屏或安全桌面）。请先让用户解锁，再重试。」
  * `SEND_INPUT_BLOCKED`（`retry: 'never'`）——「系统丢弃了合成输入。常见原因：工作站已锁屏、有 UAC 安全桌面弹窗、
    会话未连接到控制台、或目标窗口以管理员身份运行而本进程不是（UIPI）。
    请先让桌面回到可交互状态；**对提权窗口可改用 `computer_ui_action` 走 UIA 通道（不受 UIPI 限制）**。」
* **要点**：错误消息不只说「失败了」，而是给出**一条可走的替代路径**（UIA 通道）。

### SEC-05　会话结束自动删截图（`save_path` 除外）

* **动机**：截图、UIA 文本、剪贴板都可能含敏感信息，不该在磁盘上留到用户自己去清。
* **机制**（`internal/capture.mjs:190-310`）：
  * 只有**两个集合**：`producedBySession`（sessionKey → 该会话产出的路径集合）与
    `unclaimed`（已落盘但尚未归属任何会话的路径）。
  * 归属是**后填的**：`nextCapturePath` 只知道要一个文件名；真正把图交给模型的是 `admit`，
    而 `admit` 手里有 sessionId。所以路径先落进 `unclaimed`，`admit` 时再挪进对应会话的集合——
    「谁的文件」由事实决定（谁真的看过了），而不是猜。
  * **`save_path` 显式指定落盘位置的永远不进这两个集合**：那不经过 `nextCapturePath`，
    插件既不知道、也不该删。**那是调用方的文件。**
    这条豁免有**三重保证**：①不经过 `nextCapturePath`（压根没登记）；
    ②`claim()` 的 `if (!unclaimed.delete(path)) return;` 闸门（即使 `admit` 收到也立刻返回）；
    ③两个删除集合里都没有它。
  * 删除是 best effort，且**按基名前缀扫目录**而不是给每个派生帧单独记账——
    关联帧（`.related-<index>-<hwnd>`）由 worker 决定抓几张，记账会漏，扫目录不会。
  * `releaseSession(sessionId)` 删本次会话看过的所有图；`releaseAll()` 连 `unclaimed` 残渣一起清。
  * **只有两个触发点**：会话结束（`agent/disposed`）与插件卸载。
    **没有 idle timer、没有 TTL、没有 turn-end 钩子**——「会话结束」就是会话结束，
    不需要第三个时机。

---

## 3. 值得单独提炼的六个设计精华

这六条不属于那五条契约，但同样是可以直接搬的工程手法。

### E1　契约先于装配（contract ≠ assembly）

`internal/tool-contracts.mjs` 是 schema、描述与 worker 分发表的**唯一**一份清单。
`internal/failures.mjs` 的注释里记着它的来历：「曾经有三份并且互相矛盾」。
判定标准：**一份事实只能有一个权威副本**；装配（谁在什么作用域下注册、发给谁）可以有多个，契约不能。

### E2　失败分类学：`code` + `retry` + 「下一步做什么」

`internal/failures.mjs:58-101` 的 `FAILURE_RULES` 是一张**有序表**，第一条命中即用。每条规则给出：

| 字段 | 含义 |
| --- | --- |
| `code` | 稳定错误码，模型与 doctor 都按它归类 |
| `retry` | `never` / `retry` / `reobserve` —— **「它失败了」和「它失败了，先重新观察」需要相反的反应** |
| `hint` | 下一步做什么。`retry: 'never'` 尤其重要：重复一个目标根本不支持的动作只会烧 turn |

同文件还有 `READ_ONLY_*` / `IDEMPOTENT_*` / `isIdempotentCall`：
**没列进去的一律按不可重发处理**——「把可重发的当不可重发，只是多看一眼；把不可重发的当可重发，就是重复一次副作用」。

### E3　拒绝要说清「正在执行」还是「安静了 N 秒」

来自 E2 与 SEC-03 的交叉。一律的「另一个会话正在使用桌面，请稍后重试」会让模型做无用轮询。
源项目给出的是**可执行的下一步**：`a few seconds`（等它跑完）还是 `Ns`（马上就能接手）。

### E4　释放姿势要认全形态

见 SEC-02 的坑 1。一个只认 `.dispose()` 的释放函数，在一个只返回函数的宿主上会**静默失效**。
配套的工程纪律：**「摘掉一条限制」这种有且只有一种正确姿势的操作，单独成函数、两个调用点共用**。

### E5　幂等收敛：任何一次接触都是一次对齐

源项目的 `visibility.sync(agent)` 是幂等的，且在**三个**地方被调用：
agent 创建时、每次芯片开关、以及**每个工具调用里的授权检查**。
所以一个早于插件监听器存在的会话，也会在它下一次调用时收敛到当前授权值。
配套的鲁棒性设计：**同步一个 agent 的同时把它登记下来**——
「任何一次 sync 都是一次真实的接触，所以接触即登记，比只靠一个事件面结实」。

### E6　降级不是 404：失败也要能回答

宿主导入失败时，源项目仍挂一个只读面，`/computer-use/status` 与 `/computer-use/doctor`
返回 **503 + `{state:'failed', stage, code, resolution}`**，而不是 404。
并且**拒绝把导入失败伪装成正常**：1.40.2 曾经用裸 `import('@deepseek-ai/dsh-scope')`，
失败被 `catch` 吞掉，于是「能读到清单」被报成「可解析」，而实际作用域隔离是降级的。
1.40.3 改走统一解析器，并把**解析结果本身作为事实报出去**。

---

## 4. 与本项目的对照（差距分析）

本项目是**三段式**：Node 宿主面（`src/index.js` 服务 + `src/tool.js` 预设行）
→ Python sidecar（`computer_use/*.py`）→ Rust 原生 helper（`helper-rs/`）。

| 契约 | 源项目 | **本项目现状** | 差距 |
| --- | --- | --- | --- |
| **SEC-01** 默认关闭，按会话授权 | `index.js` 授权表 + 芯片 UI + 子代理硬拒 | ❌ **无会话授权**；❌ **无子代理拒绝**（子代理继承预设层的工具，可直接驱动桌面） | **大** |
| **SEC-02** 关闭时摘 schema | `tools.restrict({deny})` + `liftRestriction` | ❌ **无**。工具 schema 每轮全量下发 | **大**（token 成本） |
| **SEC-03** 写操作持租约，空闲 10s 释放 | `internal/lease.mjs` | ❌ **无跨会话仲裁**。只有**观测新鲜度**租约（`freshness.py`，`ObservationLease`），它管的是「输入前有没有新鲜观察」，**不管两个会话抢同一只鼠标** | **大** |
| **SEC-04** 锁屏 / UAC 明确拒绝 | `failures.mjs` 两条 `retry:'never'` 规则 | ✅ **本项目更强**。`helper-rs/src/desktop.rs` 有完整锁屏闸门（输入桌面名 + `LockApp.exe` 前台 + 屏保进程，且**只读发现路径在锁屏时仍然可用**）；`uia.rs:3607 integrity_blocked` + `win_uia.py:415` 有 UAC/UIPI 完整性闸门与专属文案 | **无缺口** |
| **SEC-05** 会话结束删截图（`save_path` 除外） | `capture.mjs` 按会话记账 + 删派生帧 | ⚠️ **架构不同**。本项目**从不把截图写到自己的磁盘**——截图字节交给宿主的 `attachments.saveImages()`（`src/tool.js:881-905`），文件生命周期归宿主。**但**浏览器路径确实漏临时文件：`browser_tab_ops.py:201` `tempfile.mkdtemp()`、`cdp_browser.py:434/509/550/618` 的 `gettempdir()/cdp-media-*.bin` 等**从不被清理** | **中**（真实泄漏，但不在截图上） |

**额外发现的两处差距**（不在五条契约里，但同源）：

1. **失败分类学缺位**。本项目 `computer_use/errors.py` 只有 4 个异常类（18 行），
   没有 `code` + `retry` 判据表。拒绝消息靠各个 `raise` 现场手写，
   同一个语义（例如「需要先观察」）在 Python 路径、Rust 路径、JS 路径各写一遍。
   → 对应 E2。
2. **「能力不可用」没有三态**。本项目有 `backend: windows|live|fake|helper`，
   但没有源项目那种「not-applicable / below-minimum / supported」的显式能力门。

---

## 5. 可吸收清单

### ✅ 吸收（本次移植实施）

| # | 精华 | 来源 | 落点 |
| --- | --- | --- | --- |
| A1 | **跨会话写操作租约**（空闲 10s 自动让位；读操作永不需要；拒绝分「正在执行 / 安静 N 秒」） | SEC-03 / `lease.mjs` | 新 `src/safety/controller-lease.js` |
| A2 | **能力关闭时摘掉 schema**（`tools.restrict` + 认全释放形态 + scope key 正确解析） | SEC-02 / `schema-visibility.mjs` | 新 `src/safety/schema-visibility.js` |
| A3 | **按会话授权 + 子代理硬拒**（授权不可从会话内部放宽） | SEC-01 | 新 `src/safety/grant.js` |
| A4 | **失败结构化分类学**：`code` + `retry` + 「下一步做什么」 | E2 / `failures.mjs` | 新 `src/safety/failures.js` |
| A5 | **插件自产临时文件的按会话清理**（`save_path` 除外的那条原则） | SEC-05 / `capture.mjs` | 新 `computer_use/temp_janitor.py` |

### ⚠️ 只做结构对齐，不改行为

* **SEC-04**：本项目已实现且更强（`helper-rs/src/desktop.rs` 锁屏闸门 + `uia.rs` 完整性闸门）。
  本次**只**把它的错误语义接进 A4 的失败分类学（`SCREEN_LOCKED` / `ELEVATION_BLOCKED`），
  让 JS 层与 Python 层说同一套话，**不动 Rust 闸门本身**。

### ❌ 不吸收（附理由）

| 精华 | 不吸收的理由 |
| --- | --- |
| 芯片 UI（`client.js`，输入框工具栏开关） | 本项目**没有前端行**，授权入口只能由宿主/配置提供；照搬 UI 需要新增一套 client 协议，收益不抵成本。改为**配置 + 宿主 API** 双入口。 |
| PowerShell 后端 / `worker.ps1` 13 段拆分 | 本项目后端是 Rust helper，架构正交，搬不过来。 |
| `doctor.mjs` 三层诊断 + `release-manifest.json` 发行身份 | 本项目已有 `scripts/ci-smoke.ps1` 与 `parity/` 体系，形态不同；不在本次范围。 |
| token-guard / app-launch | 本项目没有这两个子能力。 |
| `security-block.mjs`（AV 拦截分类） | 本项目后端是 Rust 进程而非 PowerShell 脚本，AMSI 签名判据不适用。 |

---

## 6. 移植设计

### 6.1 模块布局

新增宿主面目录 `src/safety/`（**纯 ESM、无宿主依赖**，可在 `node --test` 下单测）：

| 模块 | 导出 | 职责 |
| --- | --- | --- |
| `src/safety/failures.js` | `FAILURE_RULES`、`decorateFailure`、`RETRY` | 有序失败判据表；`code` + `retry` + `hint` |
| `src/safety/grant.js` | `createGrantStore`、`isSubagentSession` | 按会话授权表；子代理识别；`requireGrant` |
| `src/safety/schema-visibility.js` | `createSchemaVisibility` | `tools.restrict` 摘除 / `liftRestriction` 释放 / `liveAgents` 登记 |
| `src/safety/controller-lease.js` | `createControllerLease`、`movesDesktop` | 跨会话写仲裁；`beginWrite`/`finishWrite`；10s 空闲 |

### 6.2 接线

```
宿主面 src/index.js（ComputerUseService，进程级单例）
  ├── this.safety = createSafetyContract({...})   ← 装配层：四个模块的唯一入口
  │     ├── .grant      按会话授权表（Map<string, boolean>）
  │     ├── .lease      跨会话写仲裁（10s 空闲）
  │     ├── .visibility schema 摘除（tools.restrict）
  │     └── .gate(exec, toolName)  ← 预设面唯一的调用入口
  ├── ctx.on('agent/created')    → visibility.remember(agent)
  ├── ctx.on('agent/disposed')   → visibility.forget(agent)
  ├── ctx.on('session/disposed') → safety.forgetSession(id)（授权 / 租约 / 限制一起放掉）
  ├── ctx.effect(卸载)           → safety.releaseAll()
  └── health() → 追加 safety: { config, control, schemaVisibility, sessions }

预设面 src/tool.js（工具行，作用域受限，拿不到 session/event）
  ├── registerDesktop() → safety.setToolNames(exposedNames)   ← 供 schema 摘除用
  └── makeSidecarTool().execute()
        └── safety.gate(exec, spec.name)          ← 平台 → 子代理 → 授权 → 租约
              1. 平台不支持      → [UNSUPPORTED_PLATFORM]
              2. 子代理          → [SUBAGENT_COMPUTER_USE_UNAVAILABLE]
              3. 会话未授权      → [NOT_AUTHORIZED]（顺手收敛一次 schema 可见性）
              4. 另一个会话在驱动 → [CONTROLLER_BUSY]
        └── if (gate.write) { safety.beginWrite(); …; finally safety.finishWrite() }
```

**为什么租约与授权表放宿主面**：本项目的 `src/tool.js` 明确**不能**注册 `session/event` 监听器
（它在后代作用域，作用域过滤后永远收不到 `turn/end`——README 里记着这段死代码的历史）。
而 `src/index.js` 是宿主面、服务进程级、已经在监听 `session/event` 与 `session/disposed`。
所以**所有跨会话、跨会话生命周期的东西都必须住在宿主面**，预设行只做「这一次调用能不能过」。

**为什么 `src/safety/*` 不依赖任何宿主包**：一个只能在整个宿主里跑起来的策略层，
没有人会反复测试它。所以这四个模块只用纯 ESM，`tests/safety.test.mjs` 用裸 `node --test` 就能跑。
副作用是 `tools.restrict` 的 scope key 拿不到（`@deepseek-ai/dsh-scope` 在本仓库无法解析），
于是摘除走「整批试 → 失败后逐个试 → 全失败则保持『可见但被拒』」的三级降级，
而不是去猜一个 scope key。（源项目靠 `scopeOf` 精确过滤，本项目靠**降级**达到同样的安全结果。）

**首次调用的收敛窗口（已知且接受）**：`agent/created` 是串行监听器，早于本插件挂载的
会话错过那次登记；它的**第一次**工具调用会先看到完整 schema（为此付一次 token）、
在 gate 拒绝时被 `onNotAuthorized → visibility.sync()` 现场收敛。这是一次性的自愈路径，
不是缺陷：窗口大小是「每个早于插件挂载的会话各一次调用」，之后该会话的 schema 一直
与授权状态一致。要零窗口就得让注册顺序可保证，那会把插件变成宿主的核心依赖——不值。

### 6.3 配置项（新增，均有向后兼容的默认值）

宿主面 `src/index.js` 的 `Config`（扁平键，与本项目既有风格一致）：

| 配置 | 默认 | 含义 |
| --- | --- | --- |
| `safetyDefaultEnabled` | `true` | 能力默认是否开启。`false` 时能力默认关闭、schema 被摘掉（源项目语义）。**默认 `true` 是为了不破坏本项目现有行为**；「每会话开关」的语义由 `grant.revoke()` / `grant.grant()` 提供。 |
| `safetyDenySubagents` | `true` | 子代理是否一律拒绝驱动桌面。 |
| `safetyHideToolSchemasWhenOff` | `true` | 能力关闭时是否摘掉工具 schema（省 token）。 |
| `safetyControllerLease` | `true` | 是否启用跨会话写仲裁。 |
| `safetyControllerLeaseIdleMs` | `10000` | 写操作租约的空闲释放时间。 |

### 6.4 与源项目的两处**有意**分歧

| 项 | 源项目 | 本项目 | 理由 |
| --- | --- | --- | --- |
| 默认是否开启 | **关**（`defaultEnabled === true` 才开） | **开** | 源项目有输入框里的「电脑操作」芯片，点一下就开；本项目**没有前端行**，「默认关闭」等于「默认没有功能」。每会话授权、关掉时摘 schema 的语义完全一致。 |
| 授权入口 | `POST /computer-use/state` + 芯片 UI | `service.safetyGrant(id, enabled)`（装配层叫 `safety.authorize(id, enabled)`） | 本项目没有 chip UI 与 `/computer-use/*` 路由（那是源项目的宿主面形态）。授权翻转与 schema 收敛在同一个调用里完成；宿主/用户从**会话之外**调用，工具执行路径够不到它。（P3-1：之前只有授权表没有钥匙，`safetyDefaultEnabled: false` 的部署没有任何路径能把会话重新打开——本方法补上。） |

### 6.5 兼容性承诺

* 所有新配置项都有**保持现状**的默认值，默认安装下行为不变。
* 授权表默认全开 ⇒ 现有会话不受影响；关掉某个会话的授权时，schema 被摘掉且调用被拒（源项目语义）。
* 租约按**会话 id** 仲裁，单会话永远自持；只有**第二个会话**会被拒，且拒绝消息给出可执行的下一步。
* 子代理拒绝是唯一会改变现有行为的默认项：子代理继承预设层工具时不再能驱动桌面，
  错误消息明确要求「把这一步交回主代理」。
* 浏览器导出文件换了位置（从 `%TEMP%` 根目录移到 `%TEMP%\dsh-computer-use\<owner>\`），
  但**文件名与扩展名不变**，调用方拿到的路径仍然可用——而且现在会被回收。
* **下载目录与浏览器下载文件不动**：那是**用户的产物**，插件只是把路径指给了它，
  按 SEC-05 的豁免规则不是我们该删的东西。

---

## 7. 移植过程中发现并修正的三个问题

移植不是抄写。这三处是**在本项目落地时才暴露**的，且都已被回归测试钉住。

### 7.1 `release_all()` 曾经越权删除

第一版的 `release_all()` 用 `shutil.rmtree(root)` 收尾——它会把根目录里**所有**东西删掉，
包括 janitor 从没登记过的文件。那正是源项目给「调用方指定路径」豁免的那条规则所禁止的：
**我们不知道、也不该删别人的文件。**
现在 `release_all()` 只删**登记过的路径**，然后顺手收掉因此变空的目录（`temp_janitor.py`
的 `_prune_empty_dirs`）。回归测试：`test_a_path_the_janitor_never_saw_survives`。

### 7.2 归属键里的点号是路径穿越的原料

第一版的归属键清洗允许点号，于是 `sanitize_owner("tab/../../etc")` 会留下 `..`。
归属键会**直接变成一个目录名**，所以它必须是文件名里最老实的那种。
现在归属键只允许 `[A-Za-z0-9_-]`，点号只在**产物文件名**里保留（扩展名要留住，
调用方靠它判断文件类型）。回归测试：`test_an_owner_key_is_made_safe_for_a_path`。

### 7.3 清洗名字会把扩展名一起洗掉

同一个改动差点连扩展名也丢了（`export-tab-1.txt` → `export-tab-1-txt`）。
扩展名是**调用方判断文件类型的依据**，必须留住。
现在名字先取 basename、再只替换非法字符、最后才拆扩展名。
回归测试：`test_new_path_lands_inside_the_owned_root` 与既有的
`test_content_context_capabilities_assets_mcp`（断言路径以 `.txt` 结尾）。

---

## 8. 验收

* **JS（新增）**：`node --test tests/safety.test.mjs` → **35 passed / 0 failed**。
  覆盖失败分类学、授权表的三态与匿名会话、子代理判定、schema 摘除与释放形态、
  租约仲裁（读不占、写占、同会话重入、空闲释放、「正在执行 vs 安静 N 秒」、
  空闲时钟从写完那一刻重启）、契约的控制状态与会话结束清理，
  以及审计收尾新增的 `authorize()`（钥匙开/关会话、授权翻转与 schema 收敛同帧、
  匿名会话可授、无活 agent 时授权仍然成立）。
  这个套件**不依赖任何 `@deepseek-ai/*` 包**——`src/safety/*` 刻意没有宿主依赖，
  所以策略层可以脱离宿主单测。
* **Python（新增）**：`python -m pytest tests/test_temp_janitor.py` → **17 passed / 0 failed**
  （含 P3-3 回归：同根双实例 32 轮交叉命名零冲突、扩展名保留）。
* **Python（回归）**：`python -m pytest tests -q` → **333 passed / 8 skipped**
  （改动前是 316 passed / 8 skipped，新增的 17 条就是上面的新套件）。
* **Rust**：不改动 `helper-rs/`，无需重编译。
* **审计收尾（P3-1..P3-5）**：本节之外，本分支还落了审计指出的五个 P3——
  P3-1 授权钥匙（`safety.authorize` / `service.safetyGrant`，见 §6.4）、
  P3-2 CONTROLLER_BUSY 拒绝消息不再指向不存在的「会话内关闭」入口、
  P3-3 janitor 命名加随机后缀防跨实例冲突、P3-4 `track()` 死自增清理、
  P3-5 首调用收敛窗口补记（见 §6.2）。
* **已知环境问题（与本次改动无关）**：`npm run test:js`（既有的
  `tests/contracts.test.mjs` / `tests/interactive_summary.test.mjs`）在本机**改动前就已失败**——
  本仓库的 `node_modules` 是一组指向 DSH checkout 的符号链接，而
  `@deepseek-ai/schemastery` 等包在当前磁盘布局下无法解析
  （`ERR_MODULE_NOT_FOUND`）。这属于宿主依赖解析问题，不是本次移植引入的。
  新增的安全契约套件不依赖这些包，因此可以正常跑。

---

## 附录：源项目关键代码位置索引

| 主题 | 位置 |
| --- | --- |
| 五条契约的完整清单 | `README.md` §9「权限与边界」、`internal/lease.mjs`、`internal/schema-visibility.mjs`、`internal/capture.mjs` |
| 授权入口 | `index.js:654-669`（`requireGrant`）、`index.js:223`（`SUBAGENT_REFUSAL`） |
| 租约实现 | `internal/lease.mjs:24`（`CONTROLLER_IDLE_MS`）、`:34`（`MUTATING_COMMANDS`）、`:62-80`（`claimController`） |
| schema 摘除 | `internal/schema-visibility.mjs:63-69`（`HIDEABLE_TOOL_NAMES`）、`:84-87`（`liftRestriction`）、`:152-191`（`sync`） |
| 失败分类学 | `internal/failures.mjs:58-101`（`FAILURE_RULES`）、`:59`（AV）、`:63`（UAC/UIPI）、`:64`（锁屏） |
| 截图记账与删除 | `internal/capture.mjs:215-226`（归属规则）、`:252-282`（`removeFiles`）、`:291-310`（`releaseSession`/`releaseAll`） |
| 安全软件拦截 | `internal/security-block.mjs:14-58` |
| 降级挂载 | `README.md` §1.40.3「降级挂载」、`internal/degraded.mjs` |
