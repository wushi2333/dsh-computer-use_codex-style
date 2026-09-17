# Linux / X11 Headless Parity 体系

本文档定义了 `dsh-computer-use` 在 Linux X11 模式下的无头（Headless）Parity 验收体系。

---

## 1. 设计理念与背景

- **继承既有方法论**：严格对齐工作区根目录下 `parity/CHECKLIST.md` 的核心准则——**每条都能被「一条命令 + 一次观察」证伪，拒绝凭感觉声称完成**。
- **X11 Headless 测试红利**：在 Linux 下，借助 `Xvfb` 虚拟显示服务，全部 13 个 Codex 风格 window2 方法、合成光标、状态药丸、截图与输入均可在无头环境中自动运行，实现比 Windows 侧更轻量、更高可复现性的无人化 CI 回归。
- **与既有文件严格隔离**：原有根目录下的 `parity/*.ps1` 和 `parity/*.mjs` 为 Windows 专用验证套件。Linux/X11 专用套件一律放置在 `parity/x11/` 与 `scripts/x11-headless/` 中，只增不改，杜绝跨平台污染。

---

## 2. 目录拓扑

```text
parity/x11/
├── CHECKLIST.md           # Linux/X11 验收清单（对齐 13 方法与体验层矩阵）
├── README.md              # 本说明文档
├── harness_client.py      # 面向 stdio JSONL 协议的轻量级 Python 测试客户端
├── run-parity.sh          # 一键运行全部 parity 检查项的主驱动脚本
└── checks/                # 具体检查项目录（命名为 NN_*.py）
    ├── 01_enum_apps.py          # 枚举类：应用枚举与 accessible_apps 数据结构断言
    ├── 02_screenshot_contract.py # 截图类：PNG 魔数验证、字节与宽高一致性断言
    └── 03_input_dispatch.py     # 输入类：click/scroll 参数回显与 serde 模式拒绝断言
```

---

## 3. 快速上手

### 3.1 一键运行全部 Parity 检查
```bash
./parity/x11/run-parity.sh
```
此命令会自动拉起 `scripts/x11-headless/session.sh` 虚拟 X11 会话，依次执行 `parity/x11/checks/` 下所有脚本并汇总结果。

### 3.2 独立运行单项检查
通过 `session.sh` 可以将任意命令注入到 Xvfb 环境中执行：
```bash
./scripts/x11-headless/session.sh python3 parity/x11/checks/01_enum_apps.py
./scripts/x11-headless/session.sh python3 parity/x11/checks/02_screenshot_contract.py
./scripts/x11-headless/session.sh python3 parity/x11/checks/03_input_dispatch.py
```

### 3.3 运行全功能协议冒烟并输出 JSON 摘要
```bash
./scripts/x11-headless/run-smoke.sh
```
运行结果会保存为机器可读的 JSON 报告：`parity/x11/artifacts/x11-smoke-summary.json`。

---

## 4. 如何添加新的 Parity 检查项

1. 在 `parity/x11/checks/` 下创建新的脚本，命名建议按序号排列，如 `04_window_listing.py`。
2. 引入 `harness_client`：
   ```python
   import os, sys
   sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
   from harness_client import HarnessClient

   def main():
       with HarnessClient() as client:
           resp = client.call_tool("list_windows", {})
           assert resp.get("ok") is True
           # 开展针对性字段断言...
       return 0

   if __name__ == "__main__":
       sys.exit(main())
   ```
3. 赋予执行权限：`chmod +x parity/x11/checks/04_window_listing.py`。
4. 在 `parity/x11/CHECKLIST.md` 中登记对应门禁并更新勾选状态。
5. 运行 `./parity/x11/run-parity.sh` 即可自动将其包含进回归套件。

---

## 5. 约束与已知环境状态

- **无 WM 降级说明**：当前系统环境未安装 `openbox`，且普通用户无免密 `sudo` 权限，遵照安全纪律不硬试提权安装。运行器会自动检测并打印提示，降级到无 WM 模式运行。
  - 枚举回退：无 WM 时 `_NET_SUPPORTING_WM_CHECK` 与 `_NET_CLIENT_LIST` 为空，窗口枚举回退至 `query_tree` 遍历；坐标换算因无 `_NET_FRAME_EXTENTS` 回退至 `translate_coordinates` + `get_geometry`。
  - 若宿主未来安装了 openbox，运行器将自动无感启用。
- **Xvfb 绘图时序防坑纪律（Core 实测 S0 沉淀）**：
  - 在 Xvfb 下若窗口「先画图再 map_window」，会被 map 时的背景重绘刷掉内容；
  - 正确规范：**map 后必须等待 Expose 事件到达再执行绘制**，避免测试用例出现假失败。
- **扩展协议就绪度（Core 实测 S0 确认）**：
  - `XFixes` 6.0：`hide_cursor` 在 Xvfb 对任意窗口调用均被正确接受；
  - `MIT-SHM` 1.2：`shared_pixmaps=true`，支持全屏共享内存直采；
  - `XTest` 2.2：键盘与指针事件实测均可直接精准送达；
  - `XComposite`：遮挡截图正确路径为 `redirect_window` + `NameWindowPixmap`（pixmap 参数必须是 `generate_id()` 未分配的新 XID，不可预先 `create_pixmap`）。
- **截图场景断言规范**：
  - 后续用例针对窗口截图须显式区分「未被遮挡」与「被遮挡」两类场景；被遮挡场景断言其像素输出严格为目标窗口自身内容，不含遮挡物像素。
- **孤儿进程清理**：运行器与客户端均挂载了严密的进程生命周期管理机制，退出或中断时通过 `SIGTERM` + `SIGKILL` 双重保障清理 Xvfb 与 helper 进程，避免进程泄露。
