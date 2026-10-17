//! fs_snapshot — the *pure* walk over an immutable **filesystem snapshot**
//! (M3). This is the *verified* half of the walk; the actual `std::fs`
//! traversal that BUILDS the snapshot is tested, not proven (see `kb-db::
//! walk_snapshot` for the builder).
//!
//! §3 (Walk / skip rules) + §9 (Verification):
//!
//! > `fs_snapshot` (pure walk over an immutable snapshot) — enumeration
//! > **sound + complete**; **no-mutation postcondition** (= "no unintended
//! > writes"); skip rules correct per §3 Walk (no symlinks followed —
//! > permanent; dot-directories skipped unless `--include-hidden`; dot-files
//! > scanned; non-indexed extensions skipped)
//!
//! ## Model
//!
//! A *snapshot* is a **finite** description of a tree: a set of nodes, each
//! carrying
//!
//! - a **path** (absolute, unique),
//! - a **kind** (dir / file / symlink),
//! - the (already-known) list of children names for a dir (the builder
//!   computed these with `std::fs::read_dir`; the pure core *walks* them).
//!
//! The pure walk `Walk::enumerate(snapshot, opts)` yields the **set of file
//! paths** the tree's `kb add` would index, given:
//!
//! - `include_hidden: bool` — whether dot-*directories* are walked (§3).
//! - `extra_exts` — non-`.md` extensions the root permits to be indexed
//!   (default empty = `.md` only, §3 Extensions). Stored *on the snapshot*
//!   (per-root `roots.extra_exts`, §8) so the walker is a pure function of
//!   the snapshot.
//! - the *permanent* symlink rule: **symlinks are never followed** (whether
//!   they are files or dir targets) and never indexed — not overridable.
//!
//! ## Rules the walk enforces (mirrored by `spec/fs_snapshot_spec.rs`)
//!
//! 1. **Symlinks are never followed and never indexed** (permanent §3).
//! 2. **Dot-directories** are skipped unless `include_hidden`.
//! 3. **Dot-files** are scanned normally (the dot rule applies only to dirs).
//! 4. **Extension allow-list:** only files whose extension is exactly `.md`
//!    or in `extra_exts` are enumerated; no-extension / outside-ext files skip.
//!
//! Verified: `spec/fs_snapshot_spec.rs` proves the four skip-decisions are
//! pairwise-exclusive, the allow-list partition is total+disjoint, and the
//! enumeration is sound + complete (a bijection with the rule-filter of the
//! snapshot files) + deterministic (no-mutation).

use std::collections::BTreeSet;

/// A node kind (§3 — the walker distinguishes file vs dir vs symlink to apply
/// the correct skip rule).
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum NodeKind {
    Dir,
    File,
    Symlink,
}

/// The per-root walk options (§3: `include_hidden`; `extra_exts` are stored on
/// the snapshot, so this is just the toggle). `Copy` so the walker stays pure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Whether dot-*directories* are walked (§3, `--include-hidden`).
    pub include_hidden: bool,
}

/// An **immutable** filesystem snapshot (the *input* to the pure walk).
///
/// Built by `kb-db::walk_snapshot` (real `std::fs` reads — tested, not
/// proven, §9) and *walked* here (pure). Fields are private: the pure walker
/// takes `&self` and never mutates the tree (the no-mutation postcondition).
pub struct Walk {
    // path -> kind
    kinds: Vec<(String, NodeKind)>,
    // dir-path -> child names
    children: Vec<(String, Vec<String>)>,
    // the root path this snapshot belongs to
    root: String,
    // per-root `extra_exts` (from the `roots` table, §8), lower-cased, no dot
    extra_exts: BTreeSet<String>,
}

impl Walk {
    /// A new, *empty* walk (no nodes). Tests / the builder populate it via the
    /// `pub(crate)` push methods.
    pub fn new(root: impl Into<String>, extra_exts: Vec<String>) -> Self {
        Walk {
            kinds: Vec::new(),
            children: Vec::new(),
            root: root.into(),
            extra_exts: extra_exts
                .into_iter()
                .map(|e| e.trim_start_matches('.').to_lowercase())
                .filter(|e| !e.is_empty())
                .collect(),
        }
    }

    /// Record a node and its kind (idempotent on path — first write wins).
    pub fn add_node(&mut self, path: String, kind: NodeKind) {
        if !self.kinds.iter().any(|(p, _)| p == &path) {
            self.kinds.push((path, kind));
        }
    }

