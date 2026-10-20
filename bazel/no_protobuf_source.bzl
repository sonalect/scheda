"""Fails the build of a target that compiles protobuf from source.

`.bazelrc` applies `no_protobuf_source` to every target that `bazel build`,
`bazel test`, or `bazel run` names and requests its output group, so
`bazel test //...` checks the whole build graph as it is configured: every
attribute, exec tools and the targets behind a configuration transition
included. Bazel has no other way to check `//...` from inside the build:
`genquery` refuses a recursive target pattern, and a test cannot run
`bazel aquery`.

The check is an action that fails, not a `fail()` at analysis: `bazel cquery`
reads the build options, and an analysis error would break the very query
that shows the path to the offending target. Bazel builds the `_validation`
output group of a top-level aspect only, not of the aspect on a dependency,
so each target hands the failing actions below it up to the top through
`_NoProtobufSourceInfo`.

protoc comes only as the prebuilt binary of the proto toolchain
(`//bazel:protoc_prefix`). `@protobuf//:protoc` is a `cc_binary`: depending
on it compiles protoc and the protobuf C++ library with the host toolchain.
"""

_PROTOC = Label("@protobuf//:protoc")

_NoProtobufSourceInfo = provider(
    doc = "The failing checks of the protobuf targets a target reaches.",
    fields = {"checks": "depset of File: one per protobuf target that compiles C++."},
)

def _deps(attr):
    """Every Target in the attributes of a rule."""
    deps = []
    for name in dir(attr):
        value = getattr(attr, name)
        kind = type(value)
        if kind == "Target":
            deps.append(value)
        elif kind == "list":
            deps.extend([v for v in value if type(v) == "Target"])
        elif kind == "dict":
            deps.extend([k for k in value.keys() if type(k) == "Target"])
            deps.extend([v for v in value.values() if type(v) == "Target"])
    return deps

def _check(target, ctx):
    """A failing action for `target` if it compiles protobuf, else None."""
    label = target.label
    if label.repo_name != _PROTOC.repo_name:
        return None
    if label != _PROTOC and not [a for a in target.actions if a.mnemonic == "CppCompile"]:
        return None
    out = ctx.actions.declare_file(label.name + ".no_protobuf_source")
    ctx.actions.run_shell(
        outputs = [out],
        command = 'echo "$1" >&2; exit 1',
        arguments = [(
            "{} ({}) compiles protobuf from source. Use the prebuilt protoc of " +
            "the proto toolchain (//bazel:protoc_prefix), never " +
            "@protobuf//:protoc. The path: bazel cquery 'somepath(//..., {})'"
        ).format(label, ctx.rule.kind, label)],
        mnemonic = "NoProtobufSource",
        progress_message = "Checking %{label} for a protobuf source build",
    )
    return out

def _no_protobuf_source_impl(target, ctx):
    check = _check(target, ctx)
    if check:
        # The targets below it are protobuf's own: one report is enough.
        checks = depset([check])
    else:
        checks = depset(transitive = [
            dep[_NoProtobufSourceInfo].checks
            for dep in _deps(ctx.rule.attr)
            if _NoProtobufSourceInfo in dep
        ])
    if not checks:
        return []
    return [
        _NoProtobufSourceInfo(checks = checks),
        OutputGroupInfo(no_protobuf_source = checks),
    ]

no_protobuf_source = aspect(
    implementation = _no_protobuf_source_impl,
    attr_aspects = ["*"],
    doc = "Fails on `@protobuf//:protoc` and on any C++ compile of module `protobuf`.",
)
