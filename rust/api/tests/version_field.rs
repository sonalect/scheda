//! Every proto field named `version` must be field number 1.
//! An `int32 version` is the format generation of the Vault marker and
//! must carry protovalidate `const` equal to the generation, 2
//! (`api/scheda/v1/marker.proto`). A `string version` (`IndexState`) has
//! no such rule.
//! This test fails if someone writes `int32 version = 2`.
//!
//! Do not match `schema_version` or the word version in comments.

use std::fs;
use std::path::{Path, PathBuf};

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn strip_line_comments(src: &str) -> String {
    src.lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

struct VersionField {
    ty: String,
    number: u32,
    options: String,
}

fn scan_version_fields(src: &str) -> Vec<VersionField> {
    let src = strip_line_comments(src);
    let mut fields = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = src[search_from..].find("version") {
        let start = search_from + rel;
        let end = start + "version".len();
        let before_ok = start == 0
            || src[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !is_ident_char(c));
        let after_ok = src[end..].chars().next().is_none_or(|c| !is_ident_char(c));
        search_from = end;
        if !before_ok || !after_ok {
            continue;
        }
        let after = src[end..].trim_start();
        if !after.starts_with('=') {
            continue;
        }
        let after_eq = after[1..].trim_start();
        let digits: String = after_eq
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let Ok(number) = digits.parse::<u32>() else {
            continue;
        };
        let before = src[..start].trim_end();
        let ty: String = before
            .chars()
            .rev()
            .take_while(|&c| is_ident_char(c) || c == '.')
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if ty.is_empty() {
            continue;
        }
        let options = field_options(&after_eq[digits.len()..]);
        fields.push(VersionField {
            ty,
            number,
            options,
        });
    }
    fields
}

fn field_options(src: &str) -> String {
    let mut depth_paren = 0i32;
    let mut depth_brace = 0i32;
    let mut depth_bracket = 0i32;
    for (i, c) in src.char_indices() {
        match c {
            '(' => depth_paren += 1,
            ')' => depth_paren -= 1,
            '{' => depth_brace += 1,
            '}' => depth_brace -= 1,
            '[' => depth_bracket += 1,
            ']' => depth_bracket -= 1,
            ';' if depth_paren == 0 && depth_brace == 0 && depth_bracket == 0 => {
                return src[..i].to_string();
            }
            _ => {}
        }
    }
    src.to_string()
}

/// The format generation the `int32 version` of `VaultMarker` must be.
const GENERATION: u32 = 2;

fn has_int32_const_generation(options: &str) -> bool {
    let compact: String = options.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains(&format!("const:{GENERATION}}}"))
}

fn collect_protos(api_root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    push_protos_in_dir(&api_root.join("scheda/v1"), &mut files);
    files
}

fn push_protos_in_dir(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "proto") {
            files.push(path);
        }
    }
}

fn marker_proto_in(dir: &Path) -> Option<PathBuf> {
    let path = dir.join("scheda/v1/marker.proto");
    path.is_file().then_some(path)
}

fn api_roots() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        candidates.push(PathBuf::from(manifest).join("../../api"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.clone());
        candidates.push(cwd.join("api"));
    }
    if let Ok(runfiles) = std::env::var("RUNFILES_DIR") {
        let runfiles = PathBuf::from(runfiles);
        candidates.push(runfiles.join("_main/api"));
        candidates.push(runfiles.join("scheda/api"));
        if let Ok(ws) = std::env::var("TEST_WORKSPACE")
            && !ws.is_empty()
        {
            candidates.push(runfiles.join(ws).join("api"));
        }
    }
    if let Ok(srcdir) = std::env::var("TEST_SRCDIR") {
        let srcdir = PathBuf::from(srcdir);
        candidates.push(srcdir.join("_main/api"));
        candidates.push(srcdir.join("scheda/api"));
        if let Ok(ws) = std::env::var("TEST_WORKSPACE")
            && !ws.is_empty()
        {
            candidates.push(srcdir.join(ws).join("api"));
        }
    }
    for value in std::env::vars().map(|(_, v)| v) {
        if value.ends_with("scheda/v1/marker.proto") {
            let path = PathBuf::from(value);
            if let Some(root) = path.ancestors().nth(3) {
                candidates.push(root.to_path_buf());
            }
        }
    }

    let mut roots = Vec::new();
    for candidate in candidates {
        if marker_proto_in(&candidate).is_some() {
            roots.push(candidate);
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

fn all_proto_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in api_roots() {
        files.extend(collect_protos(&root));
    }
    files.sort();
    files.dedup();
    files
}

#[test]
fn every_version_field_is_number_one_with_const() {
    let files = all_proto_files();
    assert!(
        !files.is_empty(),
        "no api/**/*.proto found (CARGO_MANIFEST_DIR={:?}, RUNFILES_DIR={:?}, TEST_SRCDIR={:?}, cwd={:?})",
        std::env::var_os("CARGO_MANIFEST_DIR"),
        std::env::var_os("RUNFILES_DIR"),
        std::env::var_os("TEST_SRCDIR"),
        std::env::current_dir().ok(),
    );

    let mut saw_version_field = false;
    let mut errors = Vec::new();
    for path in &files {
        let text = fs::read_to_string(path).unwrap_or_else(|err| {
            panic!("read {}: {err}", path.display());
        });
        for field in scan_version_fields(&text) {
            saw_version_field = true;
            if field.number != 1 {
                errors.push(format!(
                    "{}: field `version` must be number 1, found {} (type {})",
                    path.display(),
                    field.number,
                    field.ty
                ));
            }
            if field.ty == "int32" && !has_int32_const_generation(&field.options) {
                errors.push(format!(
                    "{}: `int32 version` must carry protovalidate const: {GENERATION}",
                    path.display()
                ));
            }
        }
    }
    assert!(
        saw_version_field,
        "no `version` field declarations in {:?}",
        files
    );
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