    /// Record a dir's *children* names (order irrelevant; we walk the set).
    pub fn add_children(&mut self, dir_path: String, names: Vec<String>) {
        if self.children.iter().any(|(p, _)| p == &dir_path) {
            // replace (a fresh snapshot re-records its dir's children)
            if let Some(slot) = self.children.iter_mut().find(|(p, _)| p == &dir_path) {
                slot.1 = names;
            }
        } else {
            self.children.push((dir_path, names));
        }
    }

    /// True iff `ext` (lower-cased, no dot) is allowed: exactly `md` (the
    /// permanent default) or in the root's `extra_exts`.
    pub fn ext_allowed(&self, ext: &str) -> bool {
        ext == "md" || self.extra_exts.contains(ext)
    }

    /// The *pure* walk — return the **set** of file paths this tree would
    /// index, applying the §3 rules.
    ///
    /// # Rules (mirrored by `spec/fs_snapshot_spec.rs`)
    ///
    /// 1. **Symlinks** are never followed and never indexed (permanent §3).
    /// 2. **Dot-directories** are skipped unless `include_hidden`.
    /// 3. **Dot-files** are scanned normally (no dot-file rule).
    /// 4. **Extension allow-list:** only `md` or root `extra_exts` pass.
    ///
    /// # Postcondition
    /// - **soundness:** every emitted path is a *file* under `root` with an
    ///   allowed extension, and is not under a *skipped dot-directory*.
    /// - **completeness:** every file satisfying those rules is emitted.
    /// - **no-mutation / determinism:** `&self` + `Copy` opts ⇒ re-invocation
    ///   yields the same set (no unintended writes).
    pub fn enumerate(&self, opts: &Options) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        self.walk_dir(&self.root, opts, &mut out);
        out
    }

    /// The `root` (for the spec's "under root" soundness half).
    pub fn root(&self) -> &str {
        &self.root
    }

    /// The kind of a node if known.
    pub fn kind_of(&self, path: &str) -> Option<NodeKind> {
        self.kinds
            .iter()
            .find_map(|(p, k)| if p == path { Some(*k) } else { None })
    }

    /// The *extension* of a file *name* (lower-cased, no dot), or `None` if
    /// the name has none (no dot, or only a leading dot). `.notes.md` → `md`;
    /// `.notes` → `None`; `noext` → `None`; `file.` → `None`.
    pub fn ext_of(name: &str) -> Option<String> {
        let b = name.trim();
        if b.is_empty() {
            return None;
        }
        let last = match b.rfind('.') {
            Some(p) if p > 0 => p,
            _ => return None,
        };
        let ext: String = b[last + 1..].to_lowercase();
        if ext.is_empty() {
            None
        } else {
            Some(ext)
        }
    }

    /// The children (names) of a dir.
    fn children_of(&self, dir_path: &str) -> Option<&Vec<String>> {
        self.children
            .iter()
            .find_map(|(p, names)| if p == dir_path { Some(names) } else { None })
    }

    fn walk_dir(&self, dir_path: &str, opts: &Options, out: &mut BTreeSet<String>) {
        let names = match self.children_of(dir_path) {
            None => return, // empty dir (or not a dir) — base case
            Some(names) => names,
        };
        for name in names {
            let child = if dir_path == "/" {
                format!("/{name}")
            } else if name.is_empty() {
                dir_path.to_string()
            } else {
                format!("{dir_path}/{name}")
            };
            let kind = self.kind_of(&child).unwrap_or(NodeKind::File);

            // (A) symlink — never indexed, never followed (permanent §3).
            if kind == NodeKind::Symlink {
                continue;
            }
            match kind {
                NodeKind::Dir => {
                    // (B) dot-dir — skip unless include_hidden.
                    if !opts.include_hidden && name.starts_with('.') {
                        continue;
                    }
                    self.walk_dir(&child, opts, out);
                }
                NodeKind::File => {
                    // (C) dot-files scanned normally: the dot rule is on
                    //     *directories* only. The name's *extension* decides
                    //     indexability (rule D).
                    match Self::ext_of(name) {
                        Some(ext) if self.ext_allowed(&ext) => {
                            out.insert(child);
                        }
                        // (D) no ext, or an outside-ext → skipped.
                        _ => {}
                    }
                }
                NodeKind::Symlink => {}
            }
        }
    }
}

