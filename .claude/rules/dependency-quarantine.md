# Dependency quarantine

Before adding or bumping an **external** dependency in any language
(crates.io, PyPI, npm, Go modules, git tags, or any other registry), look
up the **publish time of the exact version**. Do not use a version
published less than **2 days** ago.

This is a quiet quarantine against freshly published malicious releases.

- Check the registry API or package page for that version. Do not trust
  memory, download mirrors, or lockfile dates. The `scout` agent can do
  the lookup.
- If the wanted version is too new, take the newest version that is at
  least 2 days old, or wait. Do not add the package if no qualifying
  version exists.
- Applies to new packages and to version bumps of existing ones, git-tag
  pins of forks included. Does not apply to this repo's own workspace /
  path dependencies.
