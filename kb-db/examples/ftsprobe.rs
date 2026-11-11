//! Probe Turso's native FTS surface: `fts_match` / `fts_score` /
//! `fts_highlight`. Run with `cargo run -p kb-db --example ftsprobe`.

use kb_db::sql::{Connection, Result};

fn main() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        r#"
        CREATE TABLE pages(
            id TEXT, title TEXT, description TEXT, body TEXT, frontmatter_text TEXT
        );
        CREATE INDEX idx_pages_fts ON pages
          USING fts (title, description, body, frontmatter_text);
        INSERT INTO pages(id, title, description, body, frontmatter_text) VALUES
          ('label:Transformers (architecture)',
           'Transformers (architecture)',
           'attention mechanisms paper',
           'The Transformer architecture replaces recurrence with scaled dot-product attention.',
           'type: label tags: architecture'),
          ('note:tokio', 'tokio', 'rust async runtime',
           'async tasks and channels in tokio', 'type: note tags: rust');
        "#,
    )?;

    let cols = "title, description, body, frontmatter_text";

    println!("fts_score / fts_match for 'attention':");
    let stmt = conn.prepare(format!(
        "SELECT id, fts_score({cols}, 'attention') AS score
           FROM pages WHERE fts_match({cols}, 'attention')
          ORDER BY score DESC"
    ))?;
    let hits = stmt.query_map((), |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?;
    for hit in hits {
        let (id, score) = hit?;
        println!("  {id}  score={score}");
    }

    println!("\nfts_highlight for 'attention':");
    let stmt = conn.prepare(format!(
        "SELECT id, fts_highlight({cols}, '[', ']', 'attention')
           FROM pages WHERE fts_match({cols}, 'attention')"
    ))?;
    let hits = stmt.query_map((), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for hit in hits {
        let (id, highlighted) = hit?;
        println!("  {id}: {highlighted}");
    }
    Ok(())
}
