"""Launch `vllm serve` with host/port defaults. Extra argv is appended."""

from __future__ import annotations

import sys

from vllm.entrypoints.cli.main import main as vllm_main

HOST = "127.0.0.1"
# Sized so embedder + reranker both fit one 16 GiB card (with a little
# desktop leftover). vLLM default 0.92 / 32k context does not.
# User argv after `bazel run --` overrides.
GPU_MEMORY_UTILIZATION = "0.35"
MAX_MODEL_LEN = "8192"
MAX_NUM_SEQS = "16"


def serve(model: str, port: int, extra: tuple[str, ...] = ()) -> None:
    """Replace `sys.argv` and dispatch to the vLLM CLI."""
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
