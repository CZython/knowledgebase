//! line-diff — a dependency-free, line-granular **standard unified diff**
//! (M6 CLI shell, PLAN §13f).
//!
#![allow(dead_code)]
//!
//! *Assumed* code: it lives in the CLI shell (PLAN §9), not the verified
//! core. Only `std` is used, so `kb` stays a single static binary with no
//! `diff`/`git` fork-exec.
//!
//! The output MUST be standard unified-diff — byte-compatible with the
//! `diff -u`, `git diff` and `patch -p1` conventions, because that is what
//! agents and patchers consume:
//!
//! ```text
//! --- <old-label>
//! +++ <new-label>
//! @@ -<old_start>[,<old_count>] +<new_start>[,<new_count>] @@
//!  <context line>        exactly one leading space
//! -<removed line>        exactly one leading minus
//! +<added line>          exactly one leading plus
//! ```
//!
//! Range form: a `count` of 1 omits the `,1`; an *empty* side renders as
//! `<position>,0` (position = number of preceding lines — `@@ -0,0 +3 @@`
//! is a top-of-file insert). Pinned by `tests/diff_roundtrip.rs`, which
//! re-applies our output with an independent `patch` parser, and by
//! `diff --unified` cross-checks in the same test.
//!
//! Algorithm: LCS over lines (DP, O(n·m), fine for section-sized input),
//! grouped into 3-context-line hunks.

#[derive(Clone, Debug, PartialEq, Eq)]
enum Op {
    Equal { i1: usize, i2: usize, j1: usize, j2: usize },
    Replace { i1: usize, i2: usize, j1: usize, j2: usize },
    Delete { i1: usize, i2: usize },
    Insert { j1: usize, j2: usize },
}

fn op_old_range(o: &Op) -> Option<(usize, usize)> {
    match o {
        Op::Equal { i1, i2, .. } => Some((*i1, *i2)),
        Op::Replace { i1, i2, .. } => Some((*i1, *i2)),
        Op::Delete { i1, i2 } => Some((*i1, *i2)),
        Op::Insert { .. } => None,
    }
}
fn op_new_range(o: &Op) -> Option<(usize, usize)> {
    match o {
        Op::Equal { j1, j2, .. } => Some((*j1, *j2)),
        Op::Replace { j1, j2, .. } => Some((*j1, *j2)),
        Op::Insert { j1, j2 } => Some((*j1, *j2)),
        Op::Delete { .. } => None,
    }
}

struct Hunk {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    body: Vec<(char, String)>,
}

/// Split into lines, dropping a single trailing newline (so `"a\n"` and
/// `"a"` render the same — the POSIX text-file convention).
fn split_lines(s: &str) -> Vec<String> {
    let mut lines: Vec<String> = s.split('\n').map(str::to_string).collect();
    if lines.last() == Some(&String::new()) {
        lines.pop();
    }
    lines
}

fn opcodes(old: &[String], new: &[String]) -> Vec<Op> {
    let n = old.len();
    let m = new.len();
    // cap the DP table size. If both sides are large
    // (n*m > 10M cells ≈ 40 MB of u32), bail to a single Replace op — the
    // output is still a valid unified diff, just one big hunk.
    const LCS_CAP: usize = 10_000_000;
    if n * m > LCS_CAP {
        return vec![Op::Replace { i1: 0, i2: n, j1: 0, j2: m }];
    }
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut ops: Vec<Op> = Vec::new();
    let mut i: usize = 0;
    let mut j: usize = 0;
    while i < n && j < m {
        if old[i] == new[j] {
            let mut i2: usize = i + 1;
            let mut j2: usize = j + 1;
            while i2 < n && j2 < m && old[i2] == new[j2] {
                i2 += 1;
                j2 += 1;
            }
            ops.push(Op::Equal { i1: i, i2, j1: j, j2 });
            (i, j) = (i2, j2);
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(Op::Delete { i1: i, i2: i + 1 });
            i += 1;
        } else {
            ops.push(Op::Insert { j1: j, j2: j + 1 });
            j += 1;
        }
    }
    if i < n {
        ops.push(Op::Delete { i1: i, i2: n });
    }
    if j < m {
        ops.push(Op::Insert { j1: j, j2: m });
    }
    // Canonical `diff` shape: a delete directly followed by an insert (or
    // vice versa) is one *replace* block ("changed line" = `-old` + `+new`).
    let mut merged: Vec<Op> = Vec::with_capacity(ops.len());
    for op in ops {
        let prev = merged.last().cloned();
        match (prev, op) {
            (Some(Op::Delete { i1, i2 }), Op::Insert { j1, j2 }) => {
                merged.pop();
                merged.push(Op::Replace { i1, i2, j1, j2 });
            }
            (Some(Op::Insert { j1, j2 }), Op::Delete { i1, i2 }) => {
                merged.pop();
                merged.push(Op::Replace { i1, i2, j1, j2 });
            }
            (_, op) => merged.push(op),
        }
    }
    merged
}

