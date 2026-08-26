"""Drive the Scheda CLI against `python/cli/testdata` fixtures."""

from __future__ import annotations

import argparse
import json
import logging
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

_LOG = logging.getLogger("python.cli")

_FRONT_MATTER = "---"
_NOT_FOUND = "CODE_NOT_FOUND"
_SCHEMA_NAME = ".schema.yaml"
_TOPICS_NAME = "topics.yaml"
_DEFAULT_VAULT = ".vault-test"
_DEFAULT_FIXTURE = Path("python") / "cli" / "testdata" / "animals"
_WORKSPACE_MARKERS = ("MODULE.bazel", "Cargo.toml")
_DEFAULT_EMBEDDER_URL = "http://127.0.0.1:8080/v1/"
_DEFAULT_EMBEDDER_MODEL = "Qwen/Qwen3-Embedding-0.6B"
_DEFAULT_EMBEDDER_DIMENSIONS = 1024
_DEFAULT_RERANKER_URL = "http://127.0.0.1:8081/v1/"
_DEFAULT_RERANKER_MODEL = "Qwen/Qwen3-Reranker-0.6B"
_ALL_FILTER_JSON = '{"all":{}}'
_DEFAULT_FIND_COUNT = 50
_DEFAULT_SEARCH_K = 10


class ToolError(Exception):
    """Driver failed before or after a Scheda CLI call."""


@dataclass(frozen=True, slots=True)
class Document:
    """One Markdown Record in the fixture."""

    collection: str
    record_id: str
    fields: dict[str, Any]
    body: str


@dataclass(frozen=True, slots=True)
class Topic:
    """One QRY-3 paraphrase query from `topics.yaml`."""

    topic_id: str
    text: str
    k: int
    expect_ids: tuple[str, ...]
    expect_collections: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class SearchCall:
    """Vault or collection `search` scope plus whether to rerank."""

    collection: str | None
    collections: tuple[str, ...]
    rerank: bool


@dataclass(frozen=True, slots=True)
class CliResult:
    """One `scheda` process."""

    args: tuple[str, ...]
    returncode: int
    stdout: str
    stderr: str


@dataclass(frozen=True, slots=True)
class Paths:
    """Resolved workspace, Vault, binary, and fixture."""

    workspace: Path
    vault: Path
    scheda: Path
    fixture: Path


@dataclass(frozen=True, slots=True)
class HttpModels:
    """Default Qwen HTTP sidecars (`14`). Empty argv when `--lexical-only`."""

    embedder_url: str
    embedder_model: str
    embedder_dimensions: int
    reranker_url: str
    reranker_model: str
    lexical_only: bool

    def embedder_argv(self) -> tuple[str, ...]:
        """Global `--embedder-*` flags, or none."""
        if self.lexical_only:
            return ()
        return (
            "--embedder-openai-url",
            self.embedder_url,
            "--embedder-model",
            self.embedder_model,
            "--embedder-dimensions",
            str(self.embedder_dimensions),
        )

    def reranker_argv(self) -> tuple[str, ...]:
        """`--reranker-*` flags for `search` only, or none."""
        if self.lexical_only:
            return ()
        return (
            "--reranker-cohere-url",
            self.reranker_url,
            "--reranker-model",
            self.reranker_model,
        )


