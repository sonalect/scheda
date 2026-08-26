"""OpenAI `/v1/embeddings` sidecar (`Qwen/Qwen3-Embedding-0.6B`)."""

from python.vllm.serve import serve

MODEL = "Qwen/Qwen3-Embedding-0.6B"
PORT = 8080


def main() -> None:
    """Serve the embedding model on 127.0.0.1:8080."""
    serve(MODEL, PORT, extra=("--runner", "pooling", "--convert", "embed"))


if __name__ == "__main__":
    main()
