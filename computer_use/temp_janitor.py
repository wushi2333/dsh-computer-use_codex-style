"""插件自产临时文件的按归属清理（SEC-05）。

移植自源项目 ``git-repo/dsh-computer-use/internal/capture.mjs`` 的设计精华
（见 ``docs/design-essence-dsh-computer-use.md`` §2 SEC-05）：

* **只删自己产出的。** 源项目有一条硬规则：调用方用 ``save_path`` 显式指定落盘位置的
  那些文件「插件既不知道、也不该删 —— 那是调用方的文件」。这里同一条规则换成另一种
  形式：janitor **只知道自己创建（或被显式登记）的路径**，别的路径一律不碰。
* **归属是事实，不是猜测。** 源项目让 ``admit``（真正把图交给模型的那一步）来认领文件，
  而不是在生成文件名时猜。这里让**创建文件的调用点**同时给出归属（owner），因为浏览器
  路径在创建那一刻就已经知道是哪个标签页的产物。
* **删除是 best effort。** 文件可能已经不在，或被别的进程占着；删除失败不该让收尾本身失败。

为什么截图本身不需要这个模块
------------------------------

本项目**从不把截图写到自己的磁盘**：截图字节以 base64 形式交给宿主的
``attachments.saveImages()``（``src/tool.js`` 的 ``admitImages``），文件生命周期归宿主。
所以 SEC-05 在截图上已经是「**委托即满足**」——插件不拥有那些文件，也就不会漏掉它们。

真正会漏的是**浏览器路径**：导出文本、下载的媒体、资源包 —— 它们写进系统临时目录后
**从不被清理**，于是用户的导出内容会一直躺在 ``%TEMP%`` 里。这个模块把那些产物收进
一个插件自己拥有的目录，并按归属记账，于是「关掉一个标签页」或「侧车退出」就能收干净。
"""

from __future__ import annotations

import os
import re
import shutil
import tempfile
import threading
from pathlib import Path
from typing import Any
from uuid import uuid4

#: 插件自己拥有的临时根目录。所有插件产出的临时文件都在它下面。
DEFAULT_ROOT_NAME = "dsh-computer-use"

#: 归属键里允许的字符。**点号不在其中**：``..`` 是路径穿越的原料，
#: 而归属键会直接变成一个目录名，所以它必须是文件名里最老实的那种。
_OWNER_SAFE = re.compile(r"[^A-Za-z0-9_-]+")

#: 产物文件名里允许的字符。这里**保留点号**：扩展名是调用方判断文件类型的依据。
_NAME_SAFE = re.compile(r"[^A-Za-z0-9._-]+")


def sanitize_owner(owner: str) -> str:
    """把归属键变成一个安全的目录名片段。

    空归属是合法的：它表示「还不知道归谁」，那些文件在实例收尾时被一起清掉。
    """
    cleaned = _OWNER_SAFE.sub("-", str(owner or "")).strip("-")
    return cleaned[:64]


def _env_root() -> Path:
    """插件自己拥有的临时根目录。

    允许用环境变量把它挪走（测试与多实例隔离）。
    """
    override = os.environ.get("DSH_COMPUTER_USE_TEMP_ROOT")
    if override:
        return Path(override)
    return Path(tempfile.gettempdir()) / DEFAULT_ROOT_NAME