def main(argv: list[str] | None = None) -> None:
    """Parse argv and run a subcommand."""
    logging.basicConfig(level=logging.INFO, format="%(message)s", stream=sys.stderr)
    parser = _parser()
    args = parser.parse_args(argv)
    try:
        args.func(args)
    except ToolError as err:
        _LOG.error("%s", err)
        raise SystemExit(1) from err


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Talk to the scheda binary to load CLI testdata. "
            "HTTP embedder/reranker default to the local Qwen sidecars (`14`)."
        ),
        suggest_on_error=True,
    )
    shared = argparse.ArgumentParser(add_help=False)
    shared.add_argument(
        "--workspace",
        type=Path,
        help=(
            "Scheda clone root (default: BUILD_WORKSPACE_DIRECTORY, "
            "else walk for MODULE.bazel)"
        ),
    )
    shared.add_argument(
        "--vault",
        type=Path,
        help=f"Vault root (default: <workspace>/{_DEFAULT_VAULT})",
    )
    shared.add_argument(
        "--scheda",
        type=Path,
        help=(
            "scheda binary (default: SCHEDA, runfiles, PATH, then cargo/bazel outputs)"
        ),
    )
    shared.add_argument(
        "--fixture",
        type=Path,
        help=f"Testdata root (default: <workspace>/{_DEFAULT_FIXTURE})",
    )
    shared.add_argument(
        "--embedder-openai-url",
        default=_DEFAULT_EMBEDDER_URL,
        metavar="URL",
        help=f"OpenAI embeddings root (default: {_DEFAULT_EMBEDDER_URL})",
    )
    shared.add_argument(
        "--embedder-model",
        default=_DEFAULT_EMBEDDER_MODEL,
        metavar="NAME",
        help=f"Embeddings model (default: {_DEFAULT_EMBEDDER_MODEL})",
    )
    shared.add_argument(
        "--embedder-dimensions",
        type=int,
        default=_DEFAULT_EMBEDDER_DIMENSIONS,
        metavar="N",
        help=f"Embedding length (default: {_DEFAULT_EMBEDDER_DIMENSIONS})",
    )
    shared.add_argument(
        "--reranker-cohere-url",
        default=_DEFAULT_RERANKER_URL,
        metavar="URL",
        help=f"Cohere rerank root (default: {_DEFAULT_RERANKER_URL})",
    )
    shared.add_argument(
        "--reranker-model",
        default=_DEFAULT_RERANKER_MODEL,
        metavar="NAME",
        help=f"Rerank model (default: {_DEFAULT_RERANKER_MODEL})",
    )
    shared.add_argument(
        "--lexical-only",
        action="store_true",
        help="Do not pass HTTP embedder/reranker flags (BM25 / RRF only)",
    )
    sub = parser.add_subparsers(dest="command", required=True)
    import_cmd = sub.add_parser(
        "import",
        parents=[shared],
        help=(
            f"Create <workspace>/{_DEFAULT_VAULT} if needed and add missing "
            "fixture documents"
        ),
    )
    import_cmd.set_defaults(func=_cmd_import)
    reindex_cmd = sub.add_parser(
        "reindex",
        parents=[shared],
        help="Vault reindex (`14`); JSON ReindexResult on stdout",
    )
    reindex_cmd.set_defaults(func=_cmd_reindex)
    scope = argparse.ArgumentParser(add_help=False)
    scope.add_argument(
        "--collection",
        metavar="NAME",
        help="collection NAME find|search (no --collections)",
    )
    scope.add_argument(
        "--collections",
        metavar="NAME,...",
        help="comma-separated Vault find|search names",
    )
    find_cmd = sub.add_parser(
        "find",
        parents=[shared, scope],
        help="Vault or collection find (`14`); JSON FindResult on stdout",
    )
    find_cmd.add_argument(
        "--filter",
        default=_ALL_FILTER_JSON,
        metavar="JSON",
        help=f"Protobuf JSON Filter (default: {_ALL_FILTER_JSON})",
    )
    find_cmd.add_argument("--offset", type=int, default=0, metavar="N")
    find_cmd.add_argument(
        "--count",
        type=int,
        default=_DEFAULT_FIND_COUNT,
        metavar="N",
        help=f"Page size, must be >= 1 (default: {_DEFAULT_FIND_COUNT})",
    )
    find_cmd.set_defaults(func=_cmd_find)
    search_cmd = sub.add_parser(
        "search",
        parents=[shared, scope],
        help=(
            "Vault or collection search (`14`); JSON SearchHits on stdout. "
            "With no --text/--query/--topic, run every fixture topic"
        ),
    )
    search_cmd.add_argument("--text", metavar="TEXT", help="SearchQuery.text")
    search_cmd.add_argument(
        "--k",
        type=int,
        metavar="N",
        help=f"SearchQuery.k (default: {_DEFAULT_SEARCH_K}, or the topic k)",
    )
    search_cmd.add_argument(
        "--query",
        metavar="JSON",
        help="Protobuf JSON SearchQuery (overrides --text/--k/--filter)",
    )
    search_cmd.add_argument(
        "--topic",
        metavar="ID",
        help=f"Load text/k from fixture {_TOPICS_NAME}",
    )
    search_cmd.add_argument(
        "--filter",
        metavar="JSON",
        help="Optional Filter JSON on SearchQuery (not with --query)",
    )
    search_cmd.add_argument(
        "--no-reranker",
        action="store_true",
        help="Do not pass --reranker-* (RRF order)",
    )
    search_cmd.set_defaults(func=_cmd_search)
    return parser


