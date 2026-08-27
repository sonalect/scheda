"""Cohere `/v1/rerank` sidecar (`jinaai/jina-reranker-v3.5`).

CC BY-NC 4.0 weights. Not the Apache default (`//python/vllm:reranker`).
"""

from python.vllm.serve import serve

MODEL = "jinaai/jina-reranker-v3.5"
PORT = 8081


def main() -> None:
    """Serve the Jina reranker on 127.0.0.1:8081."""
    serve(
        MODEL,
        PORT,
        extra=(
            "--runner",
            "pooling",
            "--trust-remote-code",
        ),
    )


if __name__ == "__main__":
    main()
