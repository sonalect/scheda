"""Cohere `/v1/rerank` sidecar (`Qwen/Qwen3-Reranker-0.6B`)."""

from pathlib import Path

from python.vllm.serve import serve

MODEL = "Qwen/Qwen3-Reranker-0.6B"
PORT = 8081
HF_OVERRIDES = (
    '{"architectures": ["Qwen3ForSequenceClassification"],'
    '"classifier_from_token": ["no", "yes"],'
    '"is_original_qwen3_reranker": true}'
)
_TEMPLATE = Path(__file__).resolve().parent / "qwen3_reranker.jinja"


def main() -> None:
    """Serve the reranker on 127.0.0.1:8081."""
    serve(
        MODEL,
        PORT,
        extra=(
            "--runner",
            "pooling",
            "--hf-overrides",
            HF_OVERRIDES,
            "--chat-template",
            str(_TEMPLATE),
        ),
    )


if __name__ == "__main__":
    main()
