use crate::api::StatsResponse;
use crate::error::Result;
use crate::store::Store;

pub fn stats(store: &Store) -> Result<StatsResponse> {
    let one = |sql: &str| -> Result<i64> { Ok(store.conn.query_row(sql, [], |r| r.get(0))?) };
    let docs = one("SELECT COUNT(*) FROM docs WHERE deleted_at IS NULL")?;
    let deleted_docs = one("SELECT COUNT(*) FROM docs WHERE deleted_at IS NOT NULL")?;
    let chunks = one(
        "SELECT COUNT(*) FROM chunks c JOIN docs p ON p.id = c.doc_id WHERE p.deleted_at IS NULL",
    )?;
    let indexed_words = one("SELECT COUNT(DISTINCT word) FROM word_index")?;
    let links = one("SELECT COUNT(*) FROM links")?;
    let dangling_links = one("SELECT COUNT(*) FROM links WHERE dst_doc_id IS NULL")?;

    let pairs = |sql: &str| -> Result<Vec<(String, i64)>> {
        let mut stmt = store.conn.prepare(sql)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    };
    let links_by_type =
        pairs("SELECT edge_type, COUNT(*) FROM links GROUP BY edge_type ORDER BY COUNT(*) DESC")?;
    let docs_by_source = pairs(
        "SELECT source_dir, COUNT(*) FROM docs WHERE deleted_at IS NULL
         GROUP BY source_dir ORDER BY COUNT(*) DESC",
    )?;

    Ok(StatsResponse {
        docs,
        deleted_docs,
        chunks,
        indexed_words,
        links,
        dangling_links,
        links_by_type,
        docs_by_source,
        last_synced_at: store.get_meta("last_synced_at")?,
    })
}