def _cmd_import(args: argparse.Namespace) -> None:
    paths, _models, cli = _session(args)
    _ensure_vault(cli, paths.vault)
    collections = _fixture_collections(paths.fixture)
    existing = _collection_names(cli.info())
    for name, schema_path, _docs in collections:
        if name not in existing:
            _LOG.info("create-collection %s", name)
            cli.create_collection(name, load_schema_json(schema_path))
            existing.add(name)
    added = 0
    skipped = 0
    for name, _schema_path, docs in collections:
        for doc in docs:
            if cli.get(name, doc.record_id) is not None:
                _LOG.info("skip %s/%s (exists)", name, doc.record_id)
                skipped += 1
                continue
            _LOG.info("add %s/%s", name, doc.record_id)
            cli.add(name, doc)
            added += 1
    if not _models.lexical_only and skipped:
        _LOG.info("reindex (vectors for existing records)")
        cli.reindex()
    _LOG.info(
        "import done: added %d, skipped %d, vault %s", added, skipped, paths.vault
    )


def _cmd_reindex(args: argparse.Namespace) -> None:
    paths, _models, cli = _session(args)
    _require_existing_vault(paths.vault)
    _write_json(cli.reindex())


def _cmd_find(args: argparse.Namespace) -> None:
    paths, _models, cli = _session(args)
    _require_existing_vault(paths.vault)
    collection, collections = _scope(args)
    if args.offset < 0:
        msg = "--offset must be >= 0"
        raise ToolError(msg)
    if args.count < 1:
        msg = "--count must be >= 1"
        raise ToolError(msg)
    page = cli.find(
        collection=collection,
        collections=collections,
        filter_json=args.filter,
        offset=args.offset,
        count=args.count,
    )
    _write_json(page)


def _cmd_search(args: argparse.Namespace) -> None:
    paths, _models, cli = _session(args)
    _require_existing_vault(paths.vault)
    collection, collections = _scope(args)
    call = SearchCall(
        collection=collection,
        collections=tuple(collections),
        rerank=not args.no_reranker,
    )
    n_sources = sum(value is not None for value in (args.text, args.query, args.topic))
    if n_sources > 1:
        msg = "use one of --text, --query, or --topic"
        raise ToolError(msg)
    if args.query is not None and args.filter is not None:
        msg = "--filter cannot be combined with --query"
        raise ToolError(msg)
    if args.query is not None:
        _emit_search(cli, call, args.query)
        return
    if args.text is not None:
        _emit_search(
            cli,
            call,
            _search_query_json(
                args.text, _search_k(args.k, _DEFAULT_SEARCH_K), args.filter
            ),
        )
        return
    topics = load_topics(paths.fixture / _TOPICS_NAME)
    if args.topic is not None:
        topic = _topic_named(topics, args.topic)
        hits = _emit_search(
            cli,
            call,
            _search_query_json(topic.text, _search_k(args.k, topic.k), args.filter),
        )
        _require_topic_ok(topic, hits)
        return
    _search_topics(cli, call, topics, args.k, args.filter)


def _emit_search(cli: SchedaCli, call: SearchCall, query_json: str) -> dict[str, Any]:
    hits = cli.search(
        collection=call.collection,
        collections=list(call.collections),
        query_json=query_json,
        rerank=call.rerank,
    )
    _write_json(hits)
    return hits


def _search_topics(
    cli: SchedaCli,
    call: SearchCall,
    topics: list[Topic],
    k_override: int | None,
    filter_json: str | None,
) -> None:
    reports: list[dict[str, Any]] = []
    failed = False
    for topic in topics:
        query_json = _search_query_json(
            topic.text, _search_k(k_override, topic.k), filter_json
        )
        hits = cli.search(
            collection=call.collection,
            collections=list(call.collections),
            query_json=query_json,
            rerank=call.rerank,
        )
        report = _topic_report(topic, hits)
        reports.append(report)
        if report["ok"]:
            _LOG.info("topic %s ok", topic.topic_id)
            continue
        failed = True
        _LOG.error(
            "topic %s missing %s wrong_collection %s",
            topic.topic_id,
            report["missing"],
            report["wrong_collection"],
        )
    _write_json({"topics": reports})
    if failed:
        msg = "one or more topics missed expected ids"
        raise ToolError(msg)


def _session(args: argparse.Namespace) -> tuple[Paths, HttpModels, SchedaCli]:
    paths = resolve_paths(args.workspace, args.vault, args.scheda, args.fixture)
    models = _http_models(args)
    _LOG.info("scheda %s", paths.scheda)
    _log_models(models)
    return paths, models, SchedaCli(paths.scheda, paths.vault, models)


def _scope(args: argparse.Namespace) -> tuple[str | None, list[str]]:
    collection = args.collection or None
    collections = _split_csv(args.collections)
    if collection is not None and collections:
        msg = "use --collection or --collections, not both"
        raise ToolError(msg)
    return collection, collections


