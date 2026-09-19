"""Drive the Scheda CLI against `python/cli/testdata` fixtures."""

from __future__ import annotations

import argparse
import hashlib
import json
import logging
import math
import os
import shutil
import subprocess
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass
from enum import StrEnum
from pathlib import Path
from typing import Any

from huggingface_hub import hf_hub_download
from huggingface_hub.errors import HfHubHTTPError

_LOG = logging.getLogger("python.cli")

_FRONT_MATTER = "---"
_NOT_FOUND = "CODE_NOT_FOUND"
_SCHEMA_NAME = ".collection.yaml"
_TOPICS_NAME = "topics.yaml"
_ENV_BACKEND = "SCHEDA_CLI_BACKEND"
_DEFAULT_VAULT_LLMS = ".vault-test-LLMs"
_DEFAULT_VAULT_JINA = ".vault-test-jina"
_DEFAULT_VAULT_MODEL2VEC = ".vault-test-model2vec"
_MODEL2VEC_REPO = "minishlab/potion-multilingual-128M"
_DEFAULT_MODEL2VEC_DIR = (
    Path(".cache") / "model2vec" / _MODEL2VEC_REPO.rsplit("/", 1)[-1]
)
_MODEL2VEC_REVISION = "main"
_MODEL2VEC_FILES = ("config.json", "tokenizer.json", "model.safetensors")
_DEFAULT_FIXTURE = Path("python") / "cli" / "testdata" / "animals"
_WORKSPACE_MARKERS = ("MODULE.bazel", "Cargo.toml")
_DEFAULT_EMBEDDER_URL = "http://127.0.0.1:8080/v1/"
_DEFAULT_EMBEDDER_MODEL = "Qwen/Qwen3-Embedding-0.6B"
_DEFAULT_EMBEDDER_DIMENSIONS = 1024
_DEFAULT_RERANKER_URL = "http://127.0.0.1:8081/v1/"
_DEFAULT_RERANKER_MODEL = "Qwen/Qwen3-Reranker-0.6B"
_DEFAULT_JINA_EMBEDDER_MODEL = "jinaai/jina-embeddings-v5-text-small"
_DEFAULT_JINA_RERANKER_MODEL = "jinaai/jina-reranker-v3.5"
_ALL_FILTER_JSON = '{"all":{}}'
_DEFAULT_FIND_COUNT = 50
_DEFAULT_SEARCH_K = 10
_DOWNLOAD_TIMEOUT_S = 300
_DOWNLOAD_CHUNK = 1024 * 1024


class Backend(StrEnum):
    """Which embedder the driver passes to `scheda`."""

    LLMS = "llms"
    JINA = "jina"
    MODEL2VEC = "model2vec"


class ToolError(Exception):
    """Driver failed before or after a Scheda CLI call."""


@dataclass(frozen=True, slots=True)
class Document:
    """One Markdown Record in the fixture."""

    collection: str
    record_id: str
    fields: dict[str, Any]
    body: str
    path: Path


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
    include_raw: bool


@dataclass(frozen=True, slots=True)
class CliResult:
    """One `scheda` process."""

    args: tuple[str, ...]
    returncode: int
    stdout: str
    stderr: str


@dataclass(frozen=True, slots=True)
class HubBlob:
    """Hub tree entry for one required Model2Vec file."""

    size: int
    sha256: str | None
    git_oid: str


@dataclass(frozen=True, slots=True)
class Paths:
    """Resolved workspace, Vault, binary, and fixture."""

    workspace: Path
    vault: Path
    scheda: Path
    fixture: Path


