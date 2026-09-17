# X11 Headless 会话运行器与冒烟验证套件

`scripts/x11-headless/` 提供了在 Linux 下基于 `Xvfb` 的无头（Headless）X11 会话管理与协议冒烟验证基建。

---

## 1. 架构组件

```text
scripts/x11-headless/
├── README.md              # 本说明文档
├── session.sh             # 核心会话管理器：负责 Xvfb / WM 生命周期、环境变量与退出清理
├── run-smoke.sh           # P1 一键冒烟运行器：自检/编译二进制、起会话、驱动 7 工具、出报告
├── driver.py              # P1 stdio JSONL 驱动器：覆盖 health/tools/prompt/拒绝/7 工具/生命周期
├── summary.py             # 结果分析与汇总器：验证协议契约，输出终端报表与机器可读 JSON 摘要
├── run-window2-e2e.sh     # P2 全表面端到端运行器：起会话 + 起 xterm，驱动 window2 13 方法
└── window2_driver.py      # P2 window2 驱动器：13 方法逐一调用 + 原生截图路径与 PNG 校验
```

---

## 2. 快速使用

### 2.1 一键运行协议冒烟测试
```bash
./scripts/x11-headless/run-smoke.sh
```
此脚本执行完整流水线：
1. 查找或构建 `dsh-computer-use` 二进制（`helper-linux/target/debug/dsh-computer-use`）；
2. 启动 Xvfb 虚拟屏幕（默认 `:99`，分辨率 `1280x800x24`）；
3. 检查并按需启动 Window Manager（若缺少 openbox 则以 WM-less 模式启动并给出警示）；
4. 通过 `driver.py` 向 helper 发送协议请求并记录响应；
5. 生成控制台彩报及结构化 JSON 摘要：`artifacts/x11-smoke-summary.json`；
6. 自动回收并杀灭所有 Xvfb、WM、子进程，确保 0 孤儿进程残留。

### 2.2 运行 window2 全表面端到端测试（P2）
```bash
./scripts/x11-headless/run-window2-e2e.sh
```
与 `run-smoke.sh` 互补：冒烟测试覆盖 P1 `sky.window` 表面（7 工具），此脚本覆盖官方 window2 表面
（13 方法）。它在同一虚拟屏上另起一个 `xterm` 作为被测窗口，然后断言 Xvfb 下**可以**证实的性质：

1. `tools` 在 `surface=computer` 上恰好广告官方 13 个方法；
2. `list_windows` 枚举到 xterm，句柄为数值型稳定值，`get_window` / `list_apps` 与之自洽；
3. **截图必须走 X11 原生路径**：`get_window_state` 报告的 `method` 为 `composite` 或 `direct`，
   **不能**是 `xdg-desktop-portal`。这是本检查存在的原因——portal 会截到真机桌面而非 Xvfb 屏，
   本机表现为「截图看似成功但内容是错的」；
4. 图像以独立 image part 返回合法 PNG，解码尺寸与声明的窗口尺寸一致，且 JSON `value` 内不夹带
   base64 像素（只保留空 data-URL 占位）；
5. 窗口相对点击**确实送达**：由驱动自行 map 的探针窗（`list_windows` 可枚举）收到真实
   `ButtonPress`，事件坐标即请求的窗口相对坐标；对 xterm 另以服务端指针位移验证平移量；
6. 其余方法均由 window2 分发器应答，包括 X11 下合法拒绝的 `launch_app` / `set_value` /
   `perform_secondary_action`（拒绝文本是 window2 的，而非 P1 守卫的 `unsupported method`）；
7. call-surface 契约：带 surface 的同名方法走原生 handler，不带 surface 的保持 P1 行为。

产物：`parity/x11/artifacts/x11-window2-e2e-summary.json`（含逐项结果与 `residual_risks`）。

### 2.3 使用 `session.sh` 运行自定义命令
可以在虚拟 X11 会话内直接运行任何图形或协议测试命令：
```bash
# 启动会话并查看 X11 显示信息
./scripts/x11-headless/session.sh xdpyinfo

# 启动会话运行单个 Python 驱动
./scripts/x11-headless/session.sh python3 -c 'import os; print("DISPLAY:", os.environ.get("DISPLAY"))'

# 自定义显示号与分辨率
XVFB_DISPLAY=101 XVFB_RES=1920x1080x24 ./scripts/x11-headless/session.sh xdpyinfo
```

---

## 3. 环境变量与参数配置

`session.sh` 支持通过环境变量调整虚拟显示器行为：

| 变量名 | 默认值 | 说明 |
| :--- | :--- | :--- |
| `XVFB_DISPLAY` | `99` | 显示器编号（脚本自动转为 `:99`） |
| `XVFB_RES` | `1280x800x24` | 分辨率与色深 |
| `XVFB_TIMEOUT` | `10` | 等待 Xvfb 启动就绪的最大超时秒数 |
| `SMOKE_ARTIFACTS_DIR` | `<repo>/parity/x11/artifacts` | 冒烟输出文件目录 |
| `WINDOW2_E2E_ARTIFACTS_DIR` | `<repo>/parity/x11/artifacts` | window2 端到端输出文件目录 |

---

## 4. 产出物（Artifacts）说明

冒烟测试执行后会在 `parity/x11/artifacts/` 目录生成两份文件：
1. `parity/x11/artifacts/x11-smoke-responses.jsonl`：helper 的原始响应行（包含所有请求 ID、状态及数据）。
2. `parity/x11/artifacts/x11-smoke-summary.json`：结构化报告，包含：
   - `environment`：显示号、会话类型、WM 状态、EWMH 限制说明；
   - `helper`：可执行文件退出码、surface 名称、暴露工具列表；
   - `protocol_contract`：健康状态、工具枚举、Prompt 长度、错误拒绝等契约是否全过；
   - `tools_7_status`：`list_apps` / `get_app_state` / `screenshot` / `click` / `scroll` / `press_key` / `type_text` 各自的具体执行反馈；
   - `diagnostics`：AT-SPI bus 就绪状态与降级指标列表。

---

## 5. 关键保障与边界说明

1. **零孤儿进程（Zero-Orphan）保证**：
   `session.sh` 在 `EXIT`、`SIGINT`、`SIGTERM`、`SIGHUP` 上挂载了严格的 trap 回收逻辑。首先发送 `SIGTERM` 优雅终止，若超时未退则追加 `SIGKILL` 强杀，并清理 `/tmp/.X11-unix/X<N>` 与 `/tmp/.X<N>-lock`。
2. **WM-less 降级策略**：
   若系统中未安装 `openbox`，运行器不会尝试未经授权的 `sudo apt-get` 操作，而是无感降级到 WM-less 模式继续服务，并在日志及报告中标明 `ewmh_limited: true`。一旦未来环境安装了 openbox，运行器将自动检测并启动它。