def _split_csv(raw: str | None) -> list[str]:
    if not raw:
        return []
    return [part.strip() for part in raw.split(",") if part.strip()]


def _search_k(override: int | None, default: int) -> int:
    k = default if override is None else override
    if k < 1:
        msg = "--k must be >= 1"
        raise ToolError(msg)
    return k


def _search_query_json(text: str, k: int, filter_json: str | None) -> str:
    if not text:
        msg = "search text must be non-empty"
        raise ToolError(msg)
    body: dict[str, Any] = {"text": text, "k": k}
    if filter_json:
        body["filter"] = _parse_filter_object(filter_json)
    return json.dumps(body, ensure_ascii=False)


def _parse_filter_object(filter_json: str) -> dict[str, Any]:
    try:
        data = json.loads(filter_json)
    except json.JSONDecodeError as err:
        msg = f"--filter is not JSON: {err}"
        raise ToolError(msg) from err
    if not isinstance(data, dict):
        msg = "--filter must be a JSON object"
        raise ToolError(msg)
    return data


def _write_json(data: Any) -> None:
    sys.stdout.write(json.dumps(data, ensure_ascii=False) + "\n")


def _http_models(args: argparse.Namespace) -> HttpModels:
    dimensions = args.embedder_dimensions
    if dimensions < 1:
        msg = "--embedder-dimensions must be >= 1"
        raise ToolError(msg)
    return HttpModels(
        embedder_url=args.embedder_openai_url,
        embedder_model=args.embedder_model,
        embedder_dimensions=dimensions,
        reranker_url=args.reranker_cohere_url,
        reranker_model=args.reranker_model,
        lexical_only=args.lexical_only,
    )


def _log_models(models: HttpModels) -> None:
    if models.lexical_only:
        _LOG.info("HTTP models off (--lexical-only)")
        return
    _LOG.info(
        "embedder %s %s dim=%d",
        models.embedder_model,
        models.embedder_url,
        models.embedder_dimensions,
    )
    _LOG.info("reranker %s %s (search)", models.reranker_model, models.reranker_url)


def resolve_paths(
    workspace: Path | None,
    vault: Path | None,
    scheda: Path | None,
    fixture: Path | None,
) -> Paths:
    """Resolve workspace, Vault, binary, and fixture from flags and defaults."""
    root = _workspace(workspace)
    vault_path = (vault if vault is not None else root / _DEFAULT_VAULT).resolve()
    fixture_path = (
        fixture if fixture is not None else root / _DEFAULT_FIXTURE
    ).resolve()
    if not fixture_path.is_dir():
        msg = f"fixture directory is missing: {fixture_path}"
        raise ToolError(msg)
    binary = _scheda_binary(scheda, root)
    return Paths(workspace=root, vault=vault_path, scheda=binary, fixture=fixture_path)


def _workspace(explicit: Path | None) -> Path:
    if explicit is not None:
        root = explicit.expanduser().resolve()
        _require_workspace(root)
        return root
    env = os.environ.get("BUILD_WORKSPACE_DIRECTORY")
    if env:
        root = Path(env).resolve()
        _require_workspace(root)
        return root
    here = Path(__file__).resolve().parent
    for start in (Path.cwd().resolve(), here):
        found = _walk_workspace(start)
        if found is not None:
            return found
    msg = (
        "could not find the Scheda workspace (MODULE.bazel + Cargo.toml); "
        "pass --workspace"
    )
    raise ToolError(msg)


def _walk_workspace(start: Path) -> Path | None:
    for path in (start, *start.parents):
        if all((path / name).is_file() for name in _WORKSPACE_MARKERS):
            return path
    return None


def _require_workspace(root: Path) -> None:
    if all((root / name).is_file() for name in _WORKSPACE_MARKERS):
        return
    msg = f"not a Scheda workspace: {root}"
    raise ToolError(msg)


def _scheda_binary(explicit: Path | None, workspace: Path) -> Path:
    if explicit is not None:
        return _require_file(explicit.expanduser().resolve(), "scheda binary")
    env = os.environ.get("SCHEDA")
    if env:
        return _require_file(Path(env).expanduser().resolve(), "scheda binary")
    for candidate in _scheda_candidates(workspace):
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate.resolve()
    msg = (
        "scheda binary not found; pass --scheda, set SCHEDA, put scheda on PATH, "
        "or build with `cargo build -p scheda-cli` / `bazel build //rust/cli:scheda`"
    )
    raise ToolError(msg)