// =============================================================================
// Unit tests — pin exec `fs_snapshot` to the spec shape.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn twalk() -> Walk {
        let mut w = Walk::new("/t".to_string(), vec!["epub".to_string()]);
        w.add_node("/t".into(), NodeKind::Dir);
        w.add_node("/t/.git".into(), NodeKind::Dir);
        w.add_node("/t/sub".into(), NodeKind::Dir);
        w.add_node("/t/notes.md".into(), NodeKind::File);
        w.add_node("/t/symlink.md".into(), NodeKind::Symlink);
        w.add_node("/t/.notes.md".into(), NodeKind::File);
        w.add_node("/t/noext".into(), NodeKind::File);
        w.add_node("/t/file.bin".into(), NodeKind::File);
        w.add_node("/t/.git/config".into(), NodeKind::File);
        w.add_node("/t/sub/a.md".into(), NodeKind::File);
        w.add_children(
            "/t".into(),
            vec![
                "notes.md".into(),
                "symlink.md".into(),
                ".notes.md".into(),
                "noext".into(),
                "file.bin".into(),
                ".git".into(),
                "sub".into(),
            ],
        );
        w.add_children("/t/.git".into(), vec!["config".into()]);
        w.add_children("/t/sub".into(), vec!["a.md".into()]);
        w
    }

    fn expected_default() -> BTreeSet<String> {
        BTreeSet::from([
            "/t/notes.md".to_string(),
            "/t/.notes.md".to_string(),
            "/t/sub/a.md".to_string(),
        ])
    }

    #[test]
    fn default_walk_skips_symlink_dot_dir_no_ext_outside_ext() {
        let w = twalk();
        let out = w.enumerate(&Options {
            include_hidden: false,
        });
        assert_eq!(out, expected_default());
    }

    #[test]
    fn include_hidden_walks_dot_dir_but_ext_rule_still_applies() {
        let w = twalk();
        let out = w.enumerate(&Options {
            include_hidden: true,
        });
        // /.git/config has no ext → still skipped even after the dot-dir is
        // walked (the extension rule is independent of the dot-dir rule).
        assert!(!out.contains("/t/.git/config"));
        assert_eq!(out, expected_default());
        // (a `.git/` file *with* an allowed ext *would* appear:)
        let mut w2 = twalk();
        w2.add_node("/t/.git/hidden.md".into(), NodeKind::File);
        w2.add_children("/t/.git".into(), vec!["config".into(), "hidden.md".into()]);
        let out2 = w2.enumerate(&Options {
            include_hidden: true,
        });
        assert!(out2.contains("/t/.git/hidden.md"));
    }

    #[test]
    fn symlink_never_followed_even_with_include_hidden() {
        let w = twalk();
        assert!(!w
            .enumerate(&Options {
                include_hidden: false
            })
            .contains("/t/symlink.md"));
        assert!(!w
            .enumerate(&Options {
                include_hidden: true
            })
            .contains("/t/symlink.md"));
    }

    #[test]
    fn extra_exts_allow_list_adds_files() {
        let mut w = Walk::new("/t".to_string(), vec!["epub".to_string()]);
        w.add_node("/t".into(), NodeKind::Dir);
        w.add_node("/t/book.epub".into(), NodeKind::File);
        w.add_node("/t/book.pdf".into(), NodeKind::File);
        w.add_children("/t".into(), vec!["book.epub".into(), "book.pdf".into()]);
        let out = w.enumerate(&Options {
            include_hidden: false,
        });
        assert!(out.contains("/t/book.epub"));
        assert!(!out.contains("/t/book.pdf"), "pdf not in the allow-list");
    }

    #[test]
    fn empty_dir_walks_to_empty_set() {
        let mut w = Walk::new("/empty".to_string(), vec![]);
        w.add_node("/empty".into(), NodeKind::Dir);
        assert!(w
            .enumerate(&Options {
                include_hidden: false
            })
            .is_empty());
    }

    #[test]
    fn ext_of_handles_dot_names() {
        assert_eq!(Walk::ext_of("a.md"), Some("md".to_string()));
        assert_eq!(Walk::ext_of(".notes.md"), Some("md".to_string()));
        assert_eq!(Walk::ext_of(".notes"), None);
        assert_eq!(Walk::ext_of("noext"), None);
        assert_eq!(Walk::ext_of("file."), None);
        assert_eq!(Walk::ext_of("a.MD"), Some("md".to_string()));
    }

    #[test]
    fn enumeration_is_deterministic_and_read_only() {
        let w = twalk();
        let opts = Options {
            include_hidden: true,
        };
        let a = w.enumerate(&opts);
        let b = w.enumerate(&opts);
        assert_eq!(a, b, "same snapshot + same opts ⇒ same enumeration");
        // the snapshot is unchanged across calls (pure &self):
        assert_eq!(w.kind_of("/t/notes.md"), Some(NodeKind::File));
        assert_eq!(w.kind_of("/t/symlink.md"), Some(NodeKind::Symlink));
        assert!(a == b && b == w.enumerate(&opts));
    }
}
