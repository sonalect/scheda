# CI on six platforms

Work on the branch `ci-six-platforms` (21 October 2026), after the
repository went public: `.github/workflows/ci.yml` on Linux, macOS, and
Windows, each on amd64 and arm64, as `../protocontract-buffa.rs` does in
its release workflow. Manual runs only (`workflow_dispatch`).

## What runs where

- Bazel (`bazel build //...`, `bazel test //...`, every test named):
  linux-amd64 (`ubuntu-26.04`), linux-arm64 (`ubuntu-26.04-arm`),
  macos-amd64 (`macos-26-intel`), macos-arm64 (`macos-26`), windows-amd64
  (`windows-2025`).
- Cargo (clippy, the library's tests, then the CLI's): windows-arm64
  (`windows-11-arm`). Bazel cannot build ring there: ring's build script
  switches to clang on Windows arm64, and rules_rust hands every build
  script the cl.exe `CFLAGS` of the C++ toolchain; `build_script_env` only
  appends to `CFLAGS`, and `build_script_use_cc_toolchain = "off"` sets
  `CC` to rules_rust's `no_cc.exe`. Lance needs ring (lance-namespace's
  REST client, not optional).

## Found and fixed on the branch

- crate_universe's default platform list has no `x86_64-apple-darwin` and
  no `aarch64-pc-windows-msvc`: every Rust target was silently incompatible
  there (`supported_platform_triples` in `rust.MODULE.bazel`).
- Windows: Unix-only imports and `rel_posix` were unused (deny warnings);
  cl.exe hits `MAX_PATH` on zstd-sys's
  `dictBuilder\..\compress\zstd_compress_internal.h` (266 characters under
  `--output_user_root=C:/b`, 257 on amd64 and 259 on arm64 under
  `--output_base=C:/b`, the limit); LNK4315 from rules_cc's `fastbuild`
  feature (`build:windows --features=-fastbuild`); test mirrors' URLs were
  `file://C:\…` (`file_url` in the test helpers).
- `/dev/full` is Linux-only (`another_stdout_failure_is_storage`).
- Lance unoptimized trains IVF_PQ many times slower: the deps now build
  with `opt-level = 2` (`.bazelrc` `per_crate_rustc_flag`, Cargo
  `[profile.dev.package."*"]`).
- CI hygiene: Android/JAVA variables left pointing at removed toolchains,
  git's default-branch hint, the rustc archives' sha256 (rules_rust knows
  none for 1.99.1).

- Windows packing (a library bug): `packs()` listed a pack by its `.pack`
  file; a removal that took the `.idx` and left the `.pack` (Windows keeps a
  file another reader has open) made every later merge fail with `STORAGE`
  "file not found". A pack without its index is no pack now, and a pack
  whose index goes during the merge is passed over
  (`pack::tests::a_pack_file_without_its_index_is_not_merged`, which failed
  the same way on Linux before the fix).
- `indexer::tests::a_round_that_fails_or_panics_is_reported_and_tried_again`
  read the first item of its watch after the write: on a slow machine that
  item was already the retry's state. With a 500 ms pause the old test hung,
  the new one passes.

- Two first opens of one clone at once each generated a writer identity
  and replaced `.scheda/writer`: two identities for one clone everywhere,
  and on Windows the second `MoveFileExW` replace was `ACCESS_DENIED`. A
  generated identity is now published by a rename that refuses to replace
  (`fs::create_private`); the loser reads the winner's.
  `writer::tests::first_loads_at_once_agree_on_one_token` failed 3 of 3
  runs on the old code, Linux.

- `VaultInfo.path` on Windows was the verbatim `\\?\C:\…` of
  `canonicalize`: the library resolves the Vault with `fs::canonicalize`,
  which gives the plain form when one exists (the owner's decision, 21
  October 2026).

- Windows `MAX_PATH` (cl.exe and link.exe are not long-path aware, though
  the runners set `LongPathsEnabled`): `blocking_doc_test` failed with
  LNK1181 because the path of `librustc_std_workspace_alloc-*.rlib` under
  the runner's runfiles was 270 characters. `.bazelrc` builds for
  `//bazel:host` named `w` (`bazel-out/w-fastbuild`, 10 shorter), and
  `rust.toolchain(aliases=...)` names the Windows toolchain repositories
  `rwx`/`rwa` instead of `rw-<hash>` (10 shorter): 250 now.
- Disk on the Windows runners: windows-2025 has C: (system disk, slow
  flushes) and D: (local). Bazel's output base is `D:/b` in CI. Probe, one
  test alone: `pack::tests::many_appends_…` 9 s on C:, 3 s on D:;
  `reads::tests::the_first_version_of_a_long_history_…` 56 s on C:, 3 s on
  D:. windows-11-arm has only C:.
- `reads::tests::the_first_document_of_a_big_collection_…` wrote 100 000
  files and `git add`ed them: alone over 600 s on windows-arm64, 134 s on
  macOS arm64 (RAM disk or not), 2–4 s locally; running in two suites at
  once it slowed every test beside it (the `pack::` tests, 2 s alone on
  macOS, took minutes inside the suite). The fixture now builds the
  commit in memory and writes it as one pack (libgit2 mempack): 1 s
  locally. `git fast-import` was no help: it inserts tree entries by a
  linear search, 16 s of CPU for 100 000 entries in one folder.