class TempJanitor:
    """按归属记账的临时文件清理器。

    这个类**不依赖任何宿主设施**（没有 ctypes、没有 helper），
    所以它可以在纯 Python 下直接单测。
    """

    def __init__(self, root: Path | str | None = None) -> None:
        # An explicit root wins. Without one the root is resolved **per call**
        # from the environment, so `DSH_COMPUTER_USE_TEMP_ROOT` takes effect
        # without restarting the sidecar (and tests can move it at runtime).
        self._explicit_root = Path(root) if root is not None else None
        self._lock = threading.RLock()
        # owner -> 它产出的路径集合。
        self._by_owner: dict[str, set[str]] = {}
        # 还没有归属的路径（实例收尾时一起清）。
        self._unclaimed: set[str] = set()
        self._seq = 0

    @property
    def root(self) -> Path:
        """插件自己拥有的临时根目录。"""
        return self._explicit_root if self._explicit_root is not None else _env_root()

    # ------------------------------------------------------------------ 记账

    def track(self, path: Path | str, owner: str = "") -> Path:
        """登记一个插件产出的路径。

        登记是**幂等**的：同一个路径登记两次不会重复记账。
        """
        target = Path(path)
        key = str(owner)
        with self._lock:
            if key:
                self._by_owner.setdefault(key, set()).add(str(target))
            else:
                self._unclaimed.add(str(target))
        return target

    def new_path(self, name: str, owner: str = "") -> Path:
        """在插件自己的根目录下造一个唯一路径，并登记它。

        名字保留可读部分与**扩展名**（``export-tab-3-7.txt``），再带一个
        随机短后缀。序号只保证**单个实例**内的唯一性——两个挂着不同
        ``DSH_COMPUTER_USE_TEMP_ROOT`` 的进程各有一个 ``_seq``，都从 1 数起；
        而扩展名是调用方判断文件类型的依据，所以 ``.txt`` / ``.md`` / ``.bin``
        不能被清洗掉。
        """
        with self._lock:
            self._seq += 1
            seq = self._seq
        # 先只取文件名部分：名字里的任何目录成分都不该影响结果。
        safe = _NAME_SAFE.sub("-", os.path.basename(str(name or ""))).strip("-")
        stem, dot, suffix = safe.rpartition(".")
        if not dot or not stem:
            stem, suffix = (safe, "") if not dot else (stem, suffix)
        if not stem or stem in {".", ".."}:
            stem = "artifact"
        directory = self.root / sanitize_owner(owner) if owner else self.root
        directory.mkdir(parents=True, exist_ok=True)
        filename = f"{stem}-{seq}-{uuid4().hex[:8]}{('.' + suffix) if suffix else ''}"
        return self.track(directory / filename, owner)

    def new_dir(self, prefix: str = "work", owner: str = "") -> Path:
        """在插件自己的根目录下造一个唯一目录，并登记它。"""
        with self._lock:
            self._seq += 1
            seq = self._seq
        directory = self.root / sanitize_owner(owner) if owner else self.root
        directory.mkdir(parents=True, exist_ok=True)
        base = _OWNER_SAFE.sub("-", str(prefix or "work")).strip("-") or "work"
        target = directory / f"{base}-{seq}-{uuid4().hex[:8]}"
        target.mkdir(parents=True, exist_ok=True)
        return self.track(target, owner)

    # ------------------------------------------------------------------ 清理

    def _remove(self, paths: list[str]) -> int:
        """删掉一批路径，返回实际处理过的数量。

        删除是 best effort：文件可能已经不在，或被别的进程占着；
        失败不该让收尾本身失败。
        """
        handled = 0
        for raw in paths:
            try:
                target = Path(raw)
            except (TypeError, ValueError):
                continue
            try:
                if target.is_dir():
                    shutil.rmtree(target, ignore_errors=True)
                else:
                    target.unlink()
                handled += 1
            except (OSError, ValueError):
                continue
        return handled

    def release_owner(self, owner: str) -> int:
        """删掉某个归属产出的全部文件（关掉一个标签页时调用）。

        只删这个归属的；别人的与「还没有归属的」都不动。
        """
        key = str(owner)
        with self._lock:
            paths = sorted(self._by_owner.pop(key, set()))
            self._unclaimed -= set(paths)
        if not paths:
            return 0
        handled = self._remove(paths)
        self._prune_empty_dirs(paths)
        return handled

    def release_all(self) -> int:
        """实例收尾：把登记过的全部删掉。

        「还没有归属的」也一起清——那是「造好了但没人接手」的残渣
        （进程被杀、下载失败、标签页先一步结束都会留下它）。

        **只删登记过的路径。** 根目录里可能有 janitor 从没见过的文件
        （例如调用方自己放进去的），那些不是我们的东西——把整个根目录
        删掉会连它们一起带走，那正是源项目给「调用方指定路径」豁免的
        那条规则所禁止的。空目录会被顺手收掉。
        """
        with self._lock:
            paths = sorted(self._unclaimed)
            for owned in self._by_owner.values():
                paths.extend(sorted(owned))
            self._by_owner.clear()
            self._unclaimed.clear()
        handled = self._remove(paths)
        self._prune_empty_dirs(paths)
        return handled

    def _prune_empty_dirs(self, paths: list[str]) -> None:
        """收掉因为删除而变空的目录（从下往上）。

        只收**空的**目录：里面还有东西就说明那不是我们建的，留在原地。
        """
        root = self.root
        for raw in paths:
            try:
                target = Path(raw)
            except (TypeError, ValueError):
                continue
            directory = target if target.is_dir() else target.parent
            while True:
                try:
                    if root not in directory.parents and directory != root:
                        break
                    if not directory.is_dir():
                        break
                    next(directory.iterdir())
                    break
                except StopIteration:
                    try:
                        directory.rmdir()
                    except OSError:
                        break
                except OSError:
                    break
                directory = directory.parent
                if directory == root.parent:
                    break

    def pending_count(self) -> int:
        """现在还登记着多少个路径（给诊断用）。"""
        with self._lock:
            return len(self._unclaimed) + sum(len(paths) for paths in self._by_owner.values())

    def owners(self) -> list[str]:
        """列出所有有记账的归属。"""
        with self._lock:
            return sorted(self._by_owner)

    def report(self) -> dict[str, Any]:
        """一份可审计的快照。"""
        with self._lock:
            return {
                "root": str(self.root),
                "pending": len(self._unclaimed) + sum(len(paths) for paths in self._by_owner.values()),
                "owners": sorted(self._by_owner),
                "unclaimed": len(self._unclaimed),
            }


#: 进程级 janitor。浏览器路径统一用它造临时文件。
janitor = TempJanitor()


def shutdown_janitor() -> int:
    """侧车退出时调用：清掉这个进程造出来的全部临时文件。"""
    return janitor.release_all()
