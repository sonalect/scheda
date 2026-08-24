"""Platform filters for BUILD files."""

# bazel_utils lint/audit test rules use a `.bash` executable. Windows
# CreateProcess cannot run those scripts (error 193). Linux and macOS CI
# still execute them.
NOT_WINDOWS = select({
    "@platforms//os:windows": ["@platforms//:incompatible"],
    "//conditions:default": [],
})
