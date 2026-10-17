use rusqlite::{Connection, Result};

fn main() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("
        CREATE TABLE pages(
            rowid INTEGER PRIMARY KEY,
            id TEXT, kind TEXT, title TEXT, description TEXT,
            body TEXT, frontmatter_text TEXT
        );
        CREATE VIRTUAL TABLE pages_fts USING fts5(
            title, description, body, frontmatter_text,
            content='pages', content_rowid='rowid'
        );
        INSERT INTO pages(id, kind, title, description, body, frontmatter_text) VALUES
            ('label:Transformers (architecture)','label','Transformers (architecture)',
             'attention mechanisms paper','self attention and multi head attention blocks','type: label'),
            ('note:tokio','note','tokio','rust async runtime',
             'async tasks and channels in tokio','type: note');
        INSERT INTO pages_fts(rowid, title, description, body, frontmatter_text)
             SELECT rowid, title, description, body, frontmatter_text FROM pages;
    ")?;

    let mut st = conn.prepare(
        "SELECT p.id, bm25(pages_fts)
         FROM pages_fts JOIN pages p ON p.rowid = pages_fts.rowid
        WHERE pages_fts MATCH 'attention' ORDER BY rank",
    )?;
    let ranked: Vec<(String, f64)> = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?
        .collect::<Result<Vec<_>>>()?;
    println!("== ranked (best first) ==");
    for (id, s) in &ranked {
        println!("  {id}  bm25={s}")
    }

    let top: i64 = conn.query_row(
        "SELECT pages_fts.rowid FROM pages_fts WHERE pages_fts MATCH 'attention' ORDER BY rank",
        [],
        |r| r.get::<_, i64>(0),
    )?;
    println!("\ntop rowid={top} — per-column snippet:");
    for (label, col) in [("title", 0), ("desc", 1), ("body", 2), ("front", 3)] {
        let sql = format!(
            "SELECT snippet(pages_fts, {col}, '[', ']', '...', 16) FROM pages_fts WHERE pages_fts.rowid={top}",
        );
        let v: Result<String> = conn.query_row(&sql, [], |r| r.get::<_, String>(0));
        match v {
            Ok(x) => println!("  {label}: {x}"),
            Err(e) => println!("  {label}: ERROR {e}"),
        }
    }
    Ok(())
}
