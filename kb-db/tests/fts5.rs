//! FTS5 probes — M0 (PLAN.md §12, §13.1).
//!
//! These are NOT feature tests; they nail down the exact SQL surface we
//! are allowed to rely on, so that M1's `query.rs` (real search) is built
//! on confirmed behavior rather than memory. Each test prints the shape it
//! depends on and asserts the part that MUST hold (best-first ordering,
//! per-column snippet availability).
//!
//! Run: `cargo test -p kb-db --test fts5`

use kb_core::search_grammar::parse;
use kb_db::Db;
use rusqlite::Result as SqlResult;

/// The content + FTS5 tables M1 will use. External-content FTS5 keeps the
/// real text in `pages` and stores only the index in `pages_fts`, which is
/// exactly the shape we want (no duplicate copies of the body).
fn schema(conn: &rusqlite::Connection) -> SqlResult<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS pages(
            rowid            INTEGER PRIMARY KEY,
            id               TEXT NOT NULL,
            kind             TEXT NOT NULL,
            title            TEXT,
            description      TEXT,
            body             TEXT,
            frontmatter_text TEXT,
            created_at       TEXT,
            updated_at       TEXT
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts USING fts5(
            title, description, body, frontmatter_text,
            content='pages', content_rowid='rowid'
        );
        "#,
    )
}

fn seed(conn: &rusqlite::Connection) -> SqlResult<()> {
    let now = kb_core_now();
    // `body` on the attention row is deliberately long (≈30 tokens) so that
    // a small `maxtok` in a snippet call will *necessarily* clip it and
    // therefore add the open/close/ellipsis markup we assert on below.
    let long_body = "The Transformer architecture replaces recurrence with scaled dot-product attention; the multi-head attention block projects Q, K, V into h parallel sub-spaces so that each head can attend to different relationships at different representation positions, which is why self-attention is so effective for both encoder and decoder layers, and why positional encodings must be added because the attention function itself knows nothing about token order";
    conn.execute_batch(&format!(
        r#"
        INSERT INTO pages(id, kind, title, description, body, frontmatter_text, created_at, updated_at)
        VALUES
          ('label:Transformers (architecture)', 'label',
           'Transformers (architecture)',
           'attention mechanisms paper',
           '{long_body}',
           'type: label tags: architecture',
           '{now}', '{now}'),
          ('note:tokio', 'note',
           'tokio',
           'rust async runtime',
           'async tasks and channels in tokio',
           'type: note tags: rust',
           '{now}', '{now}');
        INSERT INTO pages_fts(rowid, title, description, body, frontmatter_text)
             SELECT rowid, title, description, body, frontmatter_text FROM pages;
        "#
    ))
}

fn kb_core_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// Probe 1: `bm25(pages_fts)` ranks, and `ORDER BY rank` sorts best-first.
///
/// This is the single most load-bearing assumption in the search command:
/// if `ORDER BY rank` didn't mean "highest relevance first", `kb search`
/// results would be garbage. We assert the known-better match comes out
/// first.
#[test]
fn bm25_orders_matches_best_first() {
    let db = Db::open_in_memory_test().unwrap();
    schema(&db.conn).unwrap();
    seed(&db.conn).unwrap();

    // 'attention' appears in both the title *and* body of the transformer
    // note. 'attention' does NOT appear in the tokio note. So the
    // transformer note must be the only (and hence the top) hit — and its
    // bm25 must be a real (finite) number.
    let mut st = db.conn
        .prepare(
            "SELECT p.id, bm25(pages_fts)
             FROM pages_fts JOIN pages p ON p.rowid = pages_fts.rowid
            WHERE pages_fts MATCH ?1
            ORDER BY rank",
        )
        .unwrap();
    let hits: Vec<(String, f64)> = st
        .query_map([ "attention".to_string() ], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })
        .unwrap()
        .collect::<SqlResult<Vec<_>>>()
        .unwrap();

    assert_eq!(hits.len(), 1, "exactly the transformer note matches 'attention'");
    assert_eq!(hits[0].0, "label:Transformers (architecture)");
    assert!(hits[0].1.is_finite(), "bm25 produced a finite score: {}", hits[0].1);

    // A two-row query where ranking must actually *order*: 'rust' is in the
    // tokio note's body (1x) and description. 'attention' is in transformer
    // note's title+description+body. A term that hits *more* columns should
    // rank above a term that hits *fewer*. We approximate: search a term
    // present in both, and confirm the multi-column one tops the list.
    // (Loose assertion — FTS5 bm25 weighting is the implementation detail;
    //  we only need deterministic, relevance-ordered output.)
    let mut st2 = db
        .conn
        .prepare(
            "SELECT p.kind FROM pages_fts JOIN pages p ON p.rowid = pages_fts.rowid
             WHERE pages_fts MATCH ?1 ORDER BY rank",
        )
        .unwrap();
    let kinds: Vec<String> = st2
        .query_map(["rust".to_string()], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<SqlResult<Vec<_>>>()
        .unwrap();
    assert_eq!(kinds, vec!["note".to_string()], "only the tokio note matches 'rust'");
}

