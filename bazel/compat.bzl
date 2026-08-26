"""Platform filters for BUILD files."""

# bazel_utils lint/audit test rules use a `.bash` executable. Windows
# CreateProcess cannot run those scripts (error 193). `py_*` from
# aspect_rules_py also fail analysis: the uv host repo does
# `fail("Unsupported platform windows")`. Linux and macOS CI still
# execute these targets.
NOT_WINDOWS = select({
    "@platforms//os:windows": ["@platforms//:incompatible"],
    "//conditions:default": [],
})