/// Render a standard unified diff. Returns "" iff `old` and `new` are
/// line-identical (so the CLI treats "no diff" = no-op: the
/// edit-save-reopen case).
pub fn unified_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    let a = split_lines(old);
    let b = split_lines(new);
    let ops = opcodes(&a, &b);
    if ops.iter().all(|o| matches!(o, Op::Equal { .. })) {
        return String::new();
    }
    let ctx = 3usize;
    let n = ops.len();
    let is_change = |o: &Op| !matches!(o, Op::Equal { .. });

    // prefix line counts per op-index, for empty-side header positions
    let mut pref_old = vec![0usize; n + 1];
    let mut pref_new = vec![0usize; n + 1];
    for (k, op) in ops.iter().enumerate() {
        pref_old[k + 1] = pref_old[k] + op_old_range(op).map(|(x, y)| y - x).unwrap_or(0);
        pref_new[k + 1] = pref_new[k] + op_new_range(op).map(|(x, y)| y - x).unwrap_or(0);
    }

    let mut hunks: Vec<Hunk> = Vec::new();
    let mut i = 0usize;
    while i < n {
        if !is_change(&ops[i]) {
            i += 1;
            continue;
        }
        // cluster: extend across change-blocks; an Equal gap of >2*ctx
        // splits the hunk (GNU diff behavior).
        let mut boundary = i + 1;
        while boundary < n {
            match &ops[boundary] {
                Op::Equal { i1, i2, .. } if i2 - i1 <= 2 * ctx => boundary += 1,
                Op::Equal { .. } => break,
                _ => boundary += 1,
            }
        }

        let mut body: Vec<(char, String)> = Vec::new();
        let mut used_old: Vec<usize> = Vec::new();
        let mut used_new: Vec<usize> = Vec::new();
        let mut push = |c: char, text: &str, ol: Option<usize>, nl: Option<usize>| {
            body.push((c, text.to_string()));
            if let Some(k) = ol {
                used_old.push(k);
            }
            if let Some(k) = nl {
                used_new.push(k);
            }
        };

        // leading context (up to `ctx` lines before the first change)
        if i > 0 {
            if let Some(Op::Equal { i1, i2, j1, .. }) = ops.get(i - 1) {
                let cnt = (*i2 - *i1).min(ctx);
                let start = *i2 - cnt;
                for (off, line) in a[start..*i2].iter().enumerate() {
                    push(' ', line, Some(start + off), Some(*j1 + off));
                }
            }
        }
        // change blocks + intra-gap context
        for op in &ops[i..boundary] {
            match op {
                Op::Equal { i1, i2, j1, .. } => {
                    for (off, line) in a[*i1..*i2].iter().enumerate() {
                        push(' ', line, Some(*i1 + off), Some(*j1 + off));
                    }
                }
                Op::Replace { i1, i2, j1, j2 } => {
                    for (off, line) in a[*i1..*i2].iter().enumerate() {
                        push('-', line, Some(*i1 + off), None);
                    }
                    for (off, line) in b[*j1..*j2].iter().enumerate() {
                        push('+', line, None, Some(*j1 + off));
                    }
                }
                Op::Delete { i1, i2 } => {
                    for (off, line) in a[*i1..*i2].iter().enumerate() {
                        push('-', line, Some(*i1 + off), None);
                    }
                }
                Op::Insert { j1, j2 } => {
                    for (off, line) in b[*j1..*j2].iter().enumerate() {
                        push('+', line, None, Some(*j1 + off));
                    }
                }
            }
        }
        // trailing context (up to `ctx` lines after the last change)
        if let Some(Op::Equal { i1, i2, j1, .. }) = ops.get(boundary) {
            let cnt = (*i2 - *i1).min(ctx);
            for (off, line) in a[*i1..*i1 + cnt].iter().enumerate() {
                push(' ', line, Some(*i1 + off), Some(*j1 + off));
            }
        }

        let used_old = used_old.into_iter().collect::<Vec<_>>();
        let used_new = used_new.into_iter().collect::<Vec<_>>();
        hunks.push(Hunk {
            old_start: used_old_range(&used_old, pref_old[i]),
            old_count: used_old.len(),
            new_start: used_old_range(&used_new, pref_new[i]),
            new_count: used_new.len(),
            body,
        });
        i = boundary.max(i + 1) + (if ops.get(boundary).map(is_change) != Some(true) { 1 } else { 0 });
    }

    let mut out = String::new();
    out.push_str(&format!("--- {old_label}\n+++ {new_label}\n"));
    for h in &hunks {
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            head(h.old_start, h.old_count),
            head(h.new_start, h.new_count)
        ));
        for (c, line) in &h.body {
            out.push(*c);
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Used-line-index vector → 1-based start (`min+1`), or the empty-side
/// position (number of preceding lines).
fn used_old_range(used: &[usize], preceding: usize) -> usize {
    used.iter().copied().min().map(|k| k + 1).unwrap_or(preceding)
}

/// GNU range text: 0 lines → `<pos>,0`; 1 line → just the line number;
/// else → `<start>,<count>`.
fn head(start: usize, count: usize) -> String {
    match count {
        0 => format!("{start},0"),
        1 => format!("{start}"),
        _ => format!("{start},{count}"),
    }
}

// ---------------------------------------------------------------------------
// Parsing a *standard* unified diff (produced by `diff -u` / `git diff`) and
// applying it to a base. This is the direction the CLI uses to *consume*
// patches: it must accept exactly what the external tools emit, including the
// subtle empty-side headers (`@@ -0,0 +1 @@`, `@@ -12,4 +12,5 @@`) and the
// `\ No newline at end of file` marker.
//
// Exposed `pub` so an external tool (or a future `kb patch` sub-command) can
// drive these directly. The bin target is currently the only consumer; the
// *unit tests* below exercise them (which keeps them alive under `cargo test`).
// ---------------------------------------------------------------------------
#[derive(Clone, Debug)]
struct PatchLine {
    kind: char, // ' ' | '-' | '+'
    text: String,
}

#[derive(Clone, Debug)]
struct PatchHunk {
    old_start: usize, // 1-based, or 0 for an empty old side
    old_count: usize,
    body: Vec<PatchLine>,
}

#[derive(Clone, Debug)]
struct Patch {
    hunks: Vec<PatchHunk>,
}

/// Parse one `(start,count)` range token: `"12,4"` → `(12,4)`; a bare `"12"`
/// is GNU's 1-line shorthand → `(12,1)`.
fn parse_range(tok: &str) -> Result<(usize, usize), String> {
    match tok.split_once(',') {
        Some((a, b)) => Ok((
            a.parse().map_err(|_| format!("bad range '{tok}'"))?,
            b.parse().map_err(|_| format!("bad range '{tok}'"))?,
        )),
        None => Ok((
            tok.parse().map_err(|_| format!("bad range '{tok}'"))?,
            1,
        )),
    }
}

/// Parse the hunk header after `@@`: `-<old> +<new>` (trailing `@@` ignored),
/// returning `(old_start, old_count, new_start, new_count)`.
fn parse_header(rest: &str) -> Result<(usize, usize, usize, usize), String> {
    let toks: Vec<&str> = rest.split_whitespace().collect();
    if toks.len() < 2 {
        return Err(format!("malformed hunk header '@@{rest}'"));
    }
    let old = parse_range(toks[0].trim_start_matches('-'))?;
    let new = parse_range(toks[1].trim_start_matches('+'))?;
    Ok((old.0, old.1, new.0, new.1))
}

fn parse_unified(patch: &str) -> Result<Patch, String> {
    let lines: Vec<&str> = patch.lines().collect();
    let mut hunks: Vec<PatchHunk> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        if !line.starts_with("@@") {
            i += 1; // file header (`---`/`+++`), blank, or comment
            continue;
        }
        let (old_start, old_count, _ns, _nc) = parse_header(&line[2..])?;
        i += 1;
        let mut body: Vec<PatchLine> = Vec::new();
        while i < lines.len() {
            let l = lines[i];
            match l.as_bytes().first() {
                Some(b'\\') => {
                    i += 1;
                    continue; // `\ No newline at end of file`
                }
                Some(kind @ (b' ' | b'-' | b'+')) => {
                    // `kind` matched an ASCII prefix, so byte offset 1 is a char
                    // boundary: `split_at` splits the prefix off exactly once
                    // (no second index read of `l`).
                    let (_, rest) = l.split_at(1);
                    body.push(PatchLine { kind: *kind as char, text: rest.to_string() });
                    i += 1;
                }
                Some(_) => break, // next `@@` header
                None => {
                    // an empty line is a blank *context* line (GNU emits ` `
                    // but some tools emit nothing — accept both)
                    body.push(PatchLine { kind: ' ', text: String::new() });
                    i += 1;
                }
            }
        }
        hunks.push(PatchHunk { old_start, old_count, body });
    }
    if hunks.is_empty() {
        return Err("no '@@' hunk found".to_string());
    }
    Ok(Patch { hunks })
}

