"""vLLM HTTP sidecars for `--embedder-openai-url` / `--reranker-cohere-url`.

Apache defaults: `//python/vllm:embedder` and `:reranker` (Qwen3).
CC BY-NC opt-in: `//python/vllm:embedder_jina` and `:reranker_jina`.
"""

from python.vllm.hf_home import apply as _apply_hf_home

# huggingface_hub / vLLM freeze HF_HOME at import. Pin it first.
_apply_hf_home()