def _scheda_candidates(workspace: Path) -> list[Path]:
    names = ("scheda", "scheda.exe")
    found: list[Path] = []
    found.extend(_runfiles_scheda(names))
    for name in names:
        found.append(workspace / "bazel-bin" / "rust" / "cli" / name)
        found.append(workspace / "target" / "debug" / name)
        found.append(workspace / "target" / "release" / name)
        found.extend(workspace.glob(f"target/*/release/{name}"))
        found.extend(workspace.glob(f"target/*/debug/{name}"))
    which = shutil.which("scheda")
    if which:
        found.append(Path(which))
    return found


def _runfiles_scheda(names: tuple[str, ...]) -> list[Path]:
    roots: list[Path] = []
    for key in ("RUNFILES_DIR", "PYTHON_RUNFILES"):
        value = os.environ.get(key)
        if value:
            roots.append(Path(value))
    roots.append(Path(__file__).resolve().parents[2])
    rels = (
        Path("_main") / "rust" / "cli",
        Path("scheda") / "rust" / "cli",
        Path("rust") / "cli",
    )
    found: list[Path] = []
    for root in roots:
        for rel in rels:
            found.extend(root / rel / name for name in names)
    return found


def _require_file(path: Path, label: str) -> Path:
    if path.is_file():
        return path
    msg = f"{label} is not a file: {path}"
    raise ToolError(msg)


def _ensure_vault(cli: SchedaCli, vault: Path) -> None:
    git = vault / ".git"
    if git.exists():
        _LOG.info("vault exists: %s", vault)
        return
    if vault.exists():
        msg = f"path exists but is not a Vault (missing .git): {vault}"
        raise ToolError(msg)
    _LOG.info("open (create) vault %s", vault)
    cli.open()


def _require_existing_vault(vault: Path) -> None:
    if (vault / ".git").exists():
        _LOG.info("vault %s", vault)
        return
    if vault.exists():
        msg = f"path exists but is not a Vault (missing .git): {vault}"
        raise ToolError(msg)
    msg = f"vault missing: {vault}; run import first"
    raise ToolError(msg)


def _fixture_collections(fixture: Path) -> list[tuple[str, Path, list[Document]]]:
    collections: list[tuple[str, Path, list[Document]]] = []
    for child in sorted(p for p in fixture.iterdir() if p.is_dir()):
        schema = child / _SCHEMA_NAME
        if not schema.is_file():
            continue
        docs = [_load_document(child.name, path) for path in sorted(child.glob("*.md"))]
        if not docs:
            msg = f"no Markdown documents in {child}"
            raise ToolError(msg)
        collections.append((child.name, schema, docs))
    if not collections:
        msg = f"no collections with {_SCHEMA_NAME} under {fixture}"
        raise ToolError(msg)
    return collections


def _collection_names(info: dict[str, Any]) -> set[str]:
    collections = info.get("collections")
    if not isinstance(collections, list):
        return set()
    names: set[str] = set()
    for item in collections:
        if isinstance(item, dict) and isinstance(item.get("name"), str):
            names.add(item["name"])
    return names


def _load_document(collection: str, path: Path) -> Document:
    text = path.read_text(encoding="utf-8")
    meta, body = _split_front_matter(text, path)
    record_id = meta.pop("id", "")
    if not record_id:
        msg = f"missing id in front matter: {path}"
        raise ToolError(msg)
    if not meta:
        msg = f"no fields in front matter: {path}"
        raise ToolError(msg)
    return Document(collection=collection, record_id=record_id, fields=meta, body=body)


def _split_front_matter(text: str, path: Path) -> tuple[dict[str, Any], str]:
    if not text.startswith(f"{_FRONT_MATTER}\n"):
        msg = f"expected YAML front matter in {path}"
        raise ToolError(msg)
    rest = text[len(_FRONT_MATTER) + 1 :]
    end = rest.find(f"\n{_FRONT_MATTER}\n")
    if end < 0:
        msg = f"unclosed YAML front matter in {path}"
        raise ToolError(msg)
    header = rest[:end]
    body = rest[end + len(_FRONT_MATTER) + 2 :]
    meta: dict[str, Any] = {}
    for raw in header.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        key, sep, value = line.partition(":")
        if not sep:
            msg = f"invalid front matter line in {path}: {raw}"
            raise ToolError(msg)
        meta[key.strip()] = _yaml_scalar(value.strip())
    return meta, body


def load_schema_json(path: Path) -> str:
    """Load fixture `.schema.yaml` as Protobuf JSON `Schema`."""
    data = load_yaml_subset(path.read_text(encoding="utf-8"), path)
    if not isinstance(data, dict):
        msg = f"schema must be a mapping: {path}"
        raise ToolError(msg)
    return json.dumps(data)