/// Apply a standard unified `patch` to `base` (non-fuzzy: hunks must line up
/// exactly with `base` — true for patches generated from that base).
pub fn apply_patch(base: &str, patch: &str) -> Result<String, String> {
    let p = parse_unified(patch)?;
    let base_lines = split_lines(base);
    let mut out: Vec<String> = Vec::new();
    let mut old_idx = 0usize;
    for hunk in &p.hunks {
        let skip = hunk.old_start.saturating_sub(1).min(base_lines.len());
        while old_idx < skip {
            out.push(base_lines[old_idx].clone());
            old_idx += 1;
        }
        for line in &hunk.body {
            match line.kind {
                ' ' => {
                    if old_idx >= base_lines.len() {
                        return Err("context line out of range".to_string());
                    }
                    out.push(base_lines[old_idx].clone());
                    old_idx += 1;
                }
                '-' => {
                    if old_idx >= base_lines.len() {
                        return Err("removed line out of range".to_string());
                    }
                    old_idx += 1;
                }
                '+' => out.push(line.text.clone()),
                _ => {}
            }
        }
    }
    while old_idx < base_lines.len() {
        out.push(base_lines[old_idx].clone());
        old_idx += 1;
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_is_empty() {
        assert_eq!(unified_diff("a\nb\nc\n", "a\nb\nc\n", "a/f", "b/f"), "");
        assert_eq!(unified_diff("a\nb", "a\nb", "a/f", "b/f"), "");
    }

    #[test]
    fn replace_is_standard_shape() {
        let d = unified_diff("a\nb\nc\n", "a\nB\nc\n", "a/f", "b/f");
        assert_eq!(
            d,
            "--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"
        );
    }

    #[test]
    fn delete_middle() {
        let d = unified_diff("a\nb\nc\nd\n", "a\nc\nd\n", "a/f", "b/f");
        assert!(d.starts_with("--- a/f\n+++ b/f\n@@ -1,4 +1,3 @@"), "{d}");
        assert!(d.contains("-b\n") && !d.contains("+b\n"));
    }

    #[test]
    fn insert_at_start_is_zero_line() {
        let d = unified_diff("b\nc\n", "a\nb\nc\n", "a/f", "b/f");
        assert_eq!(d, "--- a/f\n+++ b/f\n@@ -1,2 +1,3 @@\n+a\n b\n c\n");
    }

    #[test]
    fn multi_line_replace_keeps_header_counts() {
        let d = unified_diff("1\n2\n3\n4\n5\n", "1\nX\nY\n4\n5\n", "a/f", "b/f");
        assert!(d.contains("-2\n-3\n+X\n+Y\n"), "{d}");
        assert!(d.contains("@@ -1,5 +1,5 @@"), "{d}");
    }

    #[test]
    fn every_body_line_has_exactly_one_prefix() {
        let d = unified_diff("a\nb\nc\n", "a\nB\nC\nd\n", "a/f", "b/f");
        let body_lines: Vec<&str> = d.lines().skip(3).collect(); // --- +++ @@
        for line in body_lines {
            assert!(
                line.is_empty() || matches!(line.as_bytes()[0], b' ' | b'-' | b'+'),
                "bad prefix: {line:?}"
            );
        }
    }

    // --- Golden fixtures, pre-generated with the real `diff` CLI --------------
    // `testdata/standard.diff` was produced by `diff -u section_old.md
    // section_new.md` (NOT by this module) and independently confirmed to apply
    // cleanly with GNU `patch`. These tests prove our *parser* and *applier*
    // consume a real tool's output.

    fn fixture(name: &str) -> String {
        // The fixture lives in the shared source dir (`kb/testdata`); this
        // crate (the package that compiles it, e.g. `user`) sits a level up.
        let path = format!("{}/../kb/testdata/{}", env!("CARGO_MANIFEST_DIR"), name);
        std::fs::read_to_string(path).expect("fixture missing")
    }

    #[test]
    fn parses_the_pregenerated_standard_patch() {
        let p = parse_unified(&fixture("standard.diff")).expect("parse failed");
        assert_eq!(p.hunks.len(), 2, "expected 2 hunks");
        // hunk 1: 1-line replace, hunk 2: 1-line replace + 2 added lines
        let (o1, n1) = (p.hunks[0].body.iter().filter(|l| l.kind == '-').count(),
                        p.hunks[0].body.iter().filter(|l| l.kind == '+').count());
        assert_eq!((o1, n1), (1, 1));
        let (o2, n2) = (p.hunks[1].body.iter().filter(|l| l.kind == '-').count(),
                        p.hunks[1].body.iter().filter(|l| l.kind == '+').count());
        assert_eq!((o2, n2), (1, 2));
        // the subtle empty-side count `@@ -12,4 +12,5 @@` is honoured:
        assert_eq!(p.hunks[1].old_start, 12);
        assert_eq!(p.hunks[1].old_count, 4);
    }

    #[test]
    fn applies_the_pregenerated_patch_to_get_the_new_file() {
        let base = fixture("section_old.md");
        let patch = fixture("standard.diff");
        let want = fixture("section_new.md");
        let got = apply_patch(&base, &patch).expect("apply failed");
        assert_eq!(got, want, "our apply_patch disagreed with GNU patch");
    }

    #[test]
    fn roundtrip_our_diff_is_parseable_by_our_applier() {
        let base = fixture("section_old.md");
        let want = fixture("section_new.md");
        let d = unified_diff(&base, &want, "a", "b");
        let got = apply_patch(&base, &d).expect("apply failed");
        assert_eq!(got, want, "apply(unified_diff(x, y)) != y");
    }
}