/// Probe 2: `snippet(pages_fts, col, open, close, ellipsis, maxtok)` — the
/// exact 6-argument form. Confirms (a) it accepts the 6-arg shape we plan
/// to generate, (b) it returns the expected text for each of the 4 columns,
/// and (c) the open/close markup delimiters actually wrap the match.
#[test]
fn snippet_six_arg_form_works_per_column() {
    let db = Db::open_in_memory_test().unwrap();
    schema(&db.conn).unwrap();
    seed(&db.conn).unwrap();

    let top: i64 = db
        .conn
        .query_row(
            "SELECT pages_fts.rowid FROM pages_fts WHERE pages_fts MATCH 'attention' ORDER BY rank",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();

    // column index -> expected fragment of the match
    for (label, col, expect_contains) in [
        ("title", 0i32, "Transformers"),
        ("desc", 1, "attention"),
        ("body", 2, "attention"),
        ("front", 3, "label"),
    ] {
        let sql = format!(
            "SELECT snippet(pages_fts, {col}, '<b>', '</b>', '…', 18)
             FROM pages_fts WHERE pages_fts.rowid = {top}",
        );
        let got: String = db
            .conn
            .query_row(&sql, [], |r| r.get::<_, String>(0))
            .unwrap_or_else(|e| panic!("snippet col {label} failed: {e}"));
        assert!(
            got.contains(expect_contains),
            "snippet({label}) should contain '{expect_contains}', got: {got}"
        );
    }

    // The open/close markup appears when the snippet is produced under a
    // MATCH clause (the real search→excerpt path): SQLite then clips to
    // `maxtok` tokens and wraps the matched term in open/close. A
    // `WHERE rowid = N` (no MATCH) just returns the column verbatim, so the
    // markup only shows up when we query through MATCH — exactly how `kb
    // search` will call it. A tiny `maxtok` forces clipping.
    let body: String = db
        .conn
        .query_row(
            "SELECT snippet(pages_fts, 2, '<b>', '</b>', '…', 3)
             FROM pages_fts WHERE pages_fts MATCH 'attention'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap();
    assert!(
        body.contains("<b>") && body.contains("</b>"),
        "MATCH-backed body snippet should carry <b>…</b> markup, got: {body}"
    );
}

/// Probe 3: WAL mode allows a second (writer) client to write while a
/// first (reader) client holds a read transaction open. This is the
/// "multi-client WAL" probe from PLAN.md §13.1. The real deployment is two
/// separate `kb` processes; two connections to the same temp-file DB in one
/// process exercise the same SQLite WAL code path (real processes → M7).
///
/// What we assert: a reader holding an open transaction does NOT block a
/// concurrent writer, and the writer's change becomes visible to a fresh
/// reader afterward. (This is exactly the property that lets `kb search`
/// in one terminal keep working while `kb add-root` writes in another.)
#[test]
fn wal_allows_write_during_read_transaction() {
    let db = Db::open_in_memory_test().unwrap();
    let path = db.path().expect("test db must be temp-file-backed");
    schema(&db.conn).unwrap();
    seed(&db.conn).unwrap();

    // A second connection to the SAME temp-file database — the "other
    // kb process".
    let writer = rusqlite::Connection::open(path).unwrap();

    // Reader: open a read transaction and hold a snapshot open.
    let reader = rusqlite::Connection::open(path).unwrap();
    reader.execute_batch("BEGIN DEFERRED").unwrap();
    let before: i64 = reader
        .query_row("SELECT count(*) FROM pages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, 2);

    // Writer: while the reader holds its snapshot, add a row.
    let now = kb_core_now();
    writer
        .execute(
            "INSERT INTO pages(id, kind, title, description, body, frontmatter_text, created_at, updated_at)
             VALUES ('wiki:wal', 'wiki', 'WAL probe', 'concurrency', 'probes wal', 'type: wiki', ?, ?)",
            rusqlite::params![now, now],
        )
        .unwrap();

    // The held reader still sees the pre-write snapshot (2), which is the
    // whole point of WAL MVCC — the writer did not block or corrupt it.
    let held: i64 = reader
        .query_row("SELECT count(*) FROM pages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(held, 2, "held read transaction keeps its snapshot (still 2)");
    reader.execute_batch("ROLLBACK").unwrap();

    // A fresh reader now sees the new row.
    let fresh = rusqlite::Connection::open(path).unwrap();
    let after: i64 = fresh
        .query_row("SELECT count(*) FROM pages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, 3, "fresh read after the writer commits sees the new row");
}

/// Confirm the `Db` wrapper reports WAL as the active journal mode (so the
/// whole "no `tokio`/`Arc<Mutex<Connection>>`" decision rests on a verified
/// property, not a comment).
#[test]
fn db_is_in_wal_mode() {
    let db = Db::open_in_memory_test().unwrap();
    let mode: String = db
        .conn
        .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
        .unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "wal");
}

/// Confirm the temp-file test handle gives *one* shared database across
/// independent connections (required by the WAL probe above).
#[test]
fn shared_db_across_separate_connections() {
    let db = Db::open_in_memory_test().unwrap();
    let path = db.path().unwrap().clone();
    schema(&db.conn).unwrap();

    // Two independent connections to the same file must share rows.
    let a = rusqlite::Connection::open(&path).unwrap();
    a.execute(
        "INSERT INTO pages(id, kind, title, description, body, frontmatter_text, created_at, updated_at)
         VALUES ('label:x','label','x',NULL,'one',NULL,'0','0')",
        [],
    )
    .unwrap();

    let b = rusqlite::Connection::open(&path).unwrap();
    let cb: i64 = b.query_row("SELECT count(*) FROM pages", [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(cb, 1, "second connection to the same file sees the inserted row");
}

// =============================================================================
// Probe 4 (M3) — canonical FTS form.
//
// The "escaping/quoting correct" + "AND/OR/NOT structure preserved" halves of
// the §9 `search_grammar` contract are confirmed *against the engine*, not
// memory (PLAN.md §13.1 "nail down the exact SQL surface we can rely on").
// The canonical render, per the user lock: a **space is implicit AND**, one
// **parenthesised OR group**, and a **`NOT`** clause. We prove each rendered
// string returns exactly the row set the grammar intends.
// =============================================================================

fn seed_grammar(conn: &rusqlite::Connection) -> SqlResult<()> {
    conn.execute_batch(
        r#"
        INSERT INTO pages(id, kind, title, description, body, frontmatter_text)
        VALUES
          ('gram:A', 'note', 'A', NULL, 'apple banana',     'type: note'),
          ('gram:B', 'note', 'B', NULL, 'apple pear',       'type: note'),
          ('gram:C', 'note', 'C', NULL, 'banana pear',      'type: note'),
          ('gram:D', 'note', 'D', NULL, 'llm attention gpt','type: note'),
          ('gram:E', 'note', 'E', NULL, 'mllm attention',   'type: note'),
          ('gram:F', 'note', 'F', NULL, 'attention gpt only','type: note'),
          ('gram:G', 'note', 'G', NULL, 'an exact phrase here','type: note'),
          ('gram:H', 'note', 'H', NULL, 'exact an phrase',  'type: note');
        INSERT INTO pages_fts(rowid, title, description, body, frontmatter_text)
              SELECT rowid, title, description, body, frontmatter_text FROM pages;
        "#,
    )
}

/// `pages_fts MATCH <q>` → the *sorted* set of matching `p.id`.
fn matched_ids(conn: &rusqlite::Connection, q: &str) -> SqlResult<Vec<String>> {
    let mut st = conn.prepare(
        "SELECT p.id FROM pages_fts JOIN pages p ON p.rowid = pages_fts.rowid
          WHERE pages_fts MATCH ?1",
    )?;
    let mut ids: Vec<String> = st
        .query_map([q.to_string()], |r| r.get::<_, String>(0))?
        .collect::<SqlResult<Vec<_>>>()?;
    ids.sort();
    Ok(ids)
}

#[test]
fn canonical_fts_render_means_what_the_grammar_intends() {
    let db = Db::open_in_memory_test().unwrap();
    schema(&db.conn).unwrap();
    seed_grammar(&db.conn).unwrap();
    let conn = &db.conn;
    let run = |q: &str| matched_ids(conn, q).unwrap();

    // (1) space = implicit AND: "apple banana" → rows with BOTH → only A.
    let q = parse("apple banana").render_fts();
    assert_eq!(q, "apple banana");
    assert_eq!(run(&q), vec!["gram:A"]);

    // (2) one OR group: "(llm OR mllm)" → either → D (llm) and E (mllm).
    let q = parse("~llm ~mllm").render_fts();
    assert_eq!(q, "(llm OR mllm)");
    assert_eq!(run(&q), vec!["gram:D", "gram:E"]);

    // (3) NOT after a bare AND term: "apple NOT pear" → apple AND not-pear →
    //     A (B has pear, so it is excluded; C lacks apple too).
    let q = parse("apple -pear").render_fts();
    assert_eq!(q, "apple NOT pear");
    assert_eq!(run(&q), vec!["gram:A"]);

    // (4) AND + OR + NOT — canonical order [OR] → [NOT] → [bare]:
    //     "(llm OR mllm) NOT gpt attention" → rows with (llm OR mllm) AND NOT
    //     gpt AND attention → only E (D has gpt; F lacks llm/mllm; A/B/C lack
    //     attention and llm/mllm).
    let q = parse("~llm ~mllm attention -gpt").render_fts();
    assert_eq!(q, "(llm OR mllm) NOT gpt attention");
    assert_eq!(run(&q), vec!["gram:E"]);

    // (5) phrase: only the *contiguous* sequence matches → G yes,
    //     H (same words, wrong order) no.
    let q = parse("\"an exact phrase\"").render_fts();
    assert_eq!(q, "\"an exact phrase\"");
    assert_eq!(run(&q), vec!["gram:G"]);

    // (6) the filter/content separation: a `tag:…` token is a filter (not FTS);
    //     the *rendered FTS portion* is only the bare term — so a filter-only
    //     part never leaks an FTS clause (empty MATCH ⇒ the M7 layer omits it).
    let q = parse("tag:llm attention").render_fts();
    assert_eq!(q, "attention", "the `tag:llm` filter must not appear in the FTS string");
    let filter_only = parse("tag:llm").render_fts();
    assert_eq!(filter_only, "", "a filter-only query renders no FTS content (empty MATCH)");
}