@dataclass(frozen=True, slots=True)
class DriverModels:
    """Embedder / reranker flags for `scheda` (`14`). Empty when `--lexical-only`."""

    backend: Backend
    lexical_only: bool
    embedder_url: str
    embedder_model: str
    embedder_dimensions: int
    reranker_url: str
    reranker_model: str
    model2vec_dir: Path | None

    def embedder_argv(self) -> tuple[str, ...]:
        """Global embedder flags, or none."""
        if self.lexical_only:
            return ()
        if self.backend is Backend.MODEL2VEC:
            if self.model2vec_dir is None:
                return ()
            return ("--model2vec-embedder", str(self.model2vec_dir))
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
        if self.lexical_only or self.backend is Backend.MODEL2VEC:
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
            f"{Backend.LLMS}: Qwen HTTP sidecars and {_DEFAULT_VAULT_LLMS}. "
            f"{Backend.JINA}: Jina HTTP sidecars (CC BY-NC) and "
            f"{_DEFAULT_VAULT_JINA}. "
            f"{Backend.MODEL2VEC}: {_DEFAULT_VAULT_MODEL2VEC}; "
            f"Model2Vec ({_DEFAULT_MODEL2VEC_DIR}) on reindex and search."
        ),
        suggest_on_error=True,
    )
    parser.add_argument(
        "--backend",
        choices=tuple(Backend),
        default=os.environ.get(_ENV_BACKEND),
        help=(
            f"{Backend.LLMS}: HTTP embedder/reranker and {_DEFAULT_VAULT_LLMS}. "
            f"{Backend.JINA}: Jina HTTP (CC BY-NC) and {_DEFAULT_VAULT_JINA}. "
            f"{Backend.MODEL2VEC}: {_DEFAULT_VAULT_MODEL2VEC}; "
            f"--model2vec-embedder on reindex and search. "
            f"Default: {_ENV_BACKEND}, or required. "
            "Must precede the subcommand. Bazel targets pass this flag."
        ),
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
        help=(
            "Vault root (default: "
            f"<workspace>/{_DEFAULT_VAULT_LLMS}, "
            f"{_DEFAULT_VAULT_JINA}, or "
            f"{_DEFAULT_VAULT_MODEL2VEC})"
        ),
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
        "--model2vec-embedder",
        type=Path,
        metavar="DIR",
        help=(
            f"Model2Vec directory (default: <workspace>/{_DEFAULT_MODEL2VEC_DIR}; "
            f"downloads {_MODEL2VEC_REPO} when files are missing). "
            f"Only with --backend {Backend.MODEL2VEC}. "
            "Passed to scheda on reindex and search."
        ),
    )
    shared.add_argument(
        "--lexical-only",
        action="store_true",
        help="Do not pass embedder/reranker flags (BM25 / RRF only)",
    )
    sub = parser.add_subparsers(dest="command", required=True)
    import_cmd = sub.add_parser(
        "import",
        parents=[shared],
        help=(
            "Create the backend Vault if needed "
            f"({_DEFAULT_VAULT_LLMS}, {_DEFAULT_VAULT_JINA}, or "
            f"{_DEFAULT_VAULT_MODEL2VEC}) "
            "and add missing fixture documents"
        ),
    )
    import_cmd.set_defaults(func=_cmd_import)
    reindex_cmd = sub.add_parser(
        "reindex",
        parents=[shared],
        help="Vault reindex (`14`); YAML ReindexResult on stdout",
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
        help="Vault or collection find (`14`); YAML records on stdout",
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
    find_cmd.add_argument(
        "--include-raw",
        action="store_true",
        help="FindQuery.include_raw: include Record.raw in YAML",
    )
    find_cmd.set_defaults(func=_cmd_find)
    search_cmd = sub.add_parser(
        "search",
        parents=[shared, scope],
        help=(
            "Vault or collection search (`14`); YAML hits on stdout. "
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
    search_cmd.add_argument(
        "--include-raw",
        action="store_true",
        help="SearchQuery.include_raw: include Hit.raw in YAML",
    )
    search_cmd.set_defaults(func=_cmd_search)
    return parser


def _cmd_import(args: argparse.Namespace) -> None:
    paths, models, cli = _session(args)
    _ensure_vault(cli, paths.vault)
    collections = _fixture_collections(paths.fixture)
    existing = _collection_names(cli.info())
    for name, decl_path, _docs in collections:
        if name not in existing:
            _LOG.info("create-collection %s", name)
            cli.create_collection(name, load_schema_json(decl_path))
            existing.add(name)
    added = 0
    skipped = 0
    pass_embedder = models.backend is not Backend.MODEL2VEC
    for name, _decl_path, docs in collections:
        for doc in docs:
            if cli.get(name, doc.record_id) is not None:
                _LOG.info("skip %s/%s (exists)", name, doc.record_id)
                skipped += 1
                continue
            _LOG.info("add %s/%s", name, doc.record_id)
            cli.add(name, doc, embedder=pass_embedder)
            added += 1
    if pass_embedder and not models.lexical_only and skipped:
        _LOG.info("reindex (vectors for existing records)")
        cli.reindex()
    _LOG.info(
        "import done: added %d, skipped %d, vault %s", added, skipped, paths.vault
    )


def _cmd_reindex(args: argparse.Namespace) -> None:
    paths, _models, cli = _session(args)
    _require_existing_vault(paths.vault)
    _write_yaml(cli.reindex())


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
        paging=(args.offset, args.count),
        include_raw=args.include_raw,
    )
    _write_yaml(page, include_raw=args.include_raw)


def _cmd_search(args: argparse.Namespace) -> None:
    paths, _models, cli = _session(args)
    _require_existing_vault(paths.vault)
    collection, collections = _scope(args)
    call = SearchCall(
        collection=collection,
        collections=tuple(collections),
        rerank=not args.no_reranker,
        include_raw=args.include_raw,
    )
    n_sources = sum(value is not None for value in (args.text, args.query, args.topic))
    if n_sources > 1:
        msg = "use one of --text, --query, or --topic"
        raise ToolError(msg)
    if args.query is not None and args.filter is not None:
        msg = "--filter cannot be combined with --query"
        raise ToolError(msg)
    if args.query is not None:
        _emit_search(
            cli, call, _with_include_raw(args.query, include_raw=call.include_raw)
        )
        return
    if args.text is not None:
        _emit_search(
            cli,
            call,
            _search_query_json(
                args.text,
                _search_k(args.k, _DEFAULT_SEARCH_K),
                args.filter,
                include_raw=call.include_raw,
            ),
        )
        return
    topics = load_topics(paths.fixture / _TOPICS_NAME)
    if args.topic is not None:
        topic = _topic_named(topics, args.topic)
        hits = _emit_search(
            cli,
            call,
            _search_query_json(
                topic.text,
                _search_k(args.k, topic.k),
                args.filter,
                include_raw=call.include_raw,
            ),
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
        include_raw=call.include_raw,
    )
    _write_yaml(hits, include_raw=call.include_raw)
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
            topic.text,
            _search_k(k_override, topic.k),
            filter_json,
            include_raw=call.include_raw,
        )
        hits = cli.search(
            collection=call.collection,
            collections=list(call.collections),
            query_json=query_json,
            rerank=call.rerank,
            include_raw=call.include_raw,
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
    _write_yaml({"topics": reports}, include_raw=call.include_raw)
    if failed:
        msg = "one or more topics missed expected ids"
        raise ToolError(msg)


def _session(args: argparse.Namespace) -> tuple[Paths, DriverModels, SchedaCli]:
    backend = _backend(args)
    paths = resolve_paths(
        args.workspace, args.vault, args.scheda, args.fixture, backend
    )
    models = _driver_models(
        args,
        backend,
        paths.workspace,
        command=args.command,
    )
    _LOG.info("scheda %s", paths.scheda)
    _LOG.info("backend %s vault %s", backend, paths.vault)
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


def _search_query_json(
    text: str, k: int, filter_json: str | None, *, include_raw: bool
) -> str:
    if not text:
        msg = "search text must be non-empty"
        raise ToolError(msg)
    body: dict[str, Any] = {"text": text, "k": k}
    if filter_json:
        body["filter"] = _parse_filter_object(filter_json)
    if include_raw:
        body["includeRaw"] = True
    return json.dumps(body, ensure_ascii=False)


def _with_include_raw(query_json: str, *, include_raw: bool) -> str:
    if not include_raw:
        return query_json
    try:
        data = json.loads(query_json)
    except json.JSONDecodeError as err:
        msg = f"--query is not JSON: {err}"
        raise ToolError(msg) from err
    if not isinstance(data, dict):
        msg = "--query must be a JSON object"
        raise ToolError(msg)
    data["includeRaw"] = True
    return json.dumps(data, ensure_ascii=False)


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


def _backend(args: argparse.Namespace) -> Backend:
    raw = args.backend
    if raw is None:
        msg = (
            "pass --backend llms|jina|model2vec before the subcommand "
            "(or bazel run //python/cli:cli_LLMs / //python/cli:cli_jina / "
            "//python/cli:cli_model2vec)"
        )
        raise ToolError(msg)
    try:
        return Backend(raw)
    except ValueError:
        msg = f"unknown --backend {raw!r} (llms, jina, or model2vec)"
        raise ToolError(msg) from None


_YAML_DOC_KEYS = ("records", "hits", "topics")
_YAML_BOOL_NULL = frozenset(
    {
        "true",
        "false",
        "null",
        "yes",
        "no",
        "on",
        "off",
        "~",
        "y",
        "n",
    }
)
_YAML_INDICATOR = frozenset("-?:{}[]!&*|>%@`'\"#,")
_YAML_SPECIAL = frozenset("\n\r:#{}[]&*!|>'\"%@`\\")


def _write_yaml(data: Any, *, include_raw: bool = False) -> None:
    docs = _yaml_documents(data, include_raw=include_raw)
    rendered = [_dump_yaml(doc).rstrip("\n") for doc in docs]
    sys.stdout.write(f"\n{_FRONT_MATTER}\n".join(rendered) + "\n")


def _yaml_documents(data: Any, *, include_raw: bool) -> list[Any]:
    if not isinstance(data, dict):
        return [data]
    for key in _YAML_DOC_KEYS:
        items = data.get(key)
        if not isinstance(items, list):
            continue
        if not items:
            return [data]
        cleaned = items if include_raw else [_omit_raw(item) for item in items]
        rest = {name: value for name, value in data.items() if name != key}
        if rest:
            return [rest, *cleaned]
        return cleaned
    return [data]


def _omit_raw(item: Any) -> Any:
    if not isinstance(item, dict) or "raw" not in item:
        return item
    return {key: value for key, value in item.items() if key != "raw"}


def _dump_yaml(value: Any) -> str:
    return "\n".join(_yaml_lines(value, 0)) + "\n"


def _yaml_lines(value: Any, indent: int) -> list[str]:
    pad = "  " * indent
    if isinstance(value, dict):
        return _yaml_map_lines(value, indent, pad)
    if isinstance(value, list):
        return _yaml_list_lines(value, indent, pad)
    return [f"{pad}{_yaml_dump_scalar(value)}"]


def _yaml_map_lines(value: dict[str, Any], indent: int, pad: str) -> list[str]:
    if not value:
        return [f"{pad}{{}}"]
    lines: list[str] = []
    for key, item in value.items():
        lines.extend(_yaml_map_item(_yaml_key(key), item, indent, pad))
    return lines


def _yaml_map_item(key: str, item: Any, indent: int, pad: str) -> list[str]:
    if isinstance(item, dict):
        if not item:
            return [f"{pad}{key}: {{}}"]
        return [f"{pad}{key}:", *_yaml_lines(item, indent + 1)]
    if isinstance(item, list):
        if not item:
            return [f"{pad}{key}: []"]
        return [f"{pad}{key}:", *_yaml_lines(item, indent + 1)]
    if isinstance(item, str) and ("\n" in item or "\r" in item):
        return _yaml_block(key, item, indent)
    return [f"{pad}{key}: {_yaml_dump_scalar(item)}"]


def _yaml_list_lines(value: list[Any], indent: int, pad: str) -> list[str]:
    if not value:
        return [f"{pad}[]"]
    lines: list[str] = []
    nested = pad + "  "
    for item in value:
        if isinstance(item, dict):
            inner = _yaml_lines(item, 0)
            if not inner:
                lines.append(f"{pad}- {{}}")
                continue
            lines.append(f"{pad}- {inner[0]}")
            lines.extend(nested + extra for extra in inner[1:])
            continue
        if isinstance(item, list):
            inner = _yaml_lines(item, indent + 1)
            if not inner:
                lines.append(f"{pad}- []")
                continue
            lines.append(f"{pad}-")
            lines.extend(inner)
            continue
        if isinstance(item, str) and ("\n" in item or "\r" in item):
            lines.append(f"{pad}- |-")
            lines.extend(nested + line for line in item.splitlines())
            continue
        lines.append(f"{pad}- {_yaml_dump_scalar(item)}")
    return lines


def _yaml_block(key: str, text: str, indent: int) -> list[str]:
    pad = "  " * indent
    nested = "  " * (indent + 1)
    marker = "|" if text.endswith("\n") else "|-"
    lines = [f"{pad}{key}: {marker}"]
    body = text.removesuffix("\n")
    lines.extend(nested + line for line in body.split("\n"))
    return lines


def _yaml_key(key: object) -> str:
    if isinstance(key, str) and key.isidentifier():
        return key
    return json.dumps(str(key), ensure_ascii=False)


def _yaml_dump_scalar(value: Any) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        return _yaml_float(value)
    if isinstance(value, str):
        if _yaml_plain_ok(value):
            return value
        return json.dumps(value, ensure_ascii=False)
    msg = f"cannot dump YAML for {type(value).__name__}"
    raise ToolError(msg)


def _yaml_float(value: float) -> str:
    if math.isnan(value):
        return ".nan"
    if math.isinf(value):
        return ".inf" if value > 0 else "-.inf"
    return repr(value)


def _yaml_plain_ok(text: str) -> bool:
    if not text or text.strip() != text:
        return False
    if text.lower() in _YAML_BOOL_NULL:
        return False
    if text[:1] in _YAML_INDICATOR:
        return False
    if any(ch in _YAML_SPECIAL for ch in text):
        return False
    try:
        float(text)
    except ValueError:
        return True
    return False


def _driver_models(
    args: argparse.Namespace,
    backend: Backend,
    workspace: Path,
    *,
    command: str,
) -> DriverModels:
    dimensions = args.embedder_dimensions
    if dimensions < 1:
        msg = "--embedder-dimensions must be >= 1"
        raise ToolError(msg)
    if backend is not Backend.MODEL2VEC and args.model2vec_embedder is not None:
        msg = "--model2vec-embedder requires --backend model2vec"
        raise ToolError(msg)
    embedder_model = args.embedder_model
    reranker_model = args.reranker_model
    if backend is Backend.JINA:
        if embedder_model == _DEFAULT_EMBEDDER_MODEL:
            embedder_model = _DEFAULT_JINA_EMBEDDER_MODEL
        if reranker_model == _DEFAULT_RERANKER_MODEL:
            reranker_model = _DEFAULT_JINA_RERANKER_MODEL
    model2vec_dir: Path | None = None
    if (
        backend is Backend.MODEL2VEC
        and not args.lexical_only
        and command in ("reindex", "search")
    ):
        directory = _model2vec_dir(workspace, args.model2vec_embedder)
        if command == "reindex":
            model2vec_dir = _ensure_model2vec_embedder(directory)
        else:
            _require_model2vec_cache(directory)
            model2vec_dir = directory
    return DriverModels(
        backend=backend,
        lexical_only=args.lexical_only,
        embedder_url=args.embedder_openai_url,
        embedder_model=embedder_model,
        embedder_dimensions=dimensions,
        reranker_url=args.reranker_cohere_url,
        reranker_model=reranker_model,
        model2vec_dir=model2vec_dir,
    )


def _log_models(models: DriverModels) -> None:
    if models.lexical_only:
        _LOG.info("embedder off (--lexical-only)")
        return
    if models.backend is Backend.MODEL2VEC:
        if models.model2vec_dir is None:
            _LOG.info("model2vec embedder only on reindex and search")
            return
        _LOG.info("model2vec embedder %s", models.model2vec_dir)
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
    backend: Backend,
) -> Paths:
    """Resolve workspace, Vault, binary, and fixture from flags and defaults."""
    root = _workspace(workspace)
    default_vault = _default_vault(backend)
    vault_path = (vault if vault is not None else root / default_vault).resolve()
    fixture_path = (
        fixture if fixture is not None else root / _DEFAULT_FIXTURE
    ).resolve()
    if not fixture_path.is_dir():
        msg = f"fixture directory is missing: {fixture_path}"
        raise ToolError(msg)
    binary = _scheda_binary(scheda, root)
    return Paths(workspace=root, vault=vault_path, scheda=binary, fixture=fixture_path)


def _default_vault(backend: Backend) -> str:
    if backend is Backend.MODEL2VEC:
        return _DEFAULT_VAULT_MODEL2VEC
    if backend is Backend.JINA:
        return _DEFAULT_VAULT_JINA
    return _DEFAULT_VAULT_LLMS


def _model2vec_dir(workspace: Path, explicit: Path | None) -> Path:
    if explicit is None:
        return (workspace / _DEFAULT_MODEL2VEC_DIR).resolve()
    path = explicit.expanduser()
    if not path.is_absolute():
        path = workspace / path
    return path.resolve()


def _require_model2vec_cache(directory: Path) -> None:
    missing = [name for name in _MODEL2VEC_FILES if not (directory / name).is_file()]
    if not missing:
        return
    msg = (
        f"model2vec cache missing {', '.join(missing)} in {directory}; "
        "run reindex to download"
    )
    raise ToolError(msg)


def _ensure_model2vec_embedder(directory: Path) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    blobs = _hub_blobs()
    missing: list[str] = []
    for name in _MODEL2VEC_FILES:
        path = directory / name
        blob = blobs[name]
        if _blob_matches(path, name, blob):
            continue
        if path.exists():
            _LOG.info("retry %s (size or hash mismatch)", name)
            path.unlink(missing_ok=True)
        missing.append(name)
    if not missing:
        _LOG.info("model2vec cache hit %s", directory)
        return directory
    _LOG.info(
        "download %s (%s) into %s%s",
        _MODEL2VEC_REPO,
        ", ".join(missing),
        directory,
        " (HF_TOKEN)" if _hf_token() else "",
    )
    for name in missing:
        _download_hf_file(name, directory / name, blobs[name])
    return directory


def _hub_blobs() -> dict[str, HubBlob]:
    url = f"https://huggingface.co/api/models/{_MODEL2VEC_REPO}/tree/{_MODEL2VEC_REVISION}"
    payload = _http_json(url)
    if not isinstance(payload, list):
        msg = f"Hub tree is not a list: {url}"
        raise ToolError(msg)
    wanted = set(_MODEL2VEC_FILES)
    blobs: dict[str, HubBlob] = {}
    for item in payload:
        parsed = _hub_blob_from_tree(item, wanted)
        if parsed is None:
            continue
        name, meta = parsed
        blobs[name] = meta
    missing = [name for name in _MODEL2VEC_FILES if name not in blobs]
    if missing:
        msg = f"Hub tree missing {', '.join(missing)}"
        raise ToolError(msg)
    return blobs


def _hub_blob_from_tree(item: object, wanted: set[str]) -> tuple[str, HubBlob] | None:
    if not isinstance(item, dict) or item.get("type") != "file":
        return None
    name = item.get("path")
    if not isinstance(name, str) or name not in wanted:
        return None
    git_oid = item.get("oid")
    if not isinstance(git_oid, str) or not git_oid:
        msg = f"Hub tree missing oid for {name}"
        raise ToolError(msg)
    size = item.get("size")
    sha256: str | None = None
    lfs = item.get("lfs")
    if isinstance(lfs, dict):
        oid = lfs.get("oid")
        if not isinstance(oid, str) or not oid:
            msg = f"Hub tree missing LFS oid for {name}"
            raise ToolError(msg)
        sha256 = oid.removeprefix("sha256:")
        lfs_size = lfs.get("size")
        if isinstance(lfs_size, int) and lfs_size > 0:
            size = lfs_size
    if not isinstance(size, int) or size < 1:
        msg = f"Hub tree missing size for {name}"
        raise ToolError(msg)
    return name, HubBlob(size=size, sha256=sha256, git_oid=git_oid)


def _http_json(url: str) -> Any:
    headers = {**_hf_headers(), "Accept": "application/json"}
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=_DOWNLOAD_TIMEOUT_S) as response:
            raw = response.read()
    except (OSError, urllib.error.URLError) as err:
        msg = f"failed to fetch {url}: {err}"
        raise ToolError(msg) from err
    try:
        return json.loads(raw)
    except json.JSONDecodeError as err:
        msg = f"{url} is not JSON: {err}"
        raise ToolError(msg) from err


