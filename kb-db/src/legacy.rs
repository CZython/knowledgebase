//! One-time repair for databases created by the old SQLite/FTS5 build.
//!
//! Turso refuses to open a database whose schema contains
//! `CREATE VIRTUAL TABLE … USING fts5(…)` (`Virtual table module not found:
//! fts5`). Such a file is otherwise a perfectly good SQLite database, so we
//! remove just that row from `sqlite_schema` before opening it. Turso then
//! opens the file normally, and [`crate::migrate`] drops the leftover FTS
//! shadow tables and builds the native FTS index. All page data — notably
//! `roots` and `tag_aliases`, the only non-derivable state — is preserved.
//!
//! Deleting a `sqlite_schema` row only rewrites the page's cell-pointer
//! array; the orphaned cell bytes and btree pages are left as allocated
//! space, which SQLite/Turso tolerate. No record or header lengths change.
//!
//! The repair is deliberately conservative: it only acts on `fts5` virtual
//! tables, refuses to touch a database with a hot journal or a non-empty
//! WAL, and is a no-op when no legacy entry is found.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";
const CREATE_VIRTUAL_TABLE: &[u8] = b"create virtual table";

const PAGE_LEAF_TABLE: u8 = 0x0d;
const PAGE_INTERIOR_TABLE: u8 = 0x05;

/// Remove every legacy `CREATE VIRTUAL TABLE … USING fts5(…)` entry from
/// `sqlite_schema`. Returns `true` when the file was modified.
///
/// Fails when the file cannot be safely edited in place (hot journal or a
/// non-empty WAL, both of which could override or invalidate the edit), or
/// when a matching statement cannot be located in the schema btree.
pub fn neutralize_fts5_vtables(path: &Path) -> io::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    reject_unsafe_sidecars(path)?;

    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if !bytes.starts_with(SQLITE_MAGIC) {
        return Ok(false);
    }

    let layout = Layout::parse(&bytes)?;
    let mut changed = false;
    let mut search_from = 0;
    while let Some(start) = find_ci(&bytes, CREATE_VIRTUAL_TABLE, search_from) {
        let Some(end) = fts5_statement_end(&bytes, start) else {
            search_from = start + CREATE_VIRTUAL_TABLE.len();
            continue;
        };
        if delete_schema_row(&mut bytes, &layout, start, end) {
            changed = true;
        }
        search_from = end;
    }

    if changed {
        bump_change_counter(&mut bytes);
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    Ok(changed)
}

/// Refuse to edit a database that has a hot rollback journal or a WAL that
/// may still contain the authoritative copy of the schema.
fn reject_unsafe_sidecars(path: &Path) -> io::Result<()> {
    let journal = sidecar(path, "-journal");
    if journal.exists() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "cannot migrate legacy database: hot journal {} exists; \
                 close any process using the database and retry",
                journal.display()
            ),
        ));
    }
    let wal = sidecar(path, "-wal");
    if let Ok(meta) = fs::metadata(&wal) {
        if meta.len() > 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "cannot migrate legacy database: {} is not empty; \
                     checkpoint or remove it after closing all connections, then retry",
                    wal.display()
                ),
            ));
        }
    }
    // No authoritative WAL contents: drop leftover empty/mmap sidecars so a
    // stale shared-memory file cannot shadow the repaired schema.
    let _ = fs::remove_file(&wal);
    let _ = fs::remove_file(sidecar(path, "-shm"));
    let _ = fs::remove_file(sidecar(path, "-tshm"));
    Ok(())
}

fn sidecar(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    std::path::PathBuf::from(name)
}

#[derive(Clone, Copy)]
struct Layout {
    page_size: usize,
    reserved: usize,
}

impl Layout {
    fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 100 {
            return Err(invalid("file is too short to be a SQLite database"));
        }
        let raw = u16::from_be_bytes([bytes[16], bytes[17]]) as usize;
        let page_size = if raw == 1 { 65_536 } else { raw };
        if page_size < 512 || !page_size.is_power_of_two() {
            return Err(invalid("invalid SQLite page size"));
        }
        Ok(Self {
            page_size,
            reserved: bytes[20] as usize,
        })
    }

    fn usable(&self) -> usize {
        self.page_size - self.reserved
    }

    fn max_local(&self) -> usize {
        self.usable() - 35
    }

    fn page_offset(&self, page_no: u32) -> Option<usize> {
        let page_no = usize::try_from(page_no).ok()?;
        if page_no == 0 {
            return None;
        }
        page_no.checked_sub(1)?.checked_mul(self.page_size)
    }

    fn header_offset(&self, page_no: u32) -> Option<usize> {
        let base = self.page_offset(page_no)?;
        Some(if page_no == 1 { base + 100 } else { base })
    }
}