- The `mirror::` failures on windows-amd64 under Bazel: the library reads
  `init.defaultBranch` through libgit2, which ignores `GIT_CONFIG_*` and
  searched the machine's configuration; under Bazel (no `HOME`, no
  `USERPROFILE`) it found Git for Windows' system one, under Cargo the
  runner's `~/.gitconfig` (`main`, from the workflow). The Vaults went on
  another branch than the test mirrors' `main`, and a clone checked out
  nothing. Reproduced on Linux with `HOME` holding `defaultBranch =
  master`: 22 of 32 `mirror::` tests failed. The integration tests now
  point libgit2's search paths at an absent file
  (`common::isolate_library_git`), as the unit tests did.
- Integration tests compared `VaultInfo.path` with `Path::canonicalize`,
  the verbatim form on Windows (`common::resolved` gives the plain one).
- `tables::tests::closing_a_vault_in_the_middle_of_a_slow_embedding_…`
  counted the embedder's calls from zero; on a loaded runner the round ran
  on before the test looked. It now counts from the start of `close`. A
  blocking port's reply stream is pulled on the blocking pool by
  protocontract (`offload::pump`, a channel of 16): after each send it
  starts the next item, and only the following send finds the reader gone.
  So one portion of a blocking embedder may begin after `Close` returned
  (seen on windows-amd64: 2 at close, 3 later), never two. Whether `Close`
  should wait for that, or protocontract not start an item for a reader
  that is gone, is open for the owner.
- 64 writes queued on one handle wait 60 s at most in all (the turn is
  FIFO, the deadline runs from the call): fine alone (7 ms a write on D:),
  ABORTED with four suites at once on windows-amd64 and macos-amd64, as
  `reads::two_writes_in_one_second_…` found no four writes within one
  second. CI runs one test binary at a time on macOS and Windows
  (`--local_test_jobs=1`).

## Measurements

Four local cores (`taskset -c 0-3`), Bazel build, one test alone:

| Test | deps opt 0 | deps opt 2 |
| - | - | - |
| `search::on_lance::a_search_through_the_vector_index_…` | 34.4 s | 5.8 s |
| `texts::on_lance::a_search_through_the_index_of_the_texts_…` | 23.2 s | 0.95 s |

CI, `//rust/scheda:integration_test` (run 4 is deps opt 2):

| Platform | run 2/3 | run 4 |
| - | - | - |
| linux-amd64 | 164–199 s | 76 s |
| linux-arm64 | over 180 s, deadlines missed | 59 s |

Cold Bazel build on linux-amd64: job 19 min with deps opt 0, 31 min with
opt 2 (critical path 21 min).

macOS arm64 (run 3, test files already on a RAM disk): `scheda_thin_test`
and `scheda_test` time out at 300 s, `integration_thin_test` takes 145 s.
Not the disk: `reads::tests::the_first_document_of_a_big_collection_…`
writes `BIG = 100_000` files and commits them with git.

## State and what is left

Released in 0.24.1 (21 October 2026); the branch is merged into `main`.
Run 13 (`39580201442`): the five Bazel platforms green; windows-arm64
green for the library (clippy, tests with every feature and without),
red only in the CLI step (esaxx-rs, below). The last commit before the
release, `/NOIMPLIB /NOEXP` on Windows, has not run on the runners yet.
CI stays manual (`workflow_dispatch`), the owner's decision.

The owner deferred the rest to a later session (21 October 2026):

- The CLI does not link with Cargo on Windows: esaxx-rs (tokenizers, from
  model2vec-rs, both of its tokenizer features) builds with
  `.static_crt(true)`, `/MT`, against `/MD` of mimalloc and the rest
  (LNK2038). Bazel links it only because the toolchain's `/MD` comes after
  esaxx's `/MT` in the compile line. esaxx-rs 0.1.10 (2023) is its latest
  release, model2vec-rs 0.3.0 its latest, and both of model2vec-rs's
  tokenizer features turn on `tokenizers/esaxx_fast`. Only the CLI depends
  on it. The owner's decision: a Sonalect fork of esaxx-rs without
  `static_crt(true)`, under `[patch.crates-io]` (every crate `/MD` as under
  Bazel, `cargo install --git` included; Knowqore may meet the same). The
  owner makes the fork together with the agent.
- `aspect_rules_py` 1.12.1 (the Sonalect fork) does not know the `abi3t`
  tag of uv 0.11.3+ locks: eight DEBUG warnings per run (cryptography's
  cp315 wheels, skipped, which 3.14 does not use anyway). Knowqore's
  `uv.lock` has them too: fix the fork once for both.
- Release automation as in protocontract is not done.
- A blocking port may be called once after `Close` returned (the
  protocontract pull above). **Decided 23 October 2026:** protocontract
  starts no item for a reader that is gone (`sender.is_closed()` before
  each `next()` in `offload::pump`); handed to an agent in
  `protocontract-buffa.rs` by the owner. When `protocontract` 0.4.2 is
  out, Scheda bumps it in `Cargo.lock` (the range `0.4.1` already takes
  it) and repins; no other change.
- Bazel's note "tests whose specified size is too big" stays: it measures
  against the default ranges, not `--test_timeout`, so a size right for a
  workstation is wrong for a runner and the other way round.

Knowqore's plan for the same work: `../knowqore/docs/41-ci-six-platforms.md`
(written 21 October 2026, not committed there yet).
