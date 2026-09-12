"""Launch `vllm serve` with host/port defaults. Extra argv is appended."""

from __future__ import annotations

import logging
import os
import platform
import sys

from huggingface_hub import snapshot_download
from huggingface_hub.errors import HfHubHTTPError

from python.vllm import hf_home

HOST = "127.0.0.1"
# Sized so embedder + reranker both fit one 16 GiB card (with a little
# desktop leftover). vLLM default 0.92 / 32k context does not.
# User argv after `bazel run --` overrides.
GPU_MEMORY_UTILIZATION = "0.35"
MAX_MODEL_LEN = "8192"
MAX_NUM_SEQS = "16"
_LOG = logging.getLogger("python.vllm")
_WSL_PIN_MEMORY_ENV = "VLLM_WSL2_ENABLE_PIN_MEMORY"


def serve(model: str, port: int, extra: tuple[str, ...] = ()) -> None:
    """Replace `sys.argv` and dispatch to the vLLM CLI.

    Loads the prefetched snapshot path so EngineCore does not fetch the
    Hub repo a second time. The API still advertises ``model``.

    On WSL, vLLM 0.29's default V2 GPU runner needs UVA (pinned host
    memory), and vLLM itself turns pin_memory off there, so EngineCore
    dies at init with ``UVA is not available``. Unless the operator
    already set ``VLLM_WSL2_ENABLE_PIN_MEMORY``, this turns pin_memory
    on so the V2 runner can start. Native Linux is unchanged. Embedder,
    reranker, and the Jina sidecars all go through this function.
    """
    logging.basicConfig(level=logging.INFO, format="%(message)s", stream=sys.stderr)
    os.environ.setdefault("PYTHONUNBUFFERED", "1")
    hf_home.log(_LOG)
    _enable_wsl_pin_memory()
    # Import after pin_memory: vLLM caches UVA availability at first use.
    from vllm.entrypoints.cli.main import main as vllm_main  # noqa: PLC0415

    snapshot = _prefetch_model(model)
    sys.argv = [
        "vllm",
        "serve",
        snapshot,
        "--served-model-name",
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


def _enable_wsl_pin_memory() -> None:
    if _WSL_PIN_MEMORY_ENV in os.environ:
        return
    if "microsoft" not in platform.release().casefold():
        return
    os.environ[_WSL_PIN_MEMORY_ENV] = "1"
    _LOG.info(
        "WSL: V2 model runner needs UVA; enabling pin_memory (%s=1)",
        _WSL_PIN_MEMORY_ENV,
    )


def _prefetch_model(model: str) -> str:
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
    return path