def load_topics(path: Path) -> list[Topic]:
    """Load QRY-3 topics from fixture `topics.yaml`."""
    if not path.is_file():
        msg = f"topics file is missing: {path}"
        raise ToolError(msg)
    data = load_yaml_subset(path.read_text(encoding="utf-8"), path)
    if not isinstance(data, dict):
        msg = f"topics must be a mapping: {path}"
        raise ToolError(msg)
    raw = data.get("topics")
    if not isinstance(raw, list) or not raw:
        msg = f"no topics in {path}"
        raise ToolError(msg)
    return [_topic_from_yaml(item, path) for item in raw]


def _topic_from_yaml(item: Any, path: Path) -> Topic:
    if not isinstance(item, dict):
        msg = f"topic must be a mapping in {path}"
        raise ToolError(msg)
    topic_id = item.get("id")
    text = item.get("text")
    if not isinstance(topic_id, str) or not topic_id:
        msg = f"topic missing id in {path}"
        raise ToolError(msg)
    if not isinstance(text, str) or not text:
        msg = f"topic {topic_id} missing text in {path}"
        raise ToolError(msg)
    k = item.get("k", _DEFAULT_SEARCH_K)
    if not isinstance(k, int) or k < 1:
        msg = f"topic {topic_id} k must be an integer >= 1 in {path}"
        raise ToolError(msg)
    expect_ids = _string_tuple(item.get("expect_ids"), "expect_ids", topic_id, path)
    if not expect_ids:
        msg = f"topic {topic_id} expect_ids is empty in {path}"
        raise ToolError(msg)
    expect_collections = _string_tuple(
        item.get("expect_collections"), "expect_collections", topic_id, path
    )
    return Topic(
        topic_id=topic_id,
        text=text,
        k=k,
        expect_ids=expect_ids,
        expect_collections=expect_collections,
    )


def _string_tuple(value: Any, field: str, topic_id: str, path: Path) -> tuple[str, ...]:
    if value is None:
        return ()
    if not isinstance(value, list):
        msg = f"topic {topic_id} {field} must be a list in {path}"
        raise ToolError(msg)
    items: list[str] = []
    for item in value:
        if not isinstance(item, str) or not item:
            msg = f"topic {topic_id} {field} entries must be strings in {path}"
            raise ToolError(msg)
        items.append(item)
    return tuple(items)


def _topic_named(topics: list[Topic], topic_id: str) -> Topic:
    for topic in topics:
        if topic.topic_id == topic_id:
            return topic
    known = ", ".join(topic.topic_id for topic in topics)
    msg = f"unknown topic {topic_id!r} (have {known})"
    raise ToolError(msg)


def _hits(data: dict[str, Any]) -> list[dict[str, Any]]:
    hits = data.get("hits")
    if not isinstance(hits, list):
        return []
    return [hit for hit in hits if isinstance(hit, dict)]


def _topic_report(topic: Topic, hits: dict[str, Any]) -> dict[str, Any]:
    ranked = _hits(hits)
    got = {hit.get("id") for hit in ranked}
    missing = [record_id for record_id in topic.expect_ids if record_id not in got]
    allowed = set(topic.expect_collections)
    wrong = [
        {"id": hit.get("id"), "collection": hit.get("collection")}
        for hit in ranked
        if hit.get("id") in topic.expect_ids
        and allowed
        and hit.get("collection") not in allowed
    ]
    return {
        "id": topic.topic_id,
        "ok": not missing and not wrong,
        "missing": missing,
        "wrong_collection": wrong,
        "hits": ranked,
    }


def _require_topic_ok(topic: Topic, hits: dict[str, Any]) -> None:
    report = _topic_report(topic, hits)
    if report["ok"]:
        _LOG.info("topic %s ok", topic.topic_id)
        return
    msg = (
        f"topic {topic.topic_id} missing {report['missing']} "
        f"wrong_collection {report['wrong_collection']}"
    )
    raise ToolError(msg)


def load_yaml_subset(text: str, path: Path) -> Any:
    """Load maps, lists, and scalars. No tags, anchors, or multiline blocks."""
    lines = text.splitlines()
    value, index = _parse_yaml_block(lines, 0, 0, path)
    while index < len(lines) and not lines[index].strip():
        index += 1
    if index != len(lines):
        msg = f"unexpected YAML content in {path} at line {index + 1}"
        raise ToolError(msg)
    return value


