"""OpenAI `/v1/embeddings` sidecar (`jinaai/jina-embeddings-v5-text-small`).

CC BY-NC 4.0 weights. Not the Apache default (`//python/vllm:embedder`).
"""

from python.vllm.serve import serve

MODEL = "jinaai/jina-embeddings-v5-text-small"
PORT = 8080
HF_OVERRIDES = '{"jina_task": "retrieval"}'


def main() -> None:
    """Serve the Jina embedding model on 127.0.0.1:8080."""
    serve(
        MODEL,
        PORT,
        extra=(
            "--runner",
            "pooling",
            "--trust-remote-code",
            "--hf-overrides",
            HF_OVERRIDES,
        ),
    )


if __name__ == "__main__":
    main()