def _blob_matches(path: Path, name: str, blob: HubBlob) -> bool:
    try:
        _verify_blob(path, name, blob)
    except ToolError:
        return False
    return True


def _verify_blob(path: Path, name: str, blob: HubBlob) -> None:
    if not path.is_file():
        msg = f"missing {name}"
        raise ToolError(msg)
    size = path.stat().st_size
    if size != blob.size:
        msg = f"{name} size {size} != Hub {blob.size}"
        raise ToolError(msg)
    if blob.sha256 is not None:
        got = _sha256_file(path)
        if got != blob.sha256:
            msg = f"{name} sha256 {got} != Hub {blob.sha256}"
            raise ToolError(msg)
        return
    got = _git_blob_sha1(path.read_bytes())
    if got != blob.git_oid:
        msg = f"{name} git oid {got} != Hub {blob.git_oid}"
        raise ToolError(msg)


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while True:
            chunk = handle.read(_DOWNLOAD_CHUNK)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()


def _git_blob_sha1(data: bytes) -> str:
    digest = hashlib.sha1()  # noqa: S324
    digest.update(f"blob {len(data)}\0".encode())
    digest.update(data)
    return digest.hexdigest()


def _hf_token() -> str | None:
    token = os.environ.get("HF_TOKEN", "").strip()
    return token or None


