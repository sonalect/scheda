"""Platform filters for BUILD files."""

# bazel_utils lint/audit test rules use a `.bash` executable. Windows
# CreateProcess cannot run those scripts (error 193). Linux and macOS CI
# still execute these targets.
NOT_WINDOWS = select({
    "@platforms//os:windows": ["@platforms//:incompatible"],
    "//conditions:default": [],
})

# uv.lock is Linux-only (vLLM manylinux). python/cli and python/vllm are
# also `--deleted_packages` on macOS/Windows in `.bazelrc` so `//...` does
# not load their BUILD files (`target_compatible_with` still does).
LINUX = ["@platforms//os:linux"]
