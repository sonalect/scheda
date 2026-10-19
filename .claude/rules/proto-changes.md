# Changing the proto contract

The contract in `api/scheda/v1` keeps no wire compatibility between
versions. A removed field or enum value is deleted, not `reserved`, and the
fields left may be numbered again in the order they are read. The owner's
decision (19 October 2026): the contract is not published as a wire API,
and its consumers move with each release anyway.

What still applies to such a change: the strict JSON parse refuses a stale
key, the change is written into `CHANGELOG.md`, and the release bumps the
version (`consumers.md`, `.claude/skills/versioning/SKILL.md`).

```protobuf
// BAD
message DocumentQuery {
  reserved 1, 3;
  reserved filter, offset;
  bool include_raw = 2;
}

// GOOD
message DocumentQuery {
  string where = 1;
  bool include_raw = 2;
}
```
