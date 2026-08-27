"""Launch `vllm serve` with host/port defaults. Extra argv is appended."""

from __future__ import annotations

import logging
import os
import sys
from pathlib import Path

from huggingface_hub import snapshot_download
from huggingface_hub.errors import HfHubHTTPError
from vllm.entrypoints.cli.main import main as vllm_main

HOST = "127.0.0.1"
# Sized so embedder + reranker both fit one 16 GiB card (with a little
# desktop leftover). vLLM default 0.92 / 32k context does not.
# User argv after `bazel run --` overrides.
GPU_MEMORY_UTILIZATION = "0.35"
MAX_MODEL_LEN = "8192"
MAX_NUM_SEQS = "16"
_WORKSPACE_MARKERS = ("MODULE.bazel", "Cargo.toml")
_DEFAULT_HF_HOME = Path(".cache") / "huggingface"
_LOG = logging.getLogger("python.vllm")


def serve(model: str, port: int, extra: tuple[str, ...] = ()) -> None:
    """Replace `sys.argv` and dispatch to the vLLM CLI."""
    logging.basicConfig(level=logging.INFO, format="%(message)s", stream=sys.stderr)
    os.environ.setdefault("PYTHONUNBUFFERED", "1")
    _set_hf_home()
    _prefetch_model(model)
    sys.argv = [
        "vllm",
        "serve",
        model,
        "--host",
        HOST,
        "--port",
        str(port),
        "--gpu-memory-utilization",
        GPU_MEMORY_UTILIZATION,
        "--max-model-len",
        MAX_MODEL_LEN,
        "--max-num-seqs",
        MAX_NUM_SEQS,
        *extra,
        *sys.argv[1:],
    ]
    vllm_main()


def _prefetch_model(model: str) -> None:
    _LOG.info("prefetch %s", model)
    try:
        path = snapshot_download(
            repo_id=model,
            token=os.environ.get("HF_TOKEN") or None,
        )
    except (OSError, RuntimeError, ValueError, HfHubHTTPError) as err:
        _LOG.error("failed to prefetch %s: %s", model, err)
        raise SystemExit(1) from err
    _LOG.info("prefetch ready %s", path)


def _set_hf_home() -> None:
    existing = os.environ.get("HF_HOME")
    if existing:
        path = Path(existing).expanduser().resolve()
        path.mkdir(parents=True, exist_ok=True)
        os.environ["HF_HOME"] = str(path)
        _LOG.info("HF_HOME %s (from env)", path)
        return
    root = _workspace()
    if root is None:
        _LOG.warning("workspace not found; Hub cache stays at the Hugging Face default")
        return
    path = (root / _DEFAULT_HF_HOME).resolve()
    path.mkdir(parents=True, exist_ok=True)
    os.environ["HF_HOME"] = str(path)
    _LOG.info("HF_HOME %s", path)


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