def _hf_headers() -> dict[str, str]:
    headers = {
        "User-Agent": (
            f"scheda-python-cli; hf_hub/1.28.0; python/{sys.version_info.major}."
            f"{sys.version_info.minor}.{sys.version_info.micro}"
        ),
        "Accept-Encoding": "identity",
    }
    token = _hf_token()
    if token is not None:
        headers["Authorization"] = f"Bearer {token}"
    return headers


def _download_hf_file(name: str, dest: Path, blob: HubBlob) -> None:
    _LOG.info("fetch %s (%d bytes) via huggingface_hub", name, blob.size)
    try:
        saved = Path(
            hf_hub_download(
                repo_id=_MODEL2VEC_REPO,
                filename=name,
                revision=_MODEL2VEC_REVISION,
                local_dir=dest.parent,
                token=_hf_token(),
                user_agent="scheda-python-cli",
            )
        )
    except (OSError, RuntimeError, ValueError, HfHubHTTPError) as err:
        msg = f"failed to download {_MODEL2VEC_REPO}/{name}: {err}"
        raise ToolError(msg) from err
    if saved.resolve() != dest.resolve():
        try:
            dest.unlink(missing_ok=True)
            dest.hardlink_to(saved)
        except OSError:
            shutil.copy2(saved, dest)
    try:
        _verify_blob(dest, name, blob)
    except ToolError:
        dest.unlink(missing_ok=True)
        raise
    _LOG.info("wrote %s (%d bytes, hash ok)", name, blob.size)


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
    record_id = path.name
    fields: dict[str, Any] = {}
    body = text
    if text.startswith(f"{_FRONT_MATTER}\n"):
        meta, body = _split_front_matter(text, path)
        record_id = str(meta.pop("id", "") or path.name)
        fields = meta
    return Document(
        collection=collection, record_id=record_id, fields=fields, body=body, path=path
    )


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
    """Load fixture `.collection.yaml` as Protobuf JSON `Schema`."""
    data = load_yaml_subset(path.read_text(encoding="utf-8"), path)
    if not isinstance(data, dict):
        msg = f"collection file must be a mapping: {path}"
        raise ToolError(msg)
    schema = data.get("schema")
    if not isinstance(schema, dict):
        msg = f"schema object is missing: {path}"
        raise ToolError(msg)
    return json.dumps(schema)


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

    def __init__(self, binary: Path, vault: Path, models: DriverModels) -> None:
        """Bind one binary to one Vault path and optional embedder flags."""
        self._binary = binary
        self._vault = vault
        self._models = models

    def open(self) -> None:
        """`open`: create the Vault if the path is missing."""
        self._run("open", expect_stdout=False, embedder=False)

    def info(self) -> dict[str, Any]:
        """`info`: `api.v1.VaultInfo` JSON."""
        return self._run_json("info", embedder=False)

    def reindex(self) -> dict[str, Any]:
        """`reindex`: rebuild lexical / vector / journal maps from files."""
        return self._run_json("reindex")

    def create_collection(self, name: str, schema_json: str) -> None:
        """`create-collection NAME --schema JSON`."""
        self._run(
            "create-collection",
            name,
            "--schema",
            schema_json,
            expect_stdout=True,
            embedder=False,
        )

    def get(self, collection: str, record_id: str) -> str | None:
        """`collection NAME get --id ID`. Raw UTF-8. `None` if `CODE_NOT_FOUND`."""
        result = self._invoke(
            "collection", collection, "get", "--id", record_id, embedder=False
        )
        if result.returncode == 0:
            return result.stdout
        error = _parse_error(result)
        if error.get("code") == _NOT_FOUND:
            return None
        msg = _error_message(result, error)
        raise ToolError(msg)

    def add(
        self, collection: str, document: Document, *, embedder: bool = True
    ) -> None:
        """`collection NAME add --id ID --file PATH`."""
        self._run(
            "collection",
            collection,
            "add",
            "--id",
            document.record_id,
            "--file",
            str(document.path),
            expect_stdout=False,
            embedder=embedder,
        )

    def find(
        self,
        *,
        collection: str | None,
        collections: list[str],
        filter_json: str,
        paging: tuple[int, int],
        include_raw: bool,
    ) -> dict[str, Any]:
        """Vault or collection `find`. Protobuf JSON `FindResult`."""
        offset, count = paging
        paging_args: list[str] = [
            "--filter",
            filter_json,
            "--offset",
            str(offset),
            "--count",
            str(count),
        ]
        if include_raw:
            paging_args.append("--include-raw")
        if collection is not None:
            return self._run_json(
                "collection", collection, "find", *paging_args, embedder=False
            )
        if collections:
            return self._run_json(
                "find",
                "--collections",
                ",".join(collections),
                *paging_args,
                embedder=False,
            )
        return self._run_json("find", *paging_args, embedder=False)

    def search(
        self,
        *,
        collection: str | None,
        collections: list[str],
        query_json: str,
        rerank: bool,
        include_raw: bool,
    ) -> dict[str, Any]:
        """Vault or collection `search`. Protobuf JSON `SearchHits`."""
        query = ("--query", query_json)
        extra = self._models.reranker_argv() if rerank else ()
        raw = ("--include-raw",) if include_raw else ()
        if collection is not None:
            return self._run_json(
                "collection", collection, "search", *query, *extra, *raw
            )
        if collections:
            return self._run_json(
                "search",
                "--collections",
                ",".join(collections),
                *query,
                *extra,
                *raw,
            )
        return self._run_json("search", *query, *extra, *raw)

    def _run_json(self, *args: str, embedder: bool = True) -> dict[str, Any]:
        result = self._run(*args, expect_stdout=True, embedder=embedder)
        data = _parse_json(result.stdout, result.args, "stdout")
        if not isinstance(data, dict):
            msg = f"scheda {' '.join(result.args)} stdout is not an object"
            raise ToolError(msg)
        return data

    def _run(self, *args: str, expect_stdout: bool, embedder: bool = True) -> CliResult:
        result = self._invoke(*args, embedder=embedder)
        if result.returncode != 0:
            msg = _error_message(result, _parse_error(result))
            raise ToolError(msg)
        if expect_stdout and not result.stdout.strip():
            msg = f"scheda {' '.join(result.args)} produced empty stdout"
            raise ToolError(msg)
        return result

    def _invoke(self, *args: str, embedder: bool = True) -> CliResult:
        flags = self._models.embedder_argv() if embedder else ()
        cmd = [
            str(self._binary),
            "--vault",
            str(self._vault),
            *flags,
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