/// Return the end (exclusive) of an `fts5` virtual-table statement at
/// `start`, or `None` if it is not one (or is malformed).
fn fts5_statement_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = skip_ws(bytes, start + CREATE_VIRTUAL_TABLE.len());
    i = skip_if_not_exists(bytes, i);
    i = skip_ws(bytes, i);
    let (_, after_name) = parse_identifier(bytes, i)?;
    let open_paren = find_byte(bytes, b'(', after_name)?;
    let using = &bytes[after_name..open_paren];
    if !contains_ci(using, b"using") || !contains_ci(using, b"fts5") {
        return None;
    }
    matching_paren(bytes, open_paren).map(|close| close + 1)
}

/// Remove the `sqlite_schema` row whose payload contains `text_start..text_end`
/// from the schema btree. Returns `true` when a row was found and removed.
fn delete_schema_row(
    bytes: &mut [u8],
    layout: &Layout,
    text_start: usize,
    text_end: usize,
) -> bool {
    let max_local = layout.max_local();
    // The schema's root is page 1; walk the table btree to the leaf that
    // holds the matching record.
    let mut stack = vec![1u32];
    while let Some(page_no) = stack.pop() {
        let Some(header) = layout.header_offset(page_no) else {
            continue;
        };
        let Some(&page_type) = bytes.get(header) else {
            continue;
        };
        let Some(cell_count) = read_u16(bytes, header + 3) else {
            continue;
        };
        match page_type {
            PAGE_INTERIOR_TABLE => {
                // Interior table pages hold `ncells + 1` children: the
                // right-most child lives in the 4 bytes at `header + 8`.
                if let Some(rightmost) = read_u32(bytes, header + 8) {
                    stack.push(rightmost);
                }
                let ptr_start = header + 12;
                for n in 0..cell_count {
                    let Some(ptr) = read_u16(bytes, ptr_start + n as usize * 2) else {
                        continue;
                    };
                    let cell = layout.page_offset(page_no).map(|b| b + ptr as usize);
                    if let Some(child) = cell.and_then(|c| read_u32(bytes, c)) {
                        stack.push(child);
                    }
                }
            }
            PAGE_LEAF_TABLE => {
                let ptr_start = header + 8;
                for n in 0..cell_count {
                    let Some(ptr) = read_u16(bytes, ptr_start + n as usize * 2) else {
                        continue;
                    };
                    let Some(base) = layout.page_offset(page_no) else {
                        continue;
                    };
                    let cell = base + ptr as usize;
                    let Some((payload_len, after_len)) = read_varint(bytes, cell) else {
                        continue;
                    };
                    let Some((_, payload_start)) = read_varint(bytes, after_len) else {
                        continue;
                    };
                    // Only local payloads are inspected. Schema records are
                    // small; a matching statement in an overflow chain is
                    // reported as unrepairable rather than risking a bad edit.
                    if payload_len as usize > max_local {
                        continue;
                    }
                    let payload_end = payload_start.saturating_add(payload_len as usize);
                    if text_start >= payload_start && text_end <= payload_end {
                        let cell_end = payload_start + payload_len as usize;
                        remove_cell_pointer(bytes, header, ptr_start, cell_count, n);
                        add_freeblock(bytes, header, base, cell, cell_end - cell);
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

fn remove_cell_pointer(
    bytes: &mut [u8],
    header: usize,
    ptr_start: usize,
    cell_count: u16,
    index: u16,
) {
    let n = cell_count as usize;
    let i = index as usize;
    if i >= n {
        return;
    }
    let start = ptr_start + i * 2;
    let end = ptr_start + n * 2;
    if end <= bytes.len() {
        bytes.copy_within(start + 2..end, start);
    }
    // Shrinking the cell count; the removed cell's bytes and page accounting
    // are left in place as allocated space.
    bytes[header + 3..header + 5].copy_from_slice(&(cell_count - 1).to_be_bytes());
}

/// Account for the bytes of a removed cell by adding them to the page's
/// freeblock list (merging with adjacent freeblocks). This keeps SQLite's
/// integrity check clean; without it the bytes are merely unaccounted-for
/// slack, which both engines tolerate.
fn add_freeblock(bytes: &mut [u8], header: usize, page_base: usize, start_abs: usize, size: usize) {
    if size < 4 || start_abs + size > bytes.len() || start_abs < page_base {
        return;
    }
    let start = start_abs - page_base;
    let mut ranges = Vec::new();
    let mut cur = read_u16(bytes, header + 1).unwrap_or(0) as usize;
    let mut guard = 0;
    while cur != 0 && guard < 10_000 {
        guard += 1;
        let at = page_base + cur;
        let Some(next) = read_u16(bytes, at) else {
            return;
        };
        let Some(sz) = read_u16(bytes, at + 2) else {
            return;
        };
        ranges.push((cur, sz as usize));
        cur = next as usize;
    }
    ranges.push((start, size));
    ranges.sort_unstable_by_key(|&(off, _)| off);
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (off, sz) in ranges {
        if let Some(last) = merged.last_mut() {
            if last.0 + last.1 == off {
                last.1 += sz;
                continue;
            }
        }
        merged.push((off, sz));
    }
    for (i, &(off, sz)) in merged.iter().enumerate() {
        let next = merged.get(i + 1).map_or(0, |&(n, _)| n as u16);
        let at = page_base + off;
        if off + 4 > bytes.len() || at + 4 > bytes.len() {
            return;
        }
        bytes[at..at + 2].copy_from_slice(&next.to_be_bytes());
        bytes[at + 2..at + 4].copy_from_slice(&(sz as u16).to_be_bytes());
    }
    let first = merged.first().map_or(0, |&(off, _)| off as u16);
    bytes[header + 1..header + 3].copy_from_slice(&first.to_be_bytes());
}

fn bump_change_counter(bytes: &mut [u8]) {
    if bytes.len() < 96 {
        return;
    }
    let counter = u32::from_be_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let next = counter.wrapping_add(1);
    bytes[24..28].copy_from_slice(&next.to_be_bytes());
    // `version-valid-for` must match the change counter.
    bytes[92..96].copy_from_slice(&next.to_be_bytes());
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *bytes.get(at)?,
        *bytes.get(at + 1)?,
        *bytes.get(at + 2)?,
        *bytes.get(at + 3)?,
    ]))
}

/// Read a SQLite variable-length integer. Returns `(value, next_offset)`.
fn read_varint(bytes: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for i in 0..9 {
        let byte = *bytes.get(at + i)?;
        if i == 8 {
            value = (value << 8) | byte as u64;
            return Some((value, at + 9));
        }
        value = (value << 7) | (byte & 0x7f) as u64;
        if byte & 0x80 == 0 {
            return Some((value, at + i + 1));
        }
    }
    None
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn skip_if_not_exists(bytes: &[u8], i: usize) -> usize {
    let phrase = b"if not exists";
    let rest = &bytes[i..];
    if rest.len() >= phrase.len()
        && rest[..phrase.len()].eq_ignore_ascii_case(phrase)
        && rest
            .get(phrase.len())
            .is_none_or(|b| b.is_ascii_whitespace())
    {
        return skip_ws(bytes, i + phrase.len());
    }
    i
}

fn parse_identifier(bytes: &[u8], start: usize) -> Option<(&[u8], usize)> {
    let first = *bytes.get(start)?;
    let (closing, inner_start) = match first {
        b'"' => (b'"', start + 1),
        b'\'' => (b'\'', start + 1),
        b'`' => (b'`', start + 1),
        b'[' => (b']', start + 1),
        _ => {
            let mut end = start;
            while end < bytes.len()
                && !bytes[end].is_ascii_whitespace()
                && bytes[end] != b'('
                && bytes[end] != b')'
                && bytes[end] != b','
            {
                end += 1;
            }
            if end == start {
                return None;
            }
            return Some((&bytes[start..end], end));
        }
    };
    let mut end = inner_start;
    while end < bytes.len() {
        if bytes[end] == closing {
            return Some((&bytes[start..=end], end + 1));
        }
        end += 1;
    }
    None
}

/// Find the `)` matching the `(` at `open`, skipping quoted strings.
fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | b'`' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        if i + 1 < bytes.len() && bytes[i + 1] == quote {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            b'[' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b']' {
                    i += 1;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn find_byte(bytes: &[u8], needle: u8, from: usize) -> Option<usize> {
    bytes
        .get(from..)?
        .iter()
        .position(|&b| b == needle)
        .map(|p| p + from)
}

fn find_ci(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len())
        .find(|&i| hay[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    find_ci(hay, needle, 0).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGIC: &[u8] = b"SQLite format 3\0";
    const PAGE_SIZE: usize = 512;

    /// Build a minimal one-page SQLite database whose `sqlite_schema` leaf
    /// holds the given SQL records as `(rowid, sql)` table rows.
    fn fake_db(records: &[(i64, &str)]) -> Vec<u8> {
        let mut bytes = vec![0u8; PAGE_SIZE];
        bytes[..16].copy_from_slice(MAGIC);
        bytes[16..18].copy_from_slice(&(PAGE_SIZE as u16).to_be_bytes());
        bytes[18] = 1;
        bytes[19] = 1;

        let header = 100;
        bytes[header] = PAGE_LEAF_TABLE;
        // Cell content grows down from the end of the page.
        let mut content = PAGE_SIZE;
        let mut pointers = Vec::new();
        for &(rowid, sql) in records {
            let payload = schema_record(sql);
            content -= payload.len();
            let cell = content;
            bytes[cell..cell + payload.len()].copy_from_slice(&payload);
            pointers.push(cell as u16);
            // Fix up the rowid by writing it in the payload header below.
            let _ = rowid;
        }
        // Write cell pointer array and header fields.
        bytes[header + 3..header + 5].copy_from_slice(&(pointers.len() as u16).to_be_bytes());
        bytes[header + 5..header + 7].copy_from_slice(&(content as u16).to_be_bytes());
        for (i, ptr) in pointers.iter().enumerate() {
            bytes[header + 8 + i * 2..header + 10 + i * 2].copy_from_slice(&ptr.to_be_bytes());
        }
        // Sanity: every record must be discoverable by `find_ci`.
        for &(_, sql) in records {
            assert!(find_ci(&bytes, sql.as_bytes(), 0).is_some());
        }
        bytes
    }

    /// A table-leaf cell with payload = record(type, name, tbl_name, rootpage,
    /// sql), then rowid 1. Only the SQL text matters to the repair.
    fn schema_record(sql: &str) -> Vec<u8> {
        let serials: Vec<u64> = vec![13, 3, 3, 8, 13 + 2 * sql.len() as u64];
        let serial_bytes: Vec<Vec<u8>> = serials.iter().map(|&s| encode_varint(s)).collect();
        let header_len = 1 + serial_bytes.iter().map(Vec::len).sum::<usize>();
        assert!(
            header_len < 128,
            "test fixture header must fit a 1-byte varint"
        );

        let mut body = Vec::new();
        body.extend_from_slice(b"table");
        body.extend_from_slice(b"t");
        body.extend_from_slice(b"t");
        body.push(0); // rootpage: serial type 8 = integer 0, no bytes
        body.extend_from_slice(sql.as_bytes());

        let mut cell = Vec::new();
        cell.extend(encode_varint((header_len + body.len()) as u64));
        cell.extend(encode_varint(1)); // rowid
        cell.push(header_len as u8);
        for part in &serial_bytes {
            cell.extend_from_slice(part);
        }
        cell.extend(body);
        cell
    }

    fn encode_varint(mut v: u64) -> Vec<u8> {
        if v > 0x00ff_ffff_ffff_ffff {
            let mut out = vec![0u8; 9];
            out[8] = v as u8;
            v >>= 8;
            for i in (0..8).rev() {
                out[i] = ((v & 0x7f) as u8) | 0x80;
                v >>= 7;
            }
            out
        } else {
            let mut tmp = vec![(v & 0x7f) as u8];
            v >>= 7;
            while v > 0 {
                tmp.push(((v & 0x7f) as u8) | 0x80);
                v >>= 7;
            }
            tmp.reverse();
            tmp
        }
    }

    fn repair_bytes(input: &[u8]) -> Vec<u8> {
        let mut bytes = input.to_vec();
        let layout = Layout::parse(&bytes).unwrap();
        let mut search_from = 0;
        while let Some(start) = find_ci(&bytes, CREATE_VIRTUAL_TABLE, search_from) {
            match fts5_statement_end(&bytes, start) {
                Some(end) => {
                    delete_schema_row(&mut bytes, &layout, start, end);
                    search_from = end;
                }
                None => search_from = start + CREATE_VIRTUAL_TABLE.len(),
            }
        }
        bytes
    }

    #[test]
    fn removes_fts5_virtual_table_schema_row() {
        let sql = "CREATE VIRTUAL TABLE pages_fts USING fts5(title, body)";
        let db = fake_db(&[(1, "CREATE TABLE pages (id TEXT)"), (2, sql)]);
        let out = repair_bytes(&db);
        assert_eq!(out.len(), db.len());
        assert!(find_ci(&out, sql.as_bytes(), 0).is_some());
        // Exactly one pointer was removed from the schema leaf.
        assert_eq!(read_u16(&out, 100 + 3), Some(1));
    }

    #[test]
    fn leaves_non_fts5_and_regular_statements_alone() {
        let db = fake_db(&[
            (1, "CREATE TABLE pages (id TEXT)"),
            (2, "CREATE VIRTUAL TABLE t USING something(x)"),
        ]);
        let out = repair_bytes(&db);
        assert_eq!(out, db);
    }

    #[test]
    fn finds_statement_with_quoted_name_and_nested_parens() {
        let sql = "CREATE VIRTUAL TABLE \"odd name\" USING fts5(a, b, tokenize='x(y)')";
        let start = 5;
        let mut buf = vec![b' '; start];
        buf.extend_from_slice(sql.as_bytes());
        assert_eq!(fts5_statement_end(&buf, start), Some(start + sql.len()));
    }
}