def _parse_yaml_block(
    lines: list[str],
    index: int,
    min_indent: int,
    path: Path,
) -> tuple[Any, int]:
    index = _skip_blank(lines, index)
    if index >= len(lines):
        return {}, index
    indent, content = _yaml_line(lines[index])
    if indent < min_indent:
        msg = f"invalid indent in {path} at line {index + 1}"
        raise ToolError(msg)
    if content.startswith("- "):
        return _parse_yaml_list(lines, index, indent, path)
    return _parse_yaml_map(lines, index, indent, path)


def _parse_yaml_map(
    lines: list[str],
    index: int,
    indent: int,
    path: Path,
) -> tuple[dict[str, Any], int]:
    mapping: dict[str, Any] = {}
    while index < len(lines):
        if not lines[index].strip() or lines[index].lstrip().startswith("#"):
            index += 1
            continue
        line_indent, content = _yaml_line(lines[index])
        if line_indent < indent:
            break
        if line_indent > indent:
            msg = f"invalid indent in {path} at line {index + 1}"
            raise ToolError(msg)
        if content.startswith("- "):
            msg = f"unexpected list item in {path} at line {index + 1}"
            raise ToolError(msg)
        key, value, index = _parse_yaml_key(lines, index, indent, path)
        mapping[key] = value
    return mapping, index


def _parse_yaml_list(
    lines: list[str],
    index: int,
    indent: int,
    path: Path,
) -> tuple[list[Any], int]:
    items: list[Any] = []
    while index < len(lines):
        if not lines[index].strip() or lines[index].lstrip().startswith("#"):
            index += 1
            continue
        line_indent, content = _yaml_line(lines[index])
        if line_indent < indent:
            break
        if line_indent != indent or not content.startswith("- "):
            break
        item_text = content[2:]
        if item_text and ":" in item_text and not item_text.startswith("{"):
            nested = [" " * (indent + 2) + item_text, *lines[index + 1 :]]
            value, consumed = _parse_yaml_map(nested, 0, indent + 2, path)
            index = index + consumed
            items.append(value)
            continue
        if item_text:
            items.append(_yaml_scalar(item_text))
            index += 1
            continue
        value, index = _parse_yaml_block(lines, index + 1, indent + 1, path)
        items.append(value)
    return items, index


def _parse_yaml_key(
    lines: list[str],
    index: int,
    indent: int,
    path: Path,
) -> tuple[str, Any, int]:
    _line_indent, content = _yaml_line(lines[index])
    key, sep, remainder = content.partition(":")
    if not sep or not key.strip():
        msg = f"expected key: value in {path} at line {index + 1}"
        raise ToolError(msg)
    key = key.strip()
    remainder = remainder.strip()
    if remainder:
        return key, _yaml_scalar(remainder), index + 1
    nxt = _skip_blank(lines, index + 1)
    if nxt >= len(lines):
        return key, {}, nxt
    next_indent, _next_content = _yaml_line(lines[nxt])
    if next_indent <= indent:
        return key, {}, index + 1
    value, nxt = _parse_yaml_block(lines, nxt, next_indent, path)
    return key, value, nxt


def _skip_blank(lines: list[str], index: int) -> int:
    while index < len(lines) and (
        not lines[index].strip() or lines[index].lstrip().startswith("#")
    ):
        index += 1
    return index


def _yaml_line(line: str) -> tuple[int, str]:
    stripped = line.lstrip(" ")
    return len(line) - len(stripped), stripped


def _yaml_scalar(text: str) -> Any:
    special: dict[str, Any] = {
        "{}": {},
        "[]": [],
        "true": True,
        "True": True,
        "false": False,
        "False": False,
        "null": None,
        "Null": None,
        "~": None,
    }
    if text in special:
        return special[text]
    if text.startswith("[") and text.endswith("]"):
        inner = text[1:-1].strip()
        if not inner:
            return []
        return [_yaml_scalar(part.strip()) for part in inner.split(",") if part.strip()]
    if text.isdigit() or (text.startswith("-") and text[1:].isdigit()):
        return int(text)
    if (text.startswith('"') and text.endswith('"')) or (
        text.startswith("'") and text.endswith("'")
    ):
        return text[1:-1]
    return text


