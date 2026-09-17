# Linux / X11 Headless Computer Use Parity — 验收清单

> 来源：基于 `parity/CHECKLIST.md` 与 `.agents/notes/proposed/feature/2026-09-16-p2-window2-x11.md`。
> 准则：每条都能被「一条命令 + 一次观察」证伪。`[x]` = 已用命令验证通过；`[ ]` = 待后续子阶段实现后点亮。
> 测试红利：所有检查项均在 Xvfb 虚拟显示服务器中无头（Headless）无人值守自动运行，不依赖真实实体桌面。

---

## 0. 自动化套件（Headless Suite Automation）

- [x] **一键运行 X11 Headless 冒烟驱动与全契约验收**：
      `./scripts/x11-headless/run-smoke.sh` → **PASS** (15/15 RPC calls OK)
      覆盖：health / tools (sky.window 7 工具) / prompt / 3 项协议级拒绝 / 7 真实工具执行 / 机器可读 JSON 摘要生成 / 零孤儿进程清理退出
- [x] **一键运行 Linux/X11 Parity 首批自动化门禁**：
      `./parity/x11/run-parity.sh` → **3 / 3 PASS**
      覆盖：应用枚举结构断言、全屏 PNG 截图魔数与字节契约、输入注入分发与反序列化拒绝
- [ ] **完整 window2 13 方法 parity 闭环**（待 core 与 experience 线集成后点亮）：
      `./parity/x11/run-parity.sh` → **N / N PASS**

---

## 1. 协议与 CLI（Protocol & CLI Envelopes）

- [x] `health` 正常返回能力与降级检测集（X11 探测状态可读，AT-SPI bus 可探知）
- [x] `tools?surface=sky.window` → P1 阶段提供 7 工具表面（`list_apps`, `get_app_state`, `screenshot`, `click`, `scroll`, `press_key`, `type_text`）
- [ ] `tools?surface=computer` → P2 阶段提供 13 工具 Codex 对齐表面（`list_windows,get_window,list_apps,launch_app,get_window_state,click,press_key,type_text,scroll,set_value,drag,perform_secondary_action,activate_window`）
- [x] 未知方法拒绝：`{"method": "not_a_method"}` → `ok: false`, `unsupported method: not_a_method`
- [x] 未知工具拒绝：`{"name": "nope_not_a_tool"}` → `ok: false`, `unsupported method: nope_not_a_tool`
- [x] 损坏 JSON 行响应：输入 `{"id": 6, "method": }` → 仍以 `id=6` 给出 `ok: false` 错误信封
- [x] 会话生命周期：`end_turn` 返回 `ended: true`, `shutdown` 返回 `closed: true` 且 helper 进程正常退出码 0

---

## 2. 应用与窗口枚举（Enumeration / Window Discovery）

- [x] **应用枚举基础契约**（`parity/x11/checks/01_enum_apps.py`）：
      `list_apps` 返回 `ok: true`，`apps` 为合法应用对象列表，包含非空 `name` 与 `pid >= 0`，`accessible_apps` 为合法列表
- [ ] **EWMH 窗口枚举**（P2 core）：
      `list_windows` 在 X11 下通过纯 Rust `x11rb` 遍历 `_NET_CLIENT_LIST` / root children，返回稳定句柄与窗口标题
- [ ] **窗口聚焦与激活**（P2 core）：
      `activate_window` 向根窗口发送 `_NET_ACTIVE_WINDOW` ClientMessage，正确设置前台焦点
- [ ] **窗口状态详情**（P2 core）：
      `get_window_state` 查询窗口 rect、minimized/maximized 状态及 accessibility 树

> ⚠️ **环境说明**：当前容器/系统未安装 openbox 等独立 WM（sudo 需密码已按规降级）；无 WM 状态下基础枚举正常，但 EWMH 属性（如 `_NET_ACTIVE_WINDOW`）在缺少 WM 时受限，待 WM 部署或 mock 接入后即可验证完整 EWMH 特性。

---

## 3. 屏幕与窗口捕获（Capture / Screenshot）

- [x] **全屏截图契约**（`parity/x11/checks/02_screenshot_contract.py`）：
      `screenshot` 返回非空 images 列表，MIME 为 `image/png`，解码后的二进制流严格以 PNG 魔数（`\x89PNG\r\n\x1a\n`）开头，字节长度与返回元数据一致，宽高非零
- [ ] **XShm 共享内存全屏直采**（P2 core）：
      X11 会话下利用 `MIT-SHM` 协议扩展直接从 Xvfb framebuffer 读取，绕过 portal 弹窗
- [ ] **未被遮挡窗口捕获（Unoccluded Capture）**（P2 core）：
      当窗口位于最顶层且无遮挡时，通过局部 rect 裁剪或 XShm 直采获取精准像素
- [ ] **被遮挡窗口捕获（Occluded Capture via XComposite）**（P2 core，S0 已验证路径）：
      当目标窗口被上层窗口完全或部分遮挡时，必须走 XComposite `redirect_window` + `NameWindowPixmap` 机制获取窗口独立 backing pixmap，断言产出内容为「窗口自身内容且不含遮挡物像素」；测试程序在 Xvfb 绘制时遵循「map 后等待 Expose 事件再绘制」纪律防止背景重绘假失败

---

## 4. 输入注入（Input Injection）

- [x] **输入调用与反序列化校验**（`parity/x11/checks/03_input_dispatch.py`）：
      `click(x=100, y=100)` 参数完整回显；`scroll(direction="down", pages=1.0)` 参数完整回显；缺失必填项（如 `direction`）时由 serde 模式精确拒绝（`ok: false`）
- [ ] **XTest 纯 Rust 鼠标注入**（P2 core）：
      通过 `x11rb` XTEST 协议扩展直接发射 MotionNotify / ButtonPress / ButtonRelease 事件，Xvfb 事件队列能捕获到光标移动与点击
- [ ] **XTest 纯 Rust 键盘与文本注入**（P2 core）：
      通过 `x11rb` XTEST 协议扩展 + `xkeysym` 转换发射 KeyPress / KeyRelease 事件，无需外置 `xdotool` / `ydotool` 二进制

---

## 5. 语义与辅助功能（AX Tree / Diffing）

- [ ] **AT-SPI 元素索引稳定**（P2 core）：
      通过 a11y bus 获取窗口内元素树，节点带有连续数字索引与角色名
- [ ] **树比对（Tree Diffing）**（P2 core）：
      连续两次观察间仅输出变化节点（`removed:` / `added/changed:`），无变化时输出 `no accessibility-tree change`

---

## 6. 体验层（Experience / Overlay / Synthetic Cursor）

- [ ] **Override-Redirect 状态药丸**（P2 experience）：
      创建带 `override_redirect=True` 的 X11 窗口作为状态浮层，不被 WM 管理也不抢占焦点
- [ ] **XFixes 光标压制与自绘合成**（P2 experience）：
      通过 XFixes 扩展隐藏系统指针，在截图前自绘合成光标，且尖端精确对齐目标坐标
- [ ] **XInput2 原始事件与新鲜度租约**（P2 experience）：
      监听 XI2 RawMotion / RawButtonPress，产生外部人为干扰时使观察租约失效
- [ ] **Esc 键全局抓取中断**（P2 experience）：
      在根窗口上通过 `XGrabKey` 注册 Escape 监听，按下时产生取消标记并打断长任务
