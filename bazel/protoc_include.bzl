"""The well-known `.proto` files of module `protobuf` as one include directory."""

def _import_path(f):
    """The path `protoc` imports `f` by: from its `google/` on."""
    at = f.short_path.find("google/")
    if at < 0:
        fail("%s is not under a google/ directory" % f.short_path)
    return f.short_path[at:]

def _protoc_include_impl(ctx):
    out = ctx.actions.declare_directory(ctx.label.name)
    srcs = ctx.files.srcs
    pairs = []
    for f in srcs:
        pairs.extend([f.path, _import_path(f)])
    ctx.actions.run_shell(
        inputs = srcs,
        outputs = [out],
        command = """
set -eu
out="$1"
shift
while [ "$#" -gt 0 ]; do
  mkdir -p "$out/$(dirname "$2")"
  cp "$1" "$out/$2"
  shift 2
done
""",
        arguments = [out.path] + pairs,
        mnemonic = "ProtocInclude",
    )
    return [DefaultInfo(files = depset([out]))]

protoc_include = rule(
    implementation = _protoc_include_impl,
    attrs = {
        "srcs": attr.label_list(
            allow_files = [".proto"],
            doc = "Well-known type protos, e.g. `@protobuf//:well_known_type_protos`.",
        ),
    },
    doc = """A directory `google/protobuf/*.proto` for `protoc -I` / `PROTOC_INCLUDE`.

The `protoc` of module `protobuf` has no include directory next to it, as an
installed `protoc` has; prost-build reads this one from `PROTOC_INCLUDE`.""",
)