class SchedaCli:
    """One `scheda --vault PATH` subprocess per call."""

    def __init__(self, binary: Path, vault: Path, models: HttpModels) -> None:
        """Bind one binary to one Vault path and optional HTTP models."""
        self._binary = binary
        self._vault = vault
        self._models = models

    def open(self) -> None:
        """`open`: create the Vault if the path is missing."""
        self._run("open", expect_stdout=False)

    def info(self) -> dict[str, Any]:
        """`info`: `api.v1.VaultInfo` JSON."""
        return self._run_json("info")

    def reindex(self) -> dict[str, Any]:
        """`reindex`: rebuild lexical / vector / edges from files."""
        return self._run_json("reindex")

    def create_collection(self, name: str, schema_json: str) -> None:
        """`create-collection NAME --schema JSON`."""
        self._run(
            "create-collection", name, "--schema", schema_json, expect_stdout=True
        )

    def get(self, collection: str, record_id: str) -> dict[str, Any] | None:
        """`collection NAME get --id ID`. `None` when `CODE_NOT_FOUND`."""
        result = self._invoke("collection", collection, "get", "--id", record_id)
        if result.returncode == 0:
            return _parse_json(result.stdout, result.args, "stdout")
        error = _parse_error(result)
        if error.get("code") == _NOT_FOUND:
            return None
        msg = _error_message(result, error)
        raise ToolError(msg)

    def add(self, collection: str, document: Document) -> None:
        """`collection NAME add --id ID --record JSON`."""
        record = json.dumps({"fields": document.fields, "body": document.body})
        self._run(
            "collection",
            collection,
            "add",
            "--id",
            document.record_id,
            "--record",
            record,
            expect_stdout=False,
        )

    def find(
        self,
        *,
        collection: str | None,
        collections: list[str],
        filter_json: str,
        offset: int,
        count: int,
    ) -> dict[str, Any]:
        """Vault or collection `find`. Protobuf JSON `FindResult`."""
        paging = (
            "--filter",
            filter_json,
            "--offset",
            str(offset),
            "--count",
            str(count),
        )
        if collection is not None:
            return self._run_json("collection", collection, "find", *paging)
        if collections:
            return self._run_json(
                "find", "--collections", ",".join(collections), *paging
            )
        return self._run_json("find", *paging)

    def search(
        self,
        *,
        collection: str | None,
        collections: list[str],
        query_json: str,
        rerank: bool,
    ) -> dict[str, Any]:
        """Vault or collection `search`. Protobuf JSON `SearchHits`."""
        query = ("--query", query_json)
        extra = self._models.reranker_argv() if rerank else ()
        if collection is not None:
            return self._run_json("collection", collection, "search", *query, *extra)
        if collections:
            return self._run_json(
                "search",
                "--collections",
                ",".join(collections),
                *query,
                *extra,
            )
        return self._run_json("search", *query, *extra)

    def _run_json(self, *args: str) -> dict[str, Any]:
        result = self._run(*args, expect_stdout=True)
        data = _parse_json(result.stdout, result.args, "stdout")
        if not isinstance(data, dict):
            msg = f"scheda {' '.join(result.args)} stdout is not an object"
            raise ToolError(msg)
        return data

    def _run(self, *args: str, expect_stdout: bool) -> CliResult:
        result = self._invoke(*args)
        if result.returncode != 0:
            msg = _error_message(result, _parse_error(result))
            raise ToolError(msg)
        if expect_stdout and not result.stdout.strip():
            msg = f"scheda {' '.join(result.args)} produced empty stdout"
            raise ToolError(msg)
        return result

    def _invoke(self, *args: str) -> CliResult:
        cmd = [
            str(self._binary),
            "--vault",
            str(self._vault),
            *self._models.embedder_argv(),
            *args,
        ]
        try:
            completed = subprocess.run(  # noqa: S603
                cmd,
                check=False,
                capture_output=True,
                text=True,
            )
        except OSError as err:
            msg = f"failed to run {cmd[0]}: {err}"
            raise ToolError(msg) from err
        return CliResult(
            args=tuple(cmd[1:]),
            returncode=completed.returncode,
            stdout=completed.stdout,
            stderr=completed.stderr,
        )


def _parse_json(text: str, args: tuple[str, ...], stream: str) -> Any:
    try:
        return json.loads(text)
    except json.JSONDecodeError as err:
        msg = f"scheda {' '.join(args)} {stream} is not JSON: {err}: {text!r}"
        raise ToolError(msg) from err


def _parse_error(result: CliResult) -> dict[str, Any]:
    text = result.stderr.strip() or result.stdout.strip()
    if not text:
        return {}
    try:
        data = json.loads(text)
    except json.JSONDecodeError:
        return {}
    return data if isinstance(data, dict) else {}


def _error_message(result: CliResult, error: dict[str, Any]) -> str:
    code = error.get("code")
    message = error.get("message")
    if isinstance(code, str) and isinstance(message, str):
        return f"scheda {' '.join(result.args)}: {code}: {message}"
    detail = (
        result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
    )
    return f"scheda {' '.join(result.args)} failed: {detail}"


if __name__ == "__main__":
    main()
