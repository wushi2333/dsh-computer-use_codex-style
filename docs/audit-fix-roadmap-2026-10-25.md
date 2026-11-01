# 近5天提交审计与修复推进计划（2026-10-25）

审计范围：`9d83ffc^..HEAD` 共 9 个提交（80c103a → 0d80e28），覆盖 helper-rs（Rust sidecar，约 +2700 行）、
computer_use（Python 驱动层）、src（JS 插件层）与 tests/skills/scripts。方法：逐提交 diff + HEAD 全文实读；
JS 测试实测（contracts + interactive_summary：52 项 / 51 过 / 1 合理跳过）；Python 测试静态审读（本机无 pytest）；
helper 二进制已核实与 0d80e28 源码同步重建。

## 需求与约束基线（问题定级的依据）

dsh web 上的重度批量自动化电脑操作 Agent 的实际约束：
1. **长时间运行**：会话以小时计，内存/句柄/陈旧缓存泄漏会累积成故障。
2. **中文 IME 环境**：合成输入不能破坏用户后续的中文输入。
3. **UIA 与坐标混合操作**：UIA 挂了必须能降级到截图+坐标，不能整体停摆。
4. **流水线不能被单点拖死**：一个 wedged 窗口最多损失该窗口的操作。
5. **模型是调用方**：工具入参不可信，任何内部标志（如 `_lease_ok`）不得可被外部注入。
6. **结果体积与 token**：批量 get_window_state 返回是上下文大头。

---

## P1 — 立即修（批量流水线上会真实踩中的正确性/安全性回归）

### P1-1 IME 布局"恢复"是硬编码假恢复（中文输入法被永久切英文）✅ 已修复（2026-10-25）

install() 现按目标窗口线程 GetKeyboardLayout 保存真实 HKL，Drop 恢复真实布局（查询失败才回退 EN-US）。helper-rs 231 项测试通过，二进制已重建发布（2765824B）。恢复失败诊断计数属 P2-1，未做。
`helper-rs/src/ime.rs:105`、`ime.rs:143-160`。install() 把 previous_hkl 硬编码为 `LANG_EN_US`，
从未调用 GetKeyboardLayout 保存目标线程真实布局；Drop 时只是把英文布局再贴一遍。每次 type_text /
press_key / type_unicode 后目标窗口线程停在 EN-US。
**修复**：install 时按目标窗口 tid 取真实 HKL 保存，Drop 恢复；恢复失败计入 diagnostic_state（联动 P2-1）。

### P1-2 AX-WEDGE 只解决"等待方超时"，未解决"执行方回收" ◐ 近期部分已修复（2026-10-25）

已落地：monitor 线程每个 job 包 catch_unwind（panic 后清 MONITOR_CURRENT、窗口进 wedge 冷却、线程存活继续服务）；线程 spawn 失败不再 .expect 崩掉 helper。根治方案（watchdog/线程重建/per-window 隔离、busy 文案升级）留第 2 批。
`helper-rs/src/uia.rs:965`（wait_reply）、`uia.rs:1089`（单 monitor 线程）、`uia.rs:930-945`（precheck）。
客户端超时返回了，但卡死的 COM 调用仍永久占着唯一 monitor 线程：无备用线程、无重启、无取消。期间
set_value / focused_value / secondary / value_at（type_text、set_value、fill_form 每步依赖）全部立即
"monitor busy" 直到 helper 被杀。只有 dump 有占位树降级。
**修复（分两步）**：
- 近期：mutate/read 类 job 提供降级路径（坐标回退）或至少把"busy"错误文案升级为可操作指引；
- 根治：monitor 线程 watchdog（stuck 超阈值后放弃该 job、重建线程）或 per-window job 隔离。注意 COM STA
  被占死时线程确实无法强回收，watchdog 方案需评估泄漏窗口；配套 catch_unwind（见 P2-7）。

### P1-3 type_text 防重复粘贴以"漏贴静默报成功"为代价 ✅ 已修复（2026-10-25）

