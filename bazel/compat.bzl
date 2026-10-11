"""Platform filters for BUILD files."""

# uv.lock is Linux-only (vLLM manylinux). python/cli and python/vllm are
# also `--deleted_packages` on macOS/Windows in `.bazelrc` so `//...` does
# not load their BUILD files (`target_compatible_with` still does).
LINUX = ["@platforms//os:linux"]
