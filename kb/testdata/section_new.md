# Release Notes

## 2026.10

Initial public release of `kb` with FTS5 search and a Verus-verified core.

## 2026.09

Pre-release. Fixed tag alias import to accept both single-tag and
list-of-tag shapes. FTS index is now rebuilt when `include_hidden`
flips.

## 2026.08

Internal alpha. First working `kb search`. Removed the legacy
`kb index --rebuild` flag (use `kb reindex` instead).