text_landed_settling 对"Unverifiable 且 before 值可读"增加一次 settle 后复验：陈旧异步提交可升级为已证实的 Landed/Wrong 而非静默成功；不可读目标的 Unverifiable 仍立即返回。防双贴优先级保持不变。
`helper-rs/src/main.rs:982` + `main.rs:1094`。before==after 判 Unverifiable，failed()=false，不重试。
真实漏贴（IME 吃掉 Ctrl+V 且未留残字符、值可读且不变）→ 假成功。
**修复**：Unverifiable 且 before 可读非空时，加长二次 settle 后用 value_at 定点复验一次，仍不变则报
unverifiable 错误而非成功。

### P1-4 lastElements 陈旧缓存跨窗口污染点击目标 ✅ 已修复（2026-10-25）

get_window_state 成功时连带记录观察窗口标识（state.lastWindow）；text 选择器仅在调用窗口与观察窗口一致（app+id 匹配）或未指定窗口时才使用缓存，否则按"无匹配"报错并提示重新观察。turn 结束清空未做（无现成 turn 生命周期钩子，跨窗口校验已消除主要风险）。
`src/tool.js:421-428`、写入点 `tool.js:487`。缓存不带窗口/应用标识、永不过期、turn 结束不清空。
A 窗口观察后对 B 窗口 click({window:B, text:"确定"}) 会静默改用 A 的 element_index 点错控件；匹配是
大小写不敏感子串首命中，歧义文本必错。
**修复**：缓存记录 window app/id，执行时校验当前 window 参数一致，不一致按"无观察"报错；turn 结束清空。

### P1-5 `_lease_ok` 内部标志可被外部注入，绕过观察租约 ✅ 已修复（2026-10-25）

实际范围比初判更窄：executor 入口即 pop 该字段，普通动作注入本就无效；真正的洞是**外部 fill_form 带 `_lease_ok: true` 可跳过前置租约校验**（整体盲填），另有一处一致性 bug——fill_form 内部步骤注入的标志在到达 driver 前被 pop，`driver.py:_before` 的免重验检查实为死代码（复合动作语义在 Python 路径未真正生效）。修复：`execute()` 公共入口剥离全部下划线前缀键；新增内部 `_execute(already_leased=...)` 通道，fill_form 内部步骤走内部通道注入标志；`driver._before` 消费标志（pop）防泄入后端 payload。回归测试 `tests/test_lease_guard.py`（3 项），全量 323 项 Python 测试通过。
`computer_use/executor.py:59-66`（`spec.pop("_lease_ok")`）、`computer_use/driver.py:196`、
`computer_use/freshness.py`（fill_form 也加入 INPUT_ACTIONS）。JS 工具层 `src/tool.js:102/301`
additionalProperties:true 全透传——模型在普通 click 调用里带 `"_lease_ok": true` 即可对未观察窗口盲操作，
租约防线整体失效。fill_form 的意图（整批只验一次）是对的，但实现把内部标志暴露在公共入参命名空间。
**修复**：入口处（execute/execute_call）剥离所有 `_` 前缀参数；内部传递改用显式函数参数
（_dispatch 已有 lease_ok 形参，Python 侧 fill_form 内部调用走 execute_call 时改走内部通道即可）。

---

## P2 — 尽快修（1-2 周内）

