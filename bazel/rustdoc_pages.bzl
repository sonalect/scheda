"""Stamp rust_doc HTML so GitHub Pages can serve it."""

def _rustdoc_pages_impl(ctx):
    out = ctx.actions.declare_directory(ctx.label.name)
    srcs = ctx.files.doc
    if len(srcs) != 1:
        fail("expected one rustdoc output, got %s" % [src.path for src in srcs])
    src = srcs[0]
    index = ctx.file.index_html

    ctx.actions.run_shell(
        mnemonic = "RustdocPages",
        progress_message = "Stamping rustdoc for GitHub Pages %{label}",
        inputs = [src, index],
        outputs = [out],
        command = """
set -euo pipefail
mkdir -p "{out}"
cp -R -L "{src}/." "{out}/"
chmod -R u+w "{out}"
rm -f "{out}/.lock"
cp "{index}" "{out}/index.html"
touch "{out}/.nojekyll"
""".format(
            out = out.path,
            src = src.path,
            index = index.path,
        ),
    )
    return [DefaultInfo(files = depset([out]))]

rustdoc_pages = rule(
    doc = "Copy rust_doc HTML, add a crate redirect and .nojekyll for GitHub Pages.",
    implementation = _rustdoc_pages_impl,
    attrs = {
        "doc": attr.label(
            doc = "A rust_doc target.",
            mandatory = True,
        ),
        "index_html": attr.label(
            allow_single_file = [".html"],
            default = Label("//bazel:rustdoc_pages_index.html"),
        ),
    },
)
