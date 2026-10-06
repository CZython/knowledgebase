---
title: Scratch — no type
tags:
  - scratch
---

# Scratch — no type

A page with NO `type:` field. Per PLAN.md §4, this is NOT indexed — the
M5 indexer skips it (with a warning). It still parses: `title`, `tags`, and
`body` all come through, but `is_indexable()` is `false`.

- no `type:` → not in the `pages` table
- can still be opened directly via the single-file tool