| # | 问题 | 位置 | 修复要点 |
|---|---|---|---|
| P2-1 | ImmAssociateContext 竞态/恢复失败无监督；helper 被杀时目标窗口 IME 永久 detach | ime.rs:102 | 恢复失败计入 diagnostic_state；启动时检测残留 detach；评估第三方输入法（搜狗自绘候选窗）兼容 |
| P2-2 | fill_form 不带 window 时零校验（不观察也不 require 租约）→ 盲填 | executor.py:_fill_form | target 非 dict 时直接拒绝或先枚举窗口要求显式指定 |
| P2-3 | 锁桌面判定反转为白名单：任何非 Default 桌面（流水线/第三方自建桌面）都被当锁屏拒绝全部输入 | computer_use/detect.py:60-64、helper-rs/src/desktop.rs:82 | 回到黑名单（winlogon/screensaver 系列）；PYTEST_CURRENT_TEST 逃生门移到测试 fixture |
| P2-4 | monitor 线程内注入键鼠（replace_text_for 剪贴板+SendInput+sleep 跑在 UIA monitor 线程），期间所有 UIA 排队 | uia.rs:2585/2596/2608 | 输入回退挪回主线程 |
| P2-5 | monitor 线程无 catch_unwind：panic 后 MONITOR_CURRENT 残留，全 UIA 永久 busy；spawn 处 .expect | uia.rs:1566、uia.rs:1090 | catch_unwind + 线程重建 + 状态复位 |
| P2-6 | 遮挡探测只采 4 角+中心 5 点，小浮层（验证码/下拉）漏判 | enum_windows.rs:979-1010 | 网格加密采样或可疑 rect WindowFromPoint 全检，失败安全方向 |
| P2-7 | bump_to_top 无条件 TOPMOST 闪顶，高频动作下永久打乱用户 z 序、可能唤出托盘隐藏窗口 | enum_windows.rs:1067/1167 | 仅在确认被遮挡时提升；还原原 z 序 |
| P2-8 | 重贴 Ctrl+A 门槛失焦误判时仍可能翻倍粘贴 | main.rs:754/767 | WM_PASTE 支线同样过 focused_accepts_text 门槛 |
| P2-9 | 删除型提交收尾欠账：package.json:37 / ci-smoke.ps1:63 / README.md:334 仍引用已删除的 tests/experience.test.mjs（node 24 静默忽略缺失文件，CI 不红） | package.json、scripts、README.md/README.zh-CN.md、docs/plugin-readme.md | 清理全部 experience.* 文档/配置表/文件树；ci-smoke 阶段描述改实；package.json 只列现存测试 |
| P2-10 | Config.preset 死配置（消费方已删、选项残留） | src/tool.js:92-97 | 删除或标废弃 |
| P2-11 | formatWindowStateBody 名不副实：tree 全量 + pretty-print，批量场景返回虚胖 1.5-2 倍 | src/tool.js:823-836 | 紧凑序列化或对 accessibility.tree 设预算 |

---

## P3 — 排期修（工程卫生与防再发）

1. wedged 表过期条目不清理，长会话累积死条目、diagnostics 报 cooldownRemainingMs:0（uia.rs:827）。
2. 二进制同步防再发：仓库无机制防"改源码忘重建"（9 提交中 6 个动过 exe）。建议 exe 内嵌 git hash 并在
   health 回读校验，或 CI 校验重建哈希；+184KB/周期的膨胀考虑 LFS 或按 tag 发布。
3. src/tool.js:482-484 catch 后直接 rethrow 的死代码（recordCall 钩子残壳）删除。
4. desktop.rs 空前台 6×50ms 轮询，最坏加 300ms 延迟。
5. set_value 防御分支对异步提交 provider 多打一轮剪贴板/键鼠（幂等，仅效率）。
6. 无问题确认项（无需动）：sidecar.js 进程管理（超时击杀+pending 全拒、unref、--parent-pid 双保险、
   chain 串行化）；dsh-plugin.json / SKILL.md 与 digest 下线一致；"先建后删"决策正确（digest 注入与受控
   Skills 职责重复，长会话能力未误删）；新增测试质量高、无 skip 掩盖。

---

## 第 2 批（P2）进展 — 2026-10-25 完成

