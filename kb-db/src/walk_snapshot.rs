//! walk_snapshot — the *real* `std::fs` builder that fills a `kb_core::
//! fs_snapshot::Walk` (the verified half) with an actual tree (PLAN §10
//! `walk_snapshot` + §9 "Assumed: snapshot builder (real std::fs reads,
//! documented read-only)").
//!
//! The builder records **every** node it sees (dir / file / symlink) plus each
//! dir's immediate children; it *does not* apply the skip rules. Applying the
//! §3 skip rules (symlinks never followed, dot-dirs skipped unless
//! `include_hidden`, extension allow-list) is the *pure, verified* walker
//! `Walk::enumerate`'s job. Keeping the IO boundary this thin is what lets
//! Verus prove the walker is read-only/complete.
//!
//! Symlink handling (permanent rule §3): we use `symlink_metadata` (which does
//! not follow links) and record symlinks as `NodeKind::Symlink` *without*
//! recursing into them — so a symlink to a directory contributes no children.

use kb_core::fs_snapshot::{NodeKind, Options, Walk};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Canonicalise a path for `abs_path` / `routes.path` use. On success returns
/// the resolved absolute path; on failure (e.g. the path contains `..` or `~`
/// that cannot be resolved against the cwd) falls back to the input so the
/// caller still gets *some* stable key rather than an error.
fn canon(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Walk `root` with `std::fs` and return a populated `Walk` snapshot.
///
/// `extra_exts` become the snapshot's per-root allow-list (see `Walk::new`);
/// the *decision* to index a file on them is still made by `enumerate`.
pub fn build_walk(root: &Path, extra_exts: Vec<String>) -> io::Result<Walk> {
    let root = canon(root);
    let mut walk = Walk::new(root.to_string_lossy().to_string(), extra_exts);

    // The root itself must exist and be a directory to walk (a single-file
    // root is an edge case the CLI can register explicitly later; today the
    // root is always a directory).
    let meta = fs::symlink_metadata(&root)?;
    let root_kind = if meta.file_type().is_symlink() {
        NodeKind::Symlink
    } else if meta.is_dir() {
        NodeKind::Dir
    } else {
        NodeKind::File
    };
    walk.add_node(walk.root().to_string(), root_kind);

    if root_kind == NodeKind::Dir {
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        let root_str = walk.root().to_string();
        fn visit(
            dir: &PathBuf,
            dir_str: &str,
            walk: &mut Walk,
            seen: &mut BTreeSet<PathBuf>,
        ) -> io::Result<()> {
            if !seen.insert(dir.clone()) {
                return Ok(()); // already visited (defends against hardlinks-as-dirs)
            }
            let rd = fs::read_dir(dir)?;
            let mut child_names: Vec<String> = Vec::new();
            for entry in rd {
                let entry = entry?;
                let ft = entry.file_type()?;
                let path = entry.path();
                let name = entry.file_name();
                let name_s = name.to_string_lossy().to_string();
                let path_s = path.to_string_lossy().to_string();

                let kind = if ft.is_symlink() {
                    NodeKind::Symlink
                } else if ft.is_dir() {
                    NodeKind::Dir
                } else {
                    NodeKind::File
                };
                walk.add_node(path_s, kind);

                // The pure walker discovers a dir's children from the *name
                // list* we record here (then re-classifies each child via
                // `kind_of`). So a file's name must be in `child_names` for
                // the walker to see it — regardless of whether we recurse.
                child_names.push(name_s.clone());

                if kind == NodeKind::Dir {
                    let child_str = format!("{dir_str}/{name_s}");
                    // Recurse now (order is irrelevant to the pure walker,
                    // which re-discovers from `children` + `kinds`).
                    visit(&path, &child_str, walk, seen)?;
                }
            }
            walk.add_children(dir_str.to_string(), child_names);
            Ok(())
        }
        visit(&root, &root_str, &mut walk, &mut seen)?;
    }
    Ok(walk)
}

/// The set of file paths under this snapshot that `kb add`/`refresh` will
/// index, given the §3 walk options. Thin wrapper over the verified
/// `Walk::enumerate`.
pub fn indexed_files(walk: &Walk, include_hidden: bool) -> BTreeSet<String> {
    walk.enumerate(&Options { include_hidden })
}

/// True if `abs_path` sits at or under `root` (by canonical string prefix,
/// with a path-component boundary so `/a/bc` is not "under" `/a/b`).
pub fn is_under(root: &Path, abs_path: &Path) -> bool {
    let abs = abs_path.components();
    let root = root.components();
    let mut it = abs.peekable();
    for c in root {
        match it.next() {
            Some(a) if a == c => {}
            _ => return false,
        }
    }
    true
}

/// Choose the *winning* root for a page `abs_path` among a set of registered
/// roots. Winning rule (PLAN §4 dedupe): the **longest** root that contains
/// the path (innermost wins); ties are broken by the **highest `seq`**
/// (most-recently-registered wins). Roots that do not contain the path are
/// ignored.
///
/// Returns `(root_canonical, rel_path)`. `rel_path` is the page's path
/// relative to the chosen root (always non-empty).
pub fn winning_root(
    roots: &[(String, i64)],
    abs_path: &str,
) -> Option<(String, String)> {
    let abs = PathBuf::from(abs_path);
    // (depth, seq) — lexicographically larger wins: innermost first, then
    // most-recent on an exact depth tie.
    let mut best: Option<(usize, i64, usize)> = None;
    for (i, (root, seq)) in roots.iter().enumerate() {
        let r = PathBuf::from(root);
        if !is_under(&r, &abs) {
            continue;
        }
        let rel = match abs.strip_prefix(&r) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        if rel.is_empty() {
            continue; // the page *is* the root dir — not an indexable file
        }
        let depth = r.components().count();
        match &best {
            None => best = Some((depth, *seq, i)),
            Some((bd, bs, _)) => {
                if depth > *bd || (depth == *bd && *seq > *bs) {
                    best = Some((depth, *seq, i));
                }
            }
        }
    }
    best.map(|(_, _, i)| {
        let (root, _) = &roots[i];
        let abs = PathBuf::from(abs_path);
        let rel = abs
            .strip_prefix(root.as_str())
            .unwrap()
            .to_string_lossy()
            .to_string();
        (root.clone(), rel)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_walk_records_kinds() {
        let dir = std::env::temp_dir().join("kbdb_walktest_a");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("notes.md"), "x").unwrap();
        fs::write(dir.join(".hidden.md"), "x").unwrap();
        fs::write(dir.join("file.md"), "x").unwrap();
        fs::write(dir.join(".git/config"), "x").unwrap();

        let w = build_walk(&dir, vec![]).unwrap();
        let set = indexed_files(&w, false);
        // dot-dir contents skipped; dot-files (notes, .hidden.md) included; no-ext not present here.
        let root = w.root().to_string();
        assert!(set.contains(&format!("{root}/notes.md")));
        assert!(set.contains(&format!("{root}/.hidden.md")));
        assert!(!set.contains(&format!("{root}/.git/config")), "dot-dir content must be skipped");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn winning_root_prefers_innermost() {
        let roots = vec![
            ("/a".to_string(), 1i64),
            ("/a/b".to_string(), 2i64),
        ];
        let (r, rel) = winning_root(&roots, "/a/b/c/d.md").unwrap();
        assert_eq!(r, "/a/b");
        assert_eq!(rel, "c/d.md");
    }

    #[test]
    fn winning_root_prefers_containing_root_only() {
        let roots = vec![
            ("/a/b".to_string(), 9i64), // does NOT contain /a/x.md
            ("/a".to_string(), 1i64),   // contains it
        ];
        let (r, rel) = winning_root(&roots, "/a/x.md").unwrap();
        assert_eq!(r, "/a");
        assert_eq!(rel, "x.md");
    }
}
