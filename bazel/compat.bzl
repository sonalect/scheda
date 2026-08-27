"""Platform filters for BUILD files."""

# bazel_utils lint/audit test rules use a `.bash` executable. Windows
# CreateProcess cannot run those scripts (error 193). Linux and macOS CI
# still execute these targets. Python packages are omitted on Windows in
# `.bazelrc` (`--deleted_packages`): loading them fetches aspect_rules_py's
# uv host, which fail()s.
NOT_WINDOWS = select({
    "@platforms//os:windows": ["@platforms//:incompatible"],
    "//conditions:default": [],
})