| 项 | 状态 | 说明 |
|---|---|---|
| P2-2 fill_form 盲填 | ✅ | 无 window 直接拒绝（fail closed），新增测试断言 |
| P2-3 锁桌面白名单反转 | ✅ | Python detect.py 与 Rust desktop.rs 均改回黑名单（winlogon/screensaver/disconnect/lock） |
| P2-4 monitor 线程内注入键鼠 | ✅ | uia.rs 三处 replace_text_for 回退改为报错，由 main.rs 既有的主线程坐标回退接管；同步更新源码契约测试 |
| P2-5 monitor 无 catch_unwind | ✅ | 已随第 1 批 P1-2 落地（逐 job catch_unwind + spawn 容错） |
| P2-6 遮挡探测 5 点采样 | ✅ | 加 3x3 内部网格共 14 采样点 |
| P2-7 TOPMOST 闪顶 | ✅ | 仅在 is_covered 为真时才 bump；去掉 SWP_SHOWWINDOW |
| P2-8 重贴门槛漏网 | ◐ 无需 | 复核发现当前代码两条重贴路径（含 WM_PASTE 支线）均已过 focused_accepts_text 门槛，无需改动 |
| P2-9 experience 残留 | ✅ | package.json/ci-smoke.ps1/README.md/README.zh-CN.md 全部清理，测试计数更新 |
| P2-10 Config.preset 死配置 | ✅ | 已删，留一行来源注释 |
| P2-11 结果瘦身 | ✅ | stringifyCompact 改真紧凑序列化；accessibility.tree 超 24k 字符中间裁剪 |
| P2-1 IME 恢复失败监督 | ⏳ 未做 | 与 P1-2 根治方案一起留待下一批 |

验证：Python 324 过/4 跳过、JS 51 过/1 跳过、Rust 231 过；helper 二进制已重建发布（2753536B）。

## 第 3 批（P3）进展 — 2026-10-25 完成

- **P3-1 wedged 表清理** ✅：`wedged_until` 见到过期条目即删除，`wedge_diagnostics` 输出前 retain 过滤——不再累积死条目/报 cooldownRemainingMs:0。
- **P3-2 二进制同步校验** ✅：helper 编译期嵌入 git 短哈希（`option_env!("DSH_CU_BUILD_GIT")`，ship-helper.ps1 注入），`health` 载荷携带 `revision`；`DSH_CU_PRINT_BUILD=1` 环境变量可直接打印（不动 argv，保住官方 CLI 契约）。ci-smoke.ps1 新增 `HelperBinaryRevision` 门禁：二进制哈希必须是 HEAD 的祖先（鸡生蛋问题：二进制不可能嵌包含它自己的提交），改源码忘重建会红。
- **P3-3 tool.js 死代码** ✅：裸 try（recordCall 残壳）删除。注：初版编辑留下裸 try 导致语法错误并被 JS 测试当场抓住——契约测试的价值自证。
- **P3-4 前台空轮询** ✅：6×50ms 收敛为 3×50ms（仅锁屏过渡期 NULL 前台会走到）。
- **P3-5 set_value 防御分支** ✅：已随 P2-4 一并移除。
- **P3-6 无问题确认项**：无需动作。

验证：Python 324 pass/4 skip、JS 51 pass/1 skip、Rust 231 pass；二进制以 5ed0026 重建（2754048B）。

## 第 4 批（收尾）进展 — 2026-10-25 完成：全部审计项闭环

- **P1-2 根治：monitor watchdog + 线程重建** ✅：接收端共享化（MONITOR_RX，锁仅在 recv 期间持有）+ 代数标记（MONITOR_GEN/MONITOR_SERVED_BY）。watchdog 线程每秒检查，当前 job 卡超过 wedge_stuck_ms 时标记窗口 wedged 并拉起**新代 monitor** 继续服务——被卡死的 COM 线程被放弃（无法安全销毁，COM 调用返回后因代数过期自行退出，且不得清掉新 monitor 的在途记账）。代价（已注释说明）：新线程 = 新 Engine，元素缓存丢失，索引动作会以既有 runtime-id mismatch 报错、需重新观察——可存活的降级路径。每个卡死 job 只重建一次（SERVED_BY 判重）。
- **P2-1 IME 恢复失败监督** ✅：`ime.rs` 新增 RESTORE_FAILURES 计数（Drop 恢复 PostMessageW 失败时递增，不再 `let _ =` 吞掉），`health` 载荷新增 `ime.restoreFailures`。注：“helper 被杀后残留 detach 的启动期恢复”不可行（HIMC 句柄跨进程无效），监督计数 + health 暴露即本项可交付范围。

验证：Rust 232 pass（含新增代数推进测试）、Python 324 pass/4 skip；二进制以 c917741 重建（2769920B）。

