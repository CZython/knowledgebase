# Golden fixture tree (M0)

The **single source of truth** fixture set that the `kb-core::frontmatter`
parser (M1) and the `kb-db` indexer (M5) must accept, and that the search /
tag / link / browse integration tests (M7) run against. Contents are **verbatim
from PLAN.md Appendix A.1–A.1b** — do not hand-edit the notes without updating
the appendix and the golden test.

## Layout = three registered roots

Each top-level directory here is a *distinct root* (the `~/notes`, `~/work`,
`~/library` of the plan). `kb` is never given `kb-core/testdata` itself — it is
given each of the three as a registered root, so multi-root identity is real:

| Root (this dir)    | MD page(s)                                   | non-MD (label targets, never indexed) |
|--------------------|----------------------------------------------|----------------------------------------|
| `notes/`           | `wiki/transformer.md` (A.1)                  | —                                      |
| `work/`            | `projects/api-gateway/README.md` (A.2)       | —                                      |
| `library/`         | `llm.md` (A.3), `transformer.md` (A.1b), `czech.md` (A.4) | `papers/*.pdf`, `books/*.epub` |

The non-MD `papers/` `books/` entries exist **only** as the targets named in the
label bodies (`## Files` in A.3, the path-list body in A.1b). They have zero rows
in the DB — they are *referenced resources*, surfaced when `kb tag llm` / a
label is expanded, read live from disk.

## Identity cheat-sheet (A.6)

| File (rel to root)            | type   | slug source | display id                  | root       |
|-------------------------------|--------|-------------|-----------------------------|------------|
| `notes/wiki/transformer.md`   | wiki   | `title:`    | `wiki:Transformer (architecture)` | `notes` |
| `work/projects/api-gateway/README.md` | note | `title:` | `note:API Gateway — notes` | `work` |
| `library/llm.md`              | label  | `title:`    | `label:LLM`                   | `library`|
| `library/czech.md`            | label  | `title:`    | `label:Czech`                 | `library`|
| `library/transformer.md`      | label  | `title:`    | `label:Transformer (papers)`  | `library`|

**Collision note:** `notes/wiki/transformer.md` and `library/transformer.md`
share a filename but differ in slug AND root → different `abs_path` (the real key)
and different display ids (`wiki:` vs `label:`). The DB never relies on display-id
uniqueness — it joins on `abs_path`.
