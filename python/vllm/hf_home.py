"""Pin ``HF_HOME`` before ``huggingface_hub`` or vLLM freeze cache paths."""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import logging

_WORKSPACE_MARKERS = ("MODULE.bazel", "Cargo.toml")
_DEFAULT_HF_HOME = Path(".cache") / "huggingface"


@dataclass
class _Pin:
    path: Path | None = None
    from_env: bool = False
    applied: bool = False


_PIN = _Pin()


def apply() -> Path | None:
    """Set ``HF_HOME`` from the environment or ``<workspace>/.cache/huggingface``.

    Hugging Face Hub and vLLM read ``HF_HOME`` / ``HF_HUB_CACHE`` at import
    time. Call this before those imports (the ``python.vllm`` package does).
    """
    if _PIN.applied:
        return _PIN.path
    _PIN.applied = True
    existing = os.environ.get("HF_HOME")
    if existing:
        path = Path(existing).expanduser().resolve()
        path.mkdir(parents=True, exist_ok=True)
        os.environ["HF_HOME"] = str(path)
        _PIN.from_env = True
        _PIN.path = path
        return path
    root = _workspace()
    if root is None:
        return None
    path = (root / _DEFAULT_HF_HOME).resolve()
    path.mkdir(parents=True, exist_ok=True)
    os.environ["HF_HOME"] = str(path)
    _PIN.path = path
    return path


def log(logger: logging.Logger) -> None:
    """Write the resolved ``HF_HOME`` (or the Hub-default warning) to ``logger``."""
    if _PIN.path is None:
        logger.warning(
            "workspace not found; Hub cache stays at the Hugging Face default"
        )
        return
    if _PIN.from_env:
        logger.info("HF_HOME %s (from env)", _PIN.path)
        return
    logger.info("HF_HOME %s", _PIN.path)


def _workspace() -> Path | None:
    env = os.environ.get("BUILD_WORKSPACE_DIRECTORY")
    if env:
        root = Path(env).resolve()
        if _is_workspace(root):
            return root
    here = Path(__file__).resolve().parent
    for start in (Path.cwd().resolve(), here):
        found = _walk_workspace(start)
        if found is not None:
            return found
    return None


def _walk_workspace(start: Path) -> Path | None:
    for path in (start, *start.parents):
        if _is_workspace(path):
            return path
    return None


def _is_workspace(path: Path) -> bool:
    return all((path / name).is_file() for name in _WORKSPACE_MARKERS)