## 第 5 批 — 最小化恢复后 UIA provider 唤醒（2026-10-25）

**现象**：v2rayN 7.25.6（Avalonia）最小化约 1 小时后，`activate_window` 正常恢复窗口，但随后
两次 `get_window_state`（间隔 65s）都在 15s dump 等待上限处超时；第二次超时时监控上的陈旧
dump job 已运行超过 `wedge_stuck_ms`（30s），把窗口打进 60s wedge 冷却。短最小化（10s/120s）不复现。

**根因**：`activate_hwnd` 恢复最小化窗口只等 100ms 就返回，而长最小化后 Avalonia 的 UIA
provider 处于休眠态，被动等待不会唤醒它；能唤醒它的是一次真实的 UIA 读取。

**修复**：
- `activate_hwnd` 的最小化恢复分支新增 `warm_up_uia_after_restore()`：在预算内（默认 12s，
  `DSH_CU_AX_RESTORE_WARMUP_MS` 可调，`0` 禁用）以 250ms 间隔重试一次廉价的
  `window_contains(hwnd, "")`（空 needle = dump 通道应答即成功）；失败中性——probe 自己挣来
  的 wedge 冷却由 `clear_probe_cooldown()` 在循环结束时清除，真死的 provider 会在调用方的
  下一次 dump 走正常路径重新挣冷却。
- `uia.rs` 暴露 `clear_probe_cooldown()`（包装私有 `clear_wedged`）。
- 完整性受限的窗口直接跳过 warm-up（读了也不会成功，不烧预算）。
- 回归测试：`restore_warm_up_budget_parses_override_disable_and_garbage`。

**构建注意**：沙箱内 MSVC link.exe 建临时文件偶发 LNK1104（9 试 8 败），改用 rust-lld
（`CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER` 指向 `~/.rustup/.../rust-lld.exe`）后稳定；
常规控制台里 `scripts/ship-helper.ps1`（MSVC + 自动打 High 完整性标签）不受影响。
本批二进制 2763776B（rust-lld，revision 1bbe4ad），尚待连同源码改动一起提交。

## 修复推进批次（建议顺序）

**第 1 批（P1，约 2-3 天）— 止血正确性**
P1-5（_lease_ok 剥离，半天，纯 Python+JS，风险最低先做）→ P1-1（IME 真实布局保存/恢复，1 天）
→ P1-4（lastElements 绑定窗口+turn 清空，半天）→ P1-3（Unverifiable 二次复验，半天）
→ P1-2 近期部分（busy 错误文案升级 + catch_unwind/线程重建，即 P2-5 一并做）。
验收：新增回归测试——中文布局 type_text 后布局还原；A/B 窗口 text 点击不串窗口；外部注入 _lease_ok
被拒；漏贴场景报 unverifiable 而非成功；模拟 monitor panic 后 UIA 可恢复。

**第 2 批（P2 功能风险，约 1 周）**
P2-2/P2-3（盲填与桌面白名单，Python 侧，先行）→ P2-5/P2-4（monitor 线程健壮性）→ P2-6/P2-7/P2-8
（遮挡采样、TOPMOST、重贴门槛）→ P1-2 根治（watchdog/隔离，涉及 COM 生命周期，单独评审）。
验收：fill_form 无 window 拒绝；自建桌面不再误判锁屏；遮挡小浮层用例检出；连续 8 小时批量回归无
monitor busy 卡死。

**第 3 批（P2 工程卫生 + P3，约 2-3 天）**
P2-9/P2-10/P2-11（experience 残留清理、死配置、结果瘦身）→ P3 全部。二进制同步校验机制（P3-2）建议
提前到本批第一位，防后续批次再犯。

## 总体评价

这批提交方向正确、根因注释与实测记录极佳、测试覆盖是同类代码少见的认真；但两处 P1（IME 假恢复、
AX-WEDGE 不回收）说明是"高质量的止血，未完成的根治"，删除型提交收尾卫生差。按上述批次推进可在一个
迭代内把批量自动化场景的主要风险收敛。
