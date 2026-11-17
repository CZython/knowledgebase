//! section — ATX-heading parse + byte-range splice (M4, port of `mdtool.py`).
//!
//! Pure, no IO: the whole document lives in a `Vec<u8>` buffer. Every read /
//! edit is a byte-range query over that buffer, so the *shape* matches
//! `mdtool.py` (byte offsets in/out, O(1) splices) while staying in the
//! "no `std::io`" zone this crate forbids.

/// A parsed ATX section (one heading + its span). Offsets are byte offsets
/// into the owning `Doc` buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// Heading level, 1..=6.
    pub level: u8,
    /// The heading title text (trimmed).
    pub title: String,
    /// Byte offset of the heading line (inclusive).
    pub start: usize,
    /// Byte offset where the *full* section ends (exclusive): up to the next
    /// heading of level <= own.
    pub end: usize,
    /// Byte offset where the section's *own body* ends (exclusive): the very
    /// next heading of any level.
    pub own_end: usize,
    /// 0-based line index of the heading line (inclusive).
    pub start_line: usize,
    /// 0-based line index of the full section's end (exclusive); i.e. the
    /// line that the full span runs through. Line count = `end_line - start_line`.
    pub end_line: usize,
    /// 0-based line index of the own body's end (exclusive).
    pub own_end_line: usize,
    /// Dotted document-order number, e.g. `1.2`.
    pub ref_: String,
}

/// A row of the `sections` index (section-search) — a page's
/// *section* (incl. the `'*'` page-level preamble row) with its text
/// resolved, as exported by `Doc::export_rows`.
///
/// The `ref` field is the **canonical** form only: the dotted number
/// (`1`, `1.2`) or the `'*'` sentinel. A title path is a *resolution
/// input* (`Doc::resolve`) that maps to the canonical ref — it is
/// never stored. The position of a row in the export `Vec` (`'*'`
/// first, headings in document order) is its `ordinal` column value —
/// the sort key for candidate lists within a page (so `1.2` precedes
/// `1.10`), not the dotted numeric value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionSpan {
    /// Heading level: `0` = the `'*'` page-level (preamble) row;
    /// `1..=6` = an ATX heading.
    pub level: u8,
    /// Canonical ref: dotted number or `'*'`.
    pub ref_: String,
    /// Heading text (trimmed); page level: the page's display title.
    pub title: String,
    /// Section's *own body* (heading through first subsection) — the
    /// hover return. `'*'` row: the preamble.
    pub own_body: String,
    /// Full span incl. subsections, `None` when it equals `own_body`
    /// (no children).
    pub full_body: Option<String>,
    /// 0-based line index of the own-body's end, exclusive.
    pub own_end_line: usize,
}

/// Where a new section is inserted relative to a target (or a file edge).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionPlacement {
    Before,
    After,
    Child,
    Top,
    End,
}

/// Errors returned by section operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionError {
    EmptyRef,
    /// A dotted number / title-path was supplied but matches no section.
    NoSection {
        ref_: String,
    },
    /// Two or more sections match — list them so the caller can disambiguate.
    Ambiguous {
        ref_: String,
        candidates: Vec<String>,
    },
    /// `replace_in` was called with a scope but the literal was not found
    /// (mirrors `mdtool.py`'s "nothing to replace").
    NotFound {
        literal: String,
        scope: String,
    },
    /// a `Child` insert under a level-6 target would
    /// need level 7, which isn't a valid heading level — the old `base.min(6)`
    /// silently clamped it to a *sibling* of the parent instead of erroring.
    ChildOfLeaf {
        ref_: String,
    },
}

impl std::fmt::Display for SectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SectionError::EmptyRef => write!(f, "empty section reference"),
            SectionError::NoSection { ref_ } => write!(f, "no section matching {ref_:?}"),
            SectionError::Ambiguous { ref_, candidates } => {
                write!(f, "ambiguous reference {ref_:?}: {}", candidates.join(", "))
            }
            SectionError::NotFound { literal, scope } => {
                write!(f, "nothing to replace: {literal:?} not found ({scope})")
            }
            SectionError::ChildOfLeaf { ref_ } => {
                write!(f, "cannot insert a child of {ref_:?}: it is already at the maximum heading level (6)")
            }
        }
    }
}

impl std::error::Error for SectionError {}

/// The parsed document: the byte buffer plus the heading index derived from
/// it. All accessors are pure reads over `data`.
#[derive(Clone, Debug)]
pub struct Doc {
    /// The raw UTF-8 document bytes.
    pub data: Vec<u8>,
    /// Headings in document order (skipping fenced code blocks).
    pub sections: Vec<Section>,
    /// Byte offset of the first heading — the byte the leading preamble ends
    /// at. `0` when the document has no headings (there is nothing to
    /// precede, so `Top` placement inserts at the very start of the file).
    pub preamble_end: usize,
    /// Total line count (newline-terminated lines; an unterminated trailing
    /// line counts as one line).
    pub total_lines: usize,
}

impl Doc {
    /// Parse `data` into a `Doc` (one scan pass, O(n)).
    ///
    /// Mirrors `mdtool.py::_scan` (fence-aware heading detection), `_apply`
    /// (span + own_end assignment) and the dotted-ref pass, in one function
    /// that is a pure function of `data` (no IO — the caller supplies the
    /// already-read bytes).
    pub fn new(data: Vec<u8>) -> Self {
        let n = data.len();
        if n == 0 {
            return Doc {
                data,
                sections: Vec::new(),
                preamble_end: 0,
                total_lines: 0,
            };
        }

        // ---- scan: fence-aware heading detection --------------------------
        let mut found: Vec<(u8, String, usize)> = Vec::new(); // (level, title, start)
        let mut line_starts: Vec<usize> = Vec::new();
        let mut in_fence = false;
        let mut fence_char: u8 = 0;
        let mut fence_len = 0usize;
        let mut pos = 0;
        while pos < n {
            line_starts.push(pos);
            let nl = match data[pos..].iter().position(|&b| b == b'\n') {
                Some(off) => pos + off,
                None => n,
            };
            let line = &data[pos..nl];
            match fence_of(line) {
                Some((c, len)) => {
                    if !in_fence {
                        in_fence = true;
                        fence_char = c;
                        fence_len = len;
                    } else if c == fence_char && len >= fence_len {
                        in_fence = false;
                    }
                }
                None => {
                    if !in_fence {
                        if let Some((level, title)) = heading_of(line) {
                            found.push((level, title, pos));
                        }
                    }
                }
            }
            pos = nl + 1;
        }

        let total_lines = line_starts.len();
        let preamble_end = found.first().map(|s| s.2).unwrap_or(0);

        // ---- spans + own_end ---------------------------------------------
        let mut sections: Vec<Section> = Vec::with_capacity(found.len());
        for (i, &(level, ref title, start)) in found.iter().enumerate() {
            let mut end = n;
            for nxt in found.iter().skip(i + 1) {
                if nxt.0 <= level {
                    end = nxt.2;
                    break;
                }
            }
            let own_end = if i + 1 < found.len() {
                found[i + 1].2
            } else {
                n
            };
            sections.push(Section {
                level,
                title: title.clone(),
                start,
                end,
                own_end,
                start_line: bisect_left(&line_starts, start),
                end_line: bisect_left(&line_starts, end),
                own_end_line: bisect_left(&line_starts, own_end),
                ref_: String::new(),
            });
        }

        // ---- dotted document-order refs ----------------------------------
        let mut counters = [0u32; 6];
        for s in &mut sections {
            let lvl = s.level as usize;
            counters[lvl - 1] += 1;
            for c in &mut counters[lvl..] {
                *c = 0;
            }
            let mut parts: Vec<String> = counters[..lvl].iter().map(|c| c.to_string()).collect();
            while parts.len() > 1 && parts[0] == "0" {
                parts.remove(0);
            }
            s.ref_ = parts.join(".");
        }

        Doc {
            data,
            sections,
            preamble_end,
            total_lines,
        }
    }

    /// The byte range `[start, end)` of the named section's **own body**.
    pub fn own_body(&self, ref_: &str) -> Result<std::ops::Range<usize>, SectionError> {
        let i = self.resolve(ref_)?;
        if i == usize::MAX {
            return Ok(self.page_span());
        }
        let s = &self.sections[i];
        Ok(s.start..s.own_end)
    }

    /// The byte range `[start, end)` of the named section's **full span**
    /// (including subsections).
    pub fn full_span(&self, ref_: &str) -> Result<std::ops::Range<usize>, SectionError> {
        let i = self.resolve(ref_)?;
        if i == usize::MAX {
            return Ok(self.page_span());
        }
        let s = &self.sections[i];
        Ok(s.start..s.end)
    }

    /// The page-level (`'*'`) byte range: the *preamble* — `data[0..
    /// preamble_end]`, or the whole body when the page has no headings
    /// (the `preamble_end == 0 && sections.is_empty()` disambiguator).
    pub fn page_span(&self) -> std::ops::Range<usize> {
        let n = self.data.len();
        let hi = if self.preamble_end == 0 && self.sections.is_empty() {
            n
        } else {
            self.preamble_end
        };
        0..hi
    }

    /// The raw byte slice of the named section's own body (the convenience
    /// read used by the CLI's `section` command).
    pub fn own_bytes(&self, ref_: &str) -> Result<&[u8], SectionError> {
        let r = self.own_body(ref_)?;
        Ok(&self.data[r])
    }

    /// The raw byte slice of the named section's full span.
    pub fn full_bytes(&self, ref_: &str) -> Result<&[u8], SectionError> {
        let r = self.full_span(ref_)?;
        Ok(&self.data[r])
    }

    /// The **page-level row** the section table indexes: the reserved
    /// `ref` sentinel for "the page itself (its preamble)". Pinned to the
    /// one character `'*'` so it can never collide with a dotted number.
    pub const PAGE_ROW_REF: &'static str = "*";

    /// Export the page's rows for the `sections` index (the
    /// section-search build): one **page-level row** (the `'*'` sentinel,
    /// `level 0`, the page's display title, `own_body` = the *preamble*)
    /// followed by one row per heading section in document order.
    ///
    /// The *preamble* is `data[0..preamble_end]` — the text before the
    /// first heading. `preamble_end == 0` is ambiguous between "no
    /// headings" (⇒ the *whole* body is the preamble) and "the page opens
    /// with a heading" (⇒ empty preamble); the disambiguator is
    /// `sections.is_empty()`. A page that opens with a heading therefore
    /// gets an *empty* `'*'` own-body — its H1 is already indexed as its
    /// own row, so nothing is lost (the `'*'` row covers the intro span
    /// that belongs to no section).
    ///
    /// `full_body` is the section's full span (incl. subsections), equal
    /// to `own_body` when the section has no children — stored as `None`
    /// in that case to save the duplicate bytes (a reader treats
    /// `None` as "same as `own_body`"). `own_end_line` is 0-based
    /// (`total_lines` is the exclusive count); the `'*'` row points at the
    /// last preamble line, or the last body line for a heading-less page.
    /// The caller's index over the returned `Vec` is the `ordinal` column
    /// value (`'*'` = 0, first heading = 1, …) — document order, not the
    /// dotted number.
    pub fn export_rows(&self, page_title: &str) -> Vec<SectionSpan> {
        let n = self.data.len();
        let pre_end = if self.preamble_end == 0 && self.sections.is_empty() {
            n
        } else {
            self.preamble_end
        };
        let mut rows = Vec::with_capacity(self.sections.len() + 1);
        rows.push(SectionSpan {
            level: 0,
            ref_: Self::PAGE_ROW_REF.to_string(),
            title: page_title.to_string(),
            own_body: String::from_utf8_lossy(&self.data[..pre_end]).into_owned(),
            full_body: None,
            own_end_line: if self.sections.is_empty() {
                self.total_lines
            } else {
                self.sections[0].start_line.saturating_sub(1)
            },
        });
        for s in &self.sections {
            rows.push(SectionSpan {
                level: s.level,
                ref_: s.ref_.clone(),
                title: s.title.clone(),
                own_body: String::from_utf8_lossy(&self.data[s.start..s.own_end]).into_owned(),
                full_body: if s.end > s.own_end {
                    Some(String::from_utf8_lossy(&self.data[s.start..s.end]).into_owned())
                } else {
                    None
                },
                own_end_line: s.own_end_line,
            });
        }
        rows
    }

    // ---- editing (pure: each returns the *new* full buffer; the CLI
    //      persists it atomically at M6) --------------------------------

    /// Find / replace within a scope (whole file when `scope` is `None`, else
    /// the named section's full span). Returns `(new_buffer, n)` where `n` is
    /// the number of non-overlapping replacements made. `count == 0` means
    /// "replace all". Errors if nothing matched (mirrors
    /// `mdtool.py::replace` — "nothing to replace").
    pub fn replace_in(
        &self,
        scope: Option<&str>,
        find: &str,
        replacement: &str,
        count: usize,
    ) -> Result<(Vec<u8>, usize), SectionError> {
        let (lo, hi) = match scope {
            None => (0, self.data.len()),
            Some(s) => {
                let i = self.resolve(s)?;
                let sec = &self.sections[i];
                (sec.start, sec.end)
            }
        };
        let chunk = &self.data[lo..hi];
        let (new_chunk, n) =
            replace_substring(chunk, find.as_bytes(), replacement.as_bytes(), count);
        if n == 0 {
            return Err(SectionError::NotFound {
                literal: find.to_string(),
                scope: scope
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "whole file".to_string()),
            });
        }
        Ok((splice(&self.data, lo, hi, &new_chunk), n))
    }

    /// Replace a section's content with `text`. By default the whole section
    /// (heading + body) is replaced; with `keep_heading` the original heading
    /// line is retained and only the body replaced (a blank line is inserted
    /// after the heading). Returns the new full buffer.
    pub fn set_section(
        &self,
        ref_: &str,
        text: &str,
        keep_heading: bool,
    ) -> Result<Vec<u8>, SectionError> {
        let i = self.resolve(ref_)?;
        let sec = &self.sections[i];
        let mut body = ensure_trailing_newline(text.as_bytes().to_vec());
        if keep_heading {
            // retain the heading line, then a blank line, then the new body
            // (mirrors mdtool.py::set_section: head + b"\n\n" + body)
            let head_end = self.heading_line_end(sec.start);
            let head = self.data[sec.start..head_end].to_vec();
            let mut kept = head;
            kept.push(b'\n');
            kept.extend_from_slice(&body);
            body = kept;
        }
        Ok(splice(&self.data, sec.start, sec.end, &body))
    }

    /// Byte offset one past the end of the heading line starting at `start`
    /// (i.e. through the newline), or `self.data.len()` if no newline.
    fn heading_line_end(&self, start: usize) -> usize {
        let rest = &self.data[start..];
        match rest.iter().position(|&b| b == b'\n') {
            Some(off) => start + off + 1,
            None => self.data.len(),
        }
    }

    /// Append `text` at the end of a section's *full span* (before the next
    /// sibling/parent heading). Whitespace at either boundary of `text` is
    /// stripped; the section boundary is then re-established in a
    /// predictable shape: exactly one blank line between the old content and
    /// the appended text, and — when a next sibling heading follows — exactly
    /// one blank line between the appended text and that heading. At
    /// end-of-file the appended text is simply newline-terminated.
    /// Returns the new full buffer.
    pub fn append_section(&self, ref_: &str, text: &str) -> Result<Vec<u8>, SectionError> {
        let i = self.resolve(ref_)?;
        let sec = &self.sections[i];
        let ins = sec.end;
        let n = self.data.len();
        let has_next = ins < n;
        // Strip boundary whitespace from the appended text; the section
        // boundary is established below, independently, so the text's own
        // leading/trailing newlines can't collide with the old or next
        // section's whitespace.
        let body: Vec<u8> = text
            .trim_start_matches('\n')
            .trim_end_matches('\n')
            .as_bytes()
            .to_vec();
        // Leading side: if the old content does not already end in a blank
        // line, annex one so the append is separated from the old content.
        // (gap ≥ 2 means the old content already has a trailing blank — keep
        // it as the old content's, not the append's.)
        let before = &self.data[..ins];
        let mut gap = 0;
        while gap < before.len() && before[before.len() - 1 - gap] == b'\n' {
            gap += 1;
        }
        let mut out: Vec<u8> = Vec::new();
        if gap <= 1 {
            out.push(b'\n');
        }
        out.extend_from_slice(&body);
        // Trailing side: newline-terminate, and — when a next sibling heading
        // (or the document's trailing content) follows — a blank line before
        // it. At end-of-file the trailing newline is the terminator.
        out.push(b'\n');
        if has_next {
            out.push(b'\n');
        }
        Ok(splice(&self.data, ins, ins, &out))
    }

    /// Remove a section — its heading + body + all subsections (the section's
    /// **full span**, exactly the span `set_section` replaces and `full_span`
    /// reports). This is the inverse of the `splice(data, start, end, &body)`
    /// call the other section ops make, with an empty body, so it composes
    /// cleanly: remove the whole `1.2` subtree, or the whole `1` subtree
    /// including `1.2`.
    ///
    /// Splice hygiene: a section's span includes its trailing separator
    /// (the blank line before the next section). When removing the file's
    /// last section that separator is a dangling blank at EOF, so we trim
    /// trailing newlines and re-annex a single one — the
    /// `ensure_trailing_newline` convention shared by every other section op
    /// (`set`, `append`, `edit`). Mid-file removals are unaffected: the next
    /// section's bytes (starting with a heading) follow the splice point and
    /// already carry the correct separator.
    ///
    /// Returns the new full buffer (pure — `data` is not mutated).
    pub fn remove_section(&self, ref_: &str) -> Result<Vec<u8>, SectionError> {
        let i = self.resolve(ref_)?;
        let s = &self.sections[i];
        let n = self.data.len();
        let start = s.start.min(n);
        let span_end = s.end.min(n);
        let mut out = splice(&self.data, start, span_end, &[]);
        // Trim dangling trailing newlines (a removed last section leaves its
        // separating blank line orphaned at EOF), then re-annex one `\n` so
        // the file stays newline-terminated (the `ensure_trailing_newline`
        // convention every other section op follows).
        while out.ends_with(b"\n") {
            out.pop();
        }
        if !out.is_empty() {
            out.push(b'\n');
        }
        Ok(out)
    }

    /// Insert a new section (heading + body) at a placement. Returns
    /// `(insertion_byte_offset, chunk)`. When `do_splice` is true the caller
    /// can rebuild via `splice` (or compare the chunk for a dry-run).
    ///
    /// `placement`: `before` | `after` | `child` | `top` | `end`.
    /// `title` is required; `level` defaults per placement
    /// (child = target+1, else target level, else 1), clamped to 1..=6.
    pub fn insert_section(
        &self,
        placement: SectionPlacement,
        ref_: Option<&str>,
        text: &str,
        title: &str,
        level: Option<u8>,
    ) -> Result<(usize, Vec<u8>), SectionError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(SectionError::EmptyRef);
        }
        let target: Option<usize> = match ref_ {
            Some(r) => Some(self.resolve(r)?),
            None => None,
        };
        let lvl = match level {
            Some(l) => l,
            None => {
                let base = match (placement, target) {
                    (SectionPlacement::Child, Some(i)) => {
                        // a Child under a level-6 target
                        // would need level 7. The old `base.min(6)` clamped it
                        // back to 6 — silently inserting a *sibling* of the
                        // parent. Error instead.
                        if self.sections[i].level >= 6 {
                            return Err(SectionError::ChildOfLeaf {
                                ref_: self.sections[i].title.clone(),
                            });
                        }
                        self.sections[i].level + 1
                    }
                    (_, Some(i)) => self.sections[i].level,
                    (_, None) => 1,
                };
                base.min(6)
            }
        };
        if !(1..=6).contains(&lvl) {
            return Err(SectionError::NoSection {
                ref_: format!("level {lvl}"),
            });
        }
        let ins = match (placement, target) {
            (SectionPlacement::End, _) => self.data.len(),
            (SectionPlacement::Top, _) => self.preamble_end,
            (_, Some(i)) if placement == SectionPlacement::Before => self.sections[i].start,
            (_, Some(i)) => self.sections[i].end,
            _ => {
                return Err(SectionError::NoSection {
                    ref_: format!("placement {placement:?} needs a ref"),
                })
            }
        };
        let mut block = Vec::new();
        block.extend_from_slice("#".repeat(lvl as usize).as_bytes());
        block.push(b' ');
        block.extend_from_slice(title.as_bytes());
        let trimmed = text.trim_start_matches('\n').trim_end_matches('\n');
        if !trimmed.is_empty() {
            block.push(b'\n');
            block.push(b'\n');
            block.extend_from_slice(trimmed.as_bytes());
        }
        block.push(b'\n');
        // one blank line on each side (unless at a file edge)
        let before = &self.data[..ins];
        let after = &self.data[ins..];
        let prefix = if before.is_empty() {
            vec![]
        } else {
            let mut gap = 0;
            while gap < before.len() && before[before.len() - 1 - gap] == b'\n' {
                gap += 1;
            }
            match gap {
                0 => vec![b'\n', b'\n'],
                1 => vec![b'\n'],
                _ => vec![],
            }
        };
        let suffix = if after.is_empty() || after.starts_with(b"\n\n") {
            vec![]
        } else if after.starts_with(b"\n") {
            vec![b'\n']
        } else {
            vec![b'\n', b'\n']
        };
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&prefix);
        chunk.extend_from_slice(&block);
        chunk.extend_from_slice(&suffix);
        Ok((ins, chunk))
    }

    /// Resolve `ref_` to a *section index* into `self.sections`.
    ///
    /// Resolution order (mirrors `mdtool.py::resolve`):
    ///
    /// 1. **Dotted document-order number** (e.g. `1.2`, `1.2.3`, `2`):
    ///    match by the assigned `Section::ref_`.
    /// 2. **`A > B > C` title path** (parts separated by ` > `): match a
    ///    *top-level* (level-1) section whose title (case-insensitively)
    ///    equals `A`, then cascade through `B`, `C` at increasing expected
    ///    levels. A unique cascade wins; an empty cascade means "fall
    ///    through".
    /// 3. **Unique exact title match anywhere** (case-insensitive, last-
    ///    position only).
    /// 4. **Unique substring title match anywhere** (case-insensitive, last-
    ///    position only).
    ///
    /// If any of the stages finds >1 candidate, `Ambiguous` is returned with
    /// the candidate list (in `ref + " " + title` form, deduped and sorted)
    /// so the caller can surface "did you mean: …". If no stage finds a
    /// candidate, `NoSection`. An empty `ref_` is `EmptyRef`.
    pub fn resolve(&self, ref_: &str) -> Result<usize, SectionError> {
        let ref_ = ref_.trim();
        if ref_.is_empty() {
            return Err(SectionError::EmptyRef);
        }

        // ---- stage 0: the `'*'` page-level row ----------------------
        // The `'*'` sentinel names the page-level (preamble) span, which is
        // *not* in `self.sections` (a heading list). It resolves to a
        // sentinel that `own_bytes` / `full_bytes` special-case to the
        // preamble byte range. The disambiguator for an empty preamble is
        // the same as `export_rows`: `preamble_end == 0 && sections` empty
        // ⇒ the whole body.
        if ref_ == Self::PAGE_ROW_REF {
            return Ok(usize::MAX);
        }

        // ---- stage 1: dotted number --------------------------------
        if is_dotted(ref_) {
            for (i, s) in self.sections.iter().enumerate() {
                if s.ref_ == ref_ {
                    return Ok(i);
                }
            }
            return Err(SectionError::NoSection {
                ref_: ref_.to_string(),
            });
        }

        // ---- stage 2: `A > B > C` title path ------------------------
        let parts: Vec<&str> = ref_.split(" > ").map(|p| p.trim()).collect();
        let parts: Vec<&str> = parts.into_iter().filter(|p| !p.is_empty()).collect();
        let mut cands: Vec<usize> = (0..self.sections.len())
            .filter(|&i| {
                self.sections[i].level == 1
                    && self.sections[i].title.to_lowercase() == parts[0].to_lowercase()
            })
            .collect();
        // cascade: for stage k (1-based), the expected level is k+1
        for (k, part) in parts[1..].iter().enumerate() {
            let expected_level = (k as u8) + 2;
            let want = part.to_lowercase();
            cands.retain(|&i| {
                self.sections[i].level == expected_level
                    && self.sections[i].title.to_lowercase() == want
            });
            if cands.is_empty() {
                break;
            }
        }
        if cands.len() == 1 {
            return Ok(cands[0]);
        }

        // ---- stage 3: unique exact title match anywhere -------------
        let last = parts.last().cloned().unwrap_or(ref_).to_lowercase();
        let exact: Vec<usize> = (0..self.sections.len())
            .filter(|&i| self.sections[i].title.to_lowercase() == last)
            .collect();
        if exact.len() == 1 {
            return Ok(exact[0]);
        }

        // ---- stage 4: unique substring title match ------------------
        let subs: Vec<usize> = (0..self.sections.len())
            .filter(|&i| self.sections[i].title.to_lowercase().contains(&last))
            .collect();
        if subs.len() == 1 {
            return Ok(subs[0]);
        }

        // ---- error reporting ---------------------------------------
        if cands.is_empty() && exact.is_empty() && subs.is_empty() {
            return Err(SectionError::NoSection {
                ref_: ref_.to_string(),
            });
        }
        let src: &Vec<usize> = if !cands.is_empty() {
            &cands
        } else if !exact.is_empty() {
            &exact
        } else {
            &subs
        };
        let mut names: Vec<String> = src
            .iter()
            .map(|&i| self.sections[i].ref_.clone() + " " + self.sections[i].title.as_str())
            .collect();
        names.sort();
        names.dedup();
        Err(SectionError::Ambiguous {
            ref_: ref_.to_string(),
            candidates: names,
        })
    }
}

/// Resolve a section reference to the **0-based line index of its heading**
/// (T7). Pure over a body `&str` — no DB, no IO. Delegates identity to the
/// same 4-stage `Doc::resolve` cascade (dotted number → `A > B > C` title
/// path → unique exact title → unique substring), then maps the winning
/// section to its heading line (`Section::start_line`).
///
/// Return contract:
/// - `Ok(Some(line))` — a unique section matched; `line` is the 0-based index
///   of the heading line (a page that is itself a single H1 with no further
///   structure still resolves, since the H1 is section `1`).
/// - `Ok(None)` — no section matched (`SectionError::NoSection` /
///   `EmptyRef` / `NotFound`). The body is empty or the ref names nothing.
/// - `Err(SectionError::Ambiguous { .. })` — 2+ sections matched the same
///   ref (the caller's signal to disambiguate, not to guess).
///
/// This is the primitive T8 (the resolver) and T9 (the TUI reader) both
/// wire into: a page that *resolves* but whose `#section` is missing maps
/// here to `Ok(None)` → **ambiguous**; a section link maps to `Ok(Some(..))`
/// → open / recenter at that heading line.
pub fn section_lookup(body: &str, ref_: &str) -> Result<Option<usize>, SectionError> {
    let doc = Doc::new(body.as_bytes().to_vec());
    match doc.resolve(ref_) {
        Ok(i) => Ok(Some(doc.sections[i].start_line)),
        Err(SectionError::Ambiguous { ref_, candidates }) => {
            Err(SectionError::Ambiguous { ref_, candidates })
        }
        // `NoSection` / `EmptyRef` / `NotFound` / `ChildOfLeaf` all mean "a
        // unique, non-ambiguous match was not found" — surface as `None` so a
        // caller that only jumps on a real hit can `if let Some(line) = …`.
        Err(
            SectionError::NoSection { .. }
            | SectionError::EmptyRef
            | SectionError::NotFound { .. }
            | SectionError::ChildOfLeaf { .. },
        ) => Ok(None),
    }
}

/// `section_lookup` convenience for callers that don't care *why* nothing
/// matched: ambiguity is folded into `None` (only an `Err` is surfaced when
/// the lookup itself is structurally broken). `T9` (a recenter hint) and
/// most read paths can ignore the `Ambiguous` detail and treat it as
/// "don't jump".
pub fn section_lookup_line(body: &str, ref_: &str) -> Option<usize> {
    // `unwrap_or_default()` folds every `Err(Ambiguous)` to `None` (the caller
    // only jumps on a real hit, so the ambiguity detail is irrelevant here).
    section_lookup(body, ref_).unwrap_or_default()
}

/// Splice `new` into `data` over the byte range `[lo, hi)`, returning the
/// new buffer. Pure: does not mutate `data` (the caller can compare / retry).
/// Invariant: `lo <= hi <= data.len()`.
pub fn splice(data: &[u8], lo: usize, hi: usize, new: &[u8]) -> Vec<u8> {
    debug_assert!(lo <= hi && hi <= data.len(), "splice range out of bounds");
    // `saturating_sub` so a violated invariant (e.g.
    // `hi - lo > data.len() + new.len()`) can never underflow the capacity
    // to a usize near 0 in release, where `debug_assert!` is gone.
    let cap = data
        .len()
        .saturating_add(new.len())
        .saturating_sub(hi.saturating_sub(lo));
    let mut out = Vec::with_capacity(cap);
    out.extend_from_slice(&data[..lo]);
    out.extend_from_slice(new);
    out.extend_from_slice(&data[hi..]);
    out
}

/// Replace non-overlapping occurrences of `find` with `replacement` inside
/// `data`, left-to-right. `count == 0` means "replace all". Returns
/// `(new_data, n)`. Pure (does not mutate `data`).
fn replace_substring(
    data: &[u8],
    find: &[u8],
    replacement: &[u8],
    count: usize,
) -> (Vec<u8>, usize) {
    if find.is_empty() {
        return (data.to_vec(), 0);
    }
    let mut out = Vec::with_capacity(data.len());
    let mut n = 0usize;
    let mut i = 0usize;
    while i < data.len() && !(count != 0 && n >= count) {
        if data[i..].starts_with(find) {
            out.extend_from_slice(replacement);
            n += 1;
            i += find.len();
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    // any tail we didn't process (either count limit hit, or loop ran out)
    if i < data.len() {
        out.extend_from_slice(&data[i..]);
    }
    (out, n)
}

/// Ensure a byte buffer ends with `\n` (append one if it doesn't). Mirrors
/// `mdtool.py::_ensure_newline`.
fn ensure_trailing_newline(data: Vec<u8>) -> Vec<u8> {
    let mut out = data;
    if out.last() != Some(&b'\n') {
        out.push(b'\n');
    }
    out
}

/// True if `ref_` is a dotted document-order number: one or more runs of
/// digits separated by dots, e.g. `1`, `1.2`, `1.2.3`. Empty / non-digit
/// parts reject (mirrors `mdtool.py`'s `re.fullmatch(r"(?:\d+\.)*\d+", ref)`).
fn is_dotted(ref_: &str) -> bool {
    if ref_.is_empty() {
        return false;
    }
    let mut last_was_digit = false;
    for c in ref_.chars() {
        if c.is_ascii_digit() {
            last_was_digit = true;
        } else if c == '.' && last_was_digit {
            last_was_digit = false; // a dot is OK only after a digit, and
                                    // must be followed by another digit (loop continues)
        } else {
            return false;
        }
    }
    last_was_digit
}

/// Binary search: the first index `i` of `v` (sorted ascending) with `v[i] >=
/// needle`, or `v.len()` if none. Mirrors `bisect.bisect_left`. Used to map
/// a byte offset to a line index.
fn bisect_left(v: &[usize], needle: usize) -> usize {
    let mut lo = 0;
    let mut hi = v.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if v[mid] < needle {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Parse a single line as an ATX heading (level 1..=6), if it matches.
/// Mirrors `mdtool.py::HEADING_RE` (`^ {0,3}(#{1,6})(?:[ \t]+(.*?))?[ \t]*$`):
/// up to three leading spaces, 1–6 `#`, then either a blank/title, with
/// trailing whitespace trimmed off the title. Returns `(level, title)`.
///
/// A `#` glued to text (`#Title`) is NOT a heading (CommonMark requires the
/// space or an end-of-line after the `#` run). A 7+ run (`#######`) is NOT a
/// heading (ATX max is 6).
pub fn heading_of(line: &[u8]) -> Option<(u8, String)> {
    let mut i = 0;
    // tolerate up to three leading spaces
    while i < line.len() && line[i] == b' ' && i < 3 {
        i += 1;
    }
    // count the `#` run
    let mut level = 0u8;
    let mut j = i;
    while j < line.len() && line[j] == b'#' && level < 6 {
        level += 1;
        j += 1;
    }
    if level == 0 {
        return None;
    }
    // if the run continued to a 7th `#`, this is not an ATX heading
    if j < line.len() && line[j] == b'#' {
        return None;
    }
    // after the `#` run: either end-of-line, or whitespace then optional title
    if j == line.len() {
        return Some((level, String::new()));
    }
    let sep = line[j];
    if sep != b' ' && sep != b'\t' {
        return None;
    }
    // skip the separating whitespace run
    while j < line.len() && (line[j] == b' ' || line[j] == b'\t') {
        j += 1;
    }
    if j == line.len() {
        return Some((level, String::new()));
    }
    // take the rest as the title, trimming trailing whitespace
    let mut end = line.len();
    while end > j && (line[end - 1] == b' ' || line[end - 1] == b'\t') {
        end -= 1;
    }
    let title = String::from_utf8_lossy(&line[j..end]).into_owned();
    Some((level, title))
}

/// A code-fence detector: up to three leading spaces, then a run of 3+
/// backticks or tildes. Returns `(fence_char, run_length)`. Mirrors
/// `mdtool.py::FENCE_RE` (`^ {0,3}(`{3,}|~{3,})`).
pub fn fence_of(line: &[u8]) -> Option<(u8, usize)> {
    let mut i = 0;
    // tolerate up to three leading spaces
    while i < line.len() && line[i] == b' ' && i < 3 {
        i += 1;
    }
    if i >= line.len() {
        return None;
    }
    let c = line[i];
    if c != b'`' && c != b'~' {
        return None;
    }
    let mut n = 0;
    let mut j = i;
    while j < line.len() && line[j] == c {
        n += 1;
        j += 1;
    }
    if n < 3 {
        return None;
    }
    Some((c, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_three_plus_same_char() {
        assert_eq!(fence_of(b"```"), Some((b'`', 3)));
        assert_eq!(fence_of(b"```rust"), Some((b'`', 3)));
        assert_eq!(fence_of(b"~~~~"), Some((b'~', 4)));
        assert_eq!(fence_of(b"  ```"), Some((b'`', 3)));
        assert_eq!(fence_of(b"   ~~~"), Some((b'~', 3)));
        // fewer than 3, or a non-fence char, is not a fence
        assert_eq!(fence_of(b"``"), None);
        assert_eq!(fence_of(b"~~"), None);
        assert_eq!(fence_of(b"###"), None);
        assert_eq!(fence_of(b"# Title"), None);
        assert_eq!(fence_of(b"    ```"), None); // four spaces is too many
    }

    #[test]
    fn fence_close_rule_same_char_at_least_len() {
        // the *caller* applies the close rule (same char, len >= opener);
        // fence_of just reports (char, len) so the caller can compare.
        let open = fence_of(b"```rust").unwrap();
        let close = fence_of(b"```").unwrap();
        assert_eq!(close.0, open.0);
        assert!(close.1 >= open.1);
        // a `~~~` line does NOT close a ``` fence (different char)
        let tilde = fence_of(b"~~~").unwrap();
        assert_ne!(tilde.0, open.0);
    }

    #[test]
    fn heading_levels_and_titles() {
        assert_eq!(heading_of(b"# Title"), Some((1, "Title".to_string())));
        assert_eq!(heading_of(b"## Files"), Some((2, "Files".to_string())));
        assert_eq!(heading_of(b"### Sub"), Some((3, "Sub".to_string())));
        assert_eq!(heading_of(b"###### Deep"), Some((6, "Deep".to_string())));
        // no title
        assert_eq!(heading_of(b"#"), Some((1, String::new())));
        assert_eq!(heading_of(b"#   "), Some((1, String::new())));
        // leading spaces (<=3)
        assert_eq!(
            heading_of(b"  # Indented"),
            Some((1, "Indented".to_string()))
        );
        // tab separator
        assert_eq!(heading_of(b"#\tTabbed"), Some((1, "Tabbed".to_string())));
    }

    #[test]
    fn heading_not_matched_cases() {
        assert_eq!(heading_of(b"####### Seven"), None); // 7+ hashes
        assert_eq!(heading_of(b"####"), Some((4, String::new())));
        assert_eq!(heading_of(b"#Title"), None); // no space
        assert_eq!(heading_of(b"    # Four spaces"), None); // >3 leading
        assert_eq!(heading_of(b"plain line"), None);
        assert_eq!(heading_of(b"##files"), None);
    }

    #[test]
    fn doc_new_empty() {
        let d = Doc::new(Vec::new());
        assert!(d.sections.is_empty());
        assert_eq!(d.total_lines, 0);
        assert_eq!(d.preamble_end, 0);
    }

    #[test]
    fn doc_new_single_h1() {
        let d = Doc::new(b"# Alpha\nhello world\n".to_vec());
        assert_eq!(d.sections.len(), 1);
        assert_eq!(d.sections[0].level, 1);
        assert_eq!(d.sections[0].title, "Alpha");
        assert_eq!(d.sections[0].ref_, "1");
        assert_eq!(d.preamble_end, 0);
        assert_eq!(d.sections[0].start, 0);
        assert_eq!(d.sections[0].end, d.data.len()); // whole file
        assert_eq!(d.sections[0].own_end, d.data.len());
        assert_eq!(d.total_lines, 2);
        assert_eq!(d.sections[0].start_line, 0);
        assert_eq!(d.sections[0].end_line, 2);
    }

    #[test]
    fn doc_new_tree_and_dotted_refs() {
        let buf = b"# Root\nintro\n\n## A\ncontent A\n\n## B\ncontent B\n\n### B1\ncontent B1\n\n# Sec2\nsecond\n"
            .to_vec();
        let d = Doc::new(buf);
        let refs: Vec<&str> = d.sections.iter().map(|s| s.ref_.as_str()).collect();
        assert_eq!(refs, vec!["1", "1.1", "1.2", "1.2.1", "2"]);
        let levels: Vec<u8> = d.sections.iter().map(|s| s.level).collect();
        assert_eq!(levels, vec![1, 2, 2, 3, 1]);
        // indices: 0=Root(1) 1=A(2) 2=B(2) 3=B1(3) 4=Sec2(1)
        // `Root`'s full span ends at the next H1 (`Sec2`).
        assert_eq!(d.sections[0].end, d.sections[4].start);
        // `A`'s full span ends at `B` (next level<=2).
        assert_eq!(d.sections[1].end, d.sections[2].start);
        // `B`'s full span ends at `Sec2` (next level<=2), *including B1*.
        assert_eq!(d.sections[2].end, d.sections[4].start);
        // `B1`'s full span also ends at `Sec2` (next level<=3).
        assert_eq!(d.sections[3].end, d.sections[4].start);
        // `Sec2` is last — reaches the file end.
        assert_eq!(d.sections[4].end, d.data.len());
        // own_end vs full: B's own body stops at B1 (next heading any level),
        // but its full span continues past B1 to Sec2.
        assert_eq!(d.sections[2].own_end, d.sections[3].start);
        assert!(d.sections[2].end > d.sections[2].own_end);
        assert!(d.sections[0].end < d.data.len());
        assert!(d.sections[2].end < d.data.len());
        // preamble is 0 (no text before the first H1)
        assert_eq!(d.preamble_end, 0);
    }

    #[test]
    fn doc_new_fences_are_skipped() {
        let buf = b"# Root\nintroduction text\n\n```\n# not a heading\n## still not\n```\n\n## Real heading\nbody\n"
            .to_vec();
        let d = Doc::new(buf);
        let titles: Vec<&str> = d.sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["Root", "Real heading"]);
    }

    #[test]
    fn is_dotted_accepts_and_rejects() {
        assert!(is_dotted("1"));
        assert!(is_dotted("12"));
        assert!(is_dotted("1.2"));
        assert!(is_dotted("1.2.3"));
        assert!(!is_dotted(""));
        assert!(!is_dotted("1."));
        assert!(!is_dotted(".1"));
        assert!(!is_dotted("1..2"));
        assert!(!is_dotted("a"));
        assert!(!is_dotted("1a"));
        assert!(!is_dotted("1 2"));
    }

    #[test]
    fn resolve_dotted_numbers() {
        let d = Doc::new(b"# Root\n\n## A\n\n## B\n\n### B1\n\n# Sec2\n".to_vec());
        assert_eq!(d.resolve("1"), Ok(0));
        assert_eq!(d.resolve("1.1"), Ok(1));
        assert_eq!(d.resolve("1.2"), Ok(2));
        assert_eq!(d.resolve("1.2.1"), Ok(3));
        assert_eq!(d.resolve("2"), Ok(4));
        assert!(matches!(
            d.resolve("1.3"),
            Err(SectionError::NoSection { .. })
        ));
        assert!(matches!(
            d.resolve("9"),
            Err(SectionError::NoSection { .. })
        ));
    }

    #[test]
    fn resolve_title_path_and_unique_exact_and_substring() {
        let d = Doc::new(
            b"# Root\nintro text\n\n## Installation\nbody\n\n### Quick Start\nbody\n\n## Usage\nbody\n"
                .to_vec(),
        );
        // dotted
        assert_eq!(d.resolve("1.1"), Ok(1));
        // full title path
        assert_eq!(d.resolve("Root > Installation"), Ok(1));
        assert_eq!(d.resolve("Root > Installation > Quick Start"), Ok(2));
        // unique exact title anywhere
        assert_eq!(d.resolve("Usage"), Ok(3));
        // unique substring (last part only)
        assert_eq!(d.resolve("Quick"), Ok(2));
    }

    #[test]
    fn resolve_empty_and_ambiguity() {
        let d = Doc::new(b"# Root\n\n## Alpha\none\n\n## Alpha\ntwo\n".to_vec());
        assert!(matches!(d.resolve(""), Err(SectionError::EmptyRef)));
        assert!(matches!(d.resolve("   "), Err(SectionError::EmptyRef)));
        // two sections both titled "Alpha" -> substring stage finds 2 -> ambiguous
        match d.resolve("Alpha") {
            Err(SectionError::Ambiguous { candidates, .. }) => {
                assert_eq!(
                    candidates,
                    vec!["1.1 Alpha".to_string(), "1.2 Alpha".to_string()]
                );
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn own_and_full_bytes_differ_when_subsections_exist() {
        let d = Doc::new(b"# Root\n\n## B\nown body\n\n### B1\nsub body\n".to_vec());
        let own = d.own_bytes("1.1").unwrap();
        let full = d.full_bytes("1.1").unwrap();
        // own body excludes B1; full span includes B1 -> full is strictly longer
        assert!(full.len() > own.len());
        let own = String::from_utf8(own.to_vec()).unwrap();
        let full = String::from_utf8(full.to_vec()).unwrap();
        assert!(own.starts_with("## B\nown body"));
        assert!(full.contains("### B1"));
    }

    #[test]
    fn splice_is_pure_and_correct() {
        let buf = b"hello world".to_vec();
        let new = splice(&buf, 0, 5, b"goodbye");
        assert_eq!(String::from_utf8(new).unwrap(), "goodbye world");
        // original buffer untouched (clone semantics)
        assert_eq!(&buf, b"hello world");
        // empty insert (pure no-op)
        let same = splice(&buf, 5, 5, b"");
        assert_eq!(String::from_utf8(same).unwrap(), "hello world");
        // replace tail
        let tail = splice(&buf, 6, 11, b"rust");
        assert_eq!(String::from_utf8(tail).unwrap(), "hello rust");
        // whole replace
        let whole = splice(&buf, 0, 11, b"X");
        assert_eq!(String::from_utf8(whole).unwrap(), "X");
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn splice_capacity_does_not_underflow_when_lo_gt_hi() {
        // in a release build (where `debug_assert!` is
        // compiled out) a caller passing `lo > hi` used to compute
        // `data.len() + new.len() - (hi - lo)` where `(hi - lo)` underflowed
        // to a huge usize → `Vec::with_capacity` OOM-panicked. With the
        // saturating capacity math the bound stays sane, so this must not
        // panic. Gated to release because in debug the `debug_assert!` guard
        // fires first (correctly).
        let data = b"hello world"; // len 11
        let new = splice(data, 8, 3, b"X"); // lo=8 > hi=3, both in-bounds
                                            // out = data[..8] ++ "X" ++ data[3..]
        let expected = format!(
            "{}X{}",
            String::from_utf8(data[..8].to_vec()).unwrap(),
            String::from_utf8(data[3..].to_vec()).unwrap()
        );
        assert_eq!(String::from_utf8_lossy(&new).as_ref(), expected);
    }

    // ---- editing ops -----------------------------------------------------

    #[test]
    fn insert_child_of_level_6_is_an_error() {
        // a `Child` insert under a level-6 target would
        // need level 7. The old `base.min(6)` clamped it back to 6 and silently
        // produced a *sibling* of the parent. Now it is a proper error.
        let d = Doc::new(b"# A\n\n## B\n\n### C\n\n#### D\n\n##### E\n\n###### F\nbody\n".to_vec());
        let f = d.resolve("1.1.1.1.1.1").expect("level-6 section resolves");
        assert_eq!(d.sections[f].level, 6, "target is level 6");
        match d.insert_section(
            SectionPlacement::Child,
            Some("1.1.1.1.1.1"),
            "child body",
            "Child of F",
            None,
        ) {
            Err(SectionError::ChildOfLeaf { .. }) => {}
            other => panic!("expected ChildOfLeaf under a level-6 target, got {other:?}"),
        }
        // A level-5 target is fine (child would be level 6) — pin the boundary.
        let d5 = Doc::new(b"# A\n\n## B\n\n### C\n\n#### D\n\n##### E\nbody\n".to_vec());
        let e = d5.resolve("1.1.1.1.1").expect("level-5 section resolves");
        assert_eq!(d5.sections[e].level, 5, "target is level 5");
        d5.insert_section(
            SectionPlacement::Child,
            Some("1.1.1.1.1"),
            "child body",
            "Child of E",
            None,
        )
        .expect("child of a level-5 target is legal (yields level 6)");
    }

    #[test]
    fn replace_in_whole_file_and_scoped() {
        let d = Doc::new(b"# Root\nhello old\n\n## A\nold text\n\n## B\nkeep old\n".to_vec());
        // whole file: replace 2 occurrences
        let (new, n) = d.replace_in(None, "old", "new", 0).unwrap();
        assert_eq!(n, 3); // whole, three occurrences (hello old, old text, keep old)
        let s = String::from_utf8(new).unwrap();
        assert!(s.contains("hello new"));
        assert!(s.contains("new text"));
        assert!(s.contains("keep new"));
        // scoped to section 1.2 (B): replace 1 occurrence only
        let (new, n) = d.replace_in(Some("1.2"), "old", "new", 0).unwrap();
        assert_eq!(n, 1);
        let s = String::from_utf8(new).unwrap();
        assert!(s.contains("keep new"));
        assert!(s.contains("old text")); // section B is not 1.2 (B is 1.2) — wait:
                                         // 1.1=A, 1.2=B. So scoped to B: B's "old"->"new", A's "old text" untouched.
    }

    #[test]
    fn replace_in_count_limits_replacements() {
        let d = Doc::new(b"# Root\nfoo foo foo\n".to_vec());
        let (new, n) = d.replace_in(None, "foo", "bar", 2).unwrap();
        assert_eq!(n, 2);
        let s = String::from_utf8(new).unwrap();
        assert_eq!(s, "# Root\nbar bar foo\n");
    }

    #[test]
    fn replace_in_no_match_is_notfound() {
        let d = Doc::new(b"# Root\nhello\n".to_vec());
        match d.replace_in(None, "absent", "x", 0) {
            Err(SectionError::NotFound { scope, .. }) => {
                assert_eq!(scope, "whole file".to_string());
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
        match d.replace_in(Some("1"), "absent", "x", 0) {
            Err(SectionError::NotFound { scope, .. }) => {
                assert_eq!(scope, "1".to_string());
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn set_section_default_replaces_whole_section() {
        let d = Doc::new(b"# Root\nold heading body\n\n## A\nold A body\n".to_vec());
        let new = d
            .set_section("1.1", "## A\nbrand new A body\n", false)
            .unwrap();
        let s = String::from_utf8(new).unwrap();
        assert!(s.contains("brand new A body"));
        assert!(!s.contains("old A body"));
        // the heading was replaced by the caller-supplied heading
        assert!(!s.contains("## A\nold"));
    }

    #[test]
    fn set_section_keep_heading_preserves_heading_line() {
        let d = Doc::new(b"# Root\n\n## A\nold A body\n".to_vec());
        let new = d.set_section("1.1", "brand new A body", true).unwrap();
        let s = String::from_utf8(new).unwrap();
        let pos = s.find("old A body").unwrap_or(usize::MAX);
        // the original heading line `## A` should still be present, followed
        // by a blank line, then the new body
        assert!(s.starts_with("# Root\n\n## A\n\nbrand new A body"));
        assert_eq!(pos, usize::MAX);
    }

    #[test]
    fn remove_section_middle() {
        // Refs: Top=1, Mid=1.1, Bot=1.2.
        let d = Doc::new(b"# Top\ntop\n\n## Mid\nmid\n\n## Bot\nbot\n".to_vec());
        let new = d.remove_section("1.1").unwrap();
        let s = String::from_utf8(new).unwrap();
        assert!(
            s.contains("## Bot") && s.contains("bot"),
            "sibling after survives: {s:?}"
        );
        assert!(
            s.contains("# Top") && s.contains("top"),
            "sibling before survives: {s:?}"
        );
        assert!(
            !s.contains("## Mid") && !s.contains("mid"),
            "target gone: {s:?}"
        );
        // No doubled blank where the section used to be.
        assert!(!s.contains("top\n\n\n"), "separator doubled: {s:?}");
        assert!(s.ends_with('\n'), "newline-terminated: {s:?}");
    }

    #[test]
    fn remove_section_last() {
        let d = Doc::new(b"# Top\ntop\n\n## Bot\nbot\n".to_vec());
        let new = d.remove_section("1.1").unwrap();
        let s = String::from_utf8(new).unwrap();
        assert!(
            s.contains("# Top") && s.contains("top"),
            "Top survives: {s:?}"
        );
        assert!(
            !s.contains("## Bot") && !s.contains("bot"),
            "Bot gone: {s:?}"
        );
        // The trailing separator of the removed last section is folded into a
        // single newline (the ensure_trailing_newline convention).
        assert!(s.ends_with("top\n"), "exactly one trailing newline: {s:?}");
    }

    #[test]
    fn remove_section_whole_root_empties_buffer() {
        let d = Doc::new(b"# Top\ntop\n\n## Mid\nmid\n".to_vec());
        let new = d.remove_section("1").unwrap();
        assert!(
            new.is_empty(),
            "removing the root section yields an empty buffer: {new:?}"
        );
    }

    #[test]
    fn remove_section_includes_its_subtree() {
        // Refs: Root=1, Alpha=1.1, Alpha-Sub=1.1.1, Beta=1.2.
        let d = Doc::new(
            b"# Root\nroot\n\n## Alpha\nalpha\n\n### Alpha-Sub\nsub\n\n## Beta\nbeta\n".to_vec(),
        );
        let new = d.remove_section("1.1").unwrap();
        let s = String::from_utf8(new).unwrap();
        assert!(
            s.contains("# Root") && s.contains("root"),
            "Root intact: {s:?}"
        );
        assert!(!s.contains("## Alpha"), "Alpha gone: {s:?}");
        assert!(!s.contains("### Alpha-Sub"), "child also gone: {s:?}");
        assert!(!s.contains("\nsub\n"), "child body gone: {s:?}");
        assert!(
            s.contains("## Beta") && s.contains("beta"),
            "sibling Beta intact: {s:?}"
        );
    }

    #[test]
    fn remove_section_unknown_ref_is_no_section() {
        let d = Doc::new(b"# Top\ntop\n".to_vec());
        match d.remove_section("9.9.9") {
            Err(SectionError::NoSection { ref_ }) => assert_eq!(ref_, "9.9.9"),
            other => panic!("expected NoSection, got {other:?}"),
        }
    }

    #[test]
    fn append_section_appends_before_next_heading() {
        let d = Doc::new(b"# Root\n\n## A\ncurrent A body\n\n## B\nb body\n".to_vec());
        let new = d.append_section("1.1", "appended text").unwrap();
        let s = String::from_utf8(new).unwrap();
        // A's content now contains the append; A still ends before `## B`.
        assert!(s.contains("current A body\n\nappended text"));
        assert!(s.contains("## B\nb body"));
        // and append is *before* `## B`
        let pos_a = s.find("appended text").unwrap();
        let pos_b = s.find("## B").unwrap();
        assert!(pos_a < pos_b);
    }

    #[test]
    fn append_section_creates_blank_line_separation() {
        let d = Doc::new(b"# Root\nno blank here\n".to_vec());
        let new = d.append_section("1", "more").unwrap();
        let s = String::from_utf8(new).unwrap();
        // existing content ends without a blank line; the append must add one
        assert!(s.contains("no blank here\n\nmore"));
    }

    #[test]
    fn append_section_trailing_blank_text_next_sibling() {
        // Regression: text carrying its own trailing blank must not double up
        // the boundary before the next sibling — exactly one blank each side.
        let d = Doc::new(b"# Root\n\n## A\ncurrent A body\n\n## B\nb body\n".to_vec());
        let new = d.append_section("1.1", "new line\n\n").unwrap();
        let s = String::from_utf8(new).unwrap();
        assert_eq!(
            s,
            "# Root\n\n## A\ncurrent A body\n\nnew line\n\n## B\nb body\n"
        );
    }

    #[test]
    fn append_section_leading_blank_text_next_sibling() {
        // Regression: a leading blank on the text must not double up either.
        let d = Doc::new(b"# Root\n\n## A\ncurrent A body\n\n## B\nb body\n".to_vec());
        let new = d.append_section("1.1", "\nnew line").unwrap();
        let s = String::from_utf8(new).unwrap();
        assert_eq!(
            s,
            "# Root\n\n## A\ncurrent A body\n\nnew line\n\n## B\nb body\n"
        );
    }

    #[test]
    fn append_section_eof_plain_terminated() {
        // At end-of-file the append is just newline-terminated — no trailing
        // blank is annexed (there is no next sibling to separate from).
        let d = Doc::new(b"# Root\n\n## A\ncurrent A body\n".to_vec());
        let new = d.append_section("1.1", "new line").unwrap();
        let s = String::from_utf8(new).unwrap();
        assert_eq!(s, "# Root\n\n## A\ncurrent A body\n\nnew line\n");
    }

    #[test]
    fn append_section_eof_trailing_blank_drops_double() {
        // The text's own trailing blank must not survive at EOF either.
        let d = Doc::new(b"# Root\n\n## A\ncurrent A body\n".to_vec());
        let new = d.append_section("1.1", "new line\n\n").unwrap();
        let s = String::from_utf8(new).unwrap();
        assert_eq!(s, "# Root\n\n## A\ncurrent A body\n\nnew line\n");
    }

    #[test]
    fn insert_section_end() {
        let d = Doc::new(b"# Root\nbody\n".to_vec());
        let (ins, chunk) = d
            .insert_section(SectionPlacement::End, None, "new body text", "New", Some(2))
            .unwrap();
        assert_eq!(ins, d.data.len());
        let s = String::from_utf8(chunk.to_vec()).unwrap();
        assert!(s.contains("## New"));
        assert!(s.contains("new body text"));
    }

    #[test]
    fn insert_section_child_inferred_level() {
        let d = Doc::new(b"# Root\n\n## A\nbody A\n".to_vec());
        let (_ins, chunk) = d
            .insert_section(
                SectionPlacement::Child,
                Some("1.1"),
                "child body",
                "Child",
                None,
            )
            .unwrap();
        let s = String::from_utf8(chunk.to_vec()).unwrap();
        // prefix is a blank-line separator (the target ends with a single
        // newline), then the heading `### Child`
        assert!(s.strip_prefix('\n').unwrap().starts_with("### Child"));
        assert!(s.contains("### Child\n\nchild body"));
    }

    #[test]
    fn insert_section_before_uses_target_start() {
        let d = Doc::new(b"# Root\n\n## A\nA body\n".to_vec());
        let (ins, _chunk) = d
            .insert_section(SectionPlacement::Before, Some("1.1"), "x", "Pre", None)
            .unwrap();
        // insertion is exactly at `A`'s start (the A heading byte)
        assert_eq!(ins, d.sections[1].start);
    }

    #[test]
    fn doc_new_fence_close_needs_same_char_and_len() {
        // open with ~~~~, close must be ~~~ or longer, same char
        let buf = b"# Root\nbefore\n\n~~~~\n# fenced H1 inside\n~~~~\n\n## After\nbody\n".to_vec();
        let d = Doc::new(buf);
        let titles: Vec<&str> = d.sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["Root", "After"]);
    }

    // ---- T7: section_lookup(body, ref) → Result<Option<usize>, SectionError> ----

    #[test]
    fn t7_lookup_dotted_returns_heading_line() {
        // lines: 0="# Root" 1="" 2="## A" 3="A body" 4="" 5="## B" 6="B body"
        let body = "# Root\n\n## A\nA body\n\n## B\nB body\n";
        assert_eq!(section_lookup(body, "1"), Ok(Some(0)));
        assert_eq!(section_lookup(body, "1.1"), Ok(Some(2)));
        assert_eq!(section_lookup(body, "1.2"), Ok(Some(5)));
    }

    #[test]
    fn t7_lookup_title_returns_heading_line() {
        // lines: 0="# Root" 1="" 2="## A" 3="A body" 4="" 5="## B" 6="B body"
        let body = "# Root\n\n## A\nA body\n\n## B\nB body\n";
        assert_eq!(section_lookup(body, "A"), Ok(Some(2)));
        assert_eq!(section_lookup(body, "Root > B"), Ok(Some(5)));
    }

    #[test]
    fn t7_lookup_missing_section_is_none() {
        let body = "# Root\n\n## A\nA body\n";
        // a ref that names no section → Ok(None) (not an error)
        assert_eq!(section_lookup(body, "9.9"), Ok(None));
        assert_eq!(section_lookup(body, "Nope"), Ok(None));
        // empty ref → Ok(None)
        assert_eq!(section_lookup(body, ""), Ok(None));
        // empty body → Ok(None)
        assert_eq!(section_lookup("", "A"), Ok(None));
    }

    #[test]
    fn t7_lookup_ambiguous_is_err() {
        let body = "# Root\n\n## Dup\nx\n\n## Dup\ny\n";
        match section_lookup(body, "Dup") {
            Err(SectionError::Ambiguous { candidates, .. }) => {
                assert!(!candidates.is_empty());
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn t7_lookup_title_is_case_insensitive() {
        // "watchmaking" / "WATCHMAKING" / "Watchmaking" all resolve to the same line
        let body = "# Root\n\n## Watchmaking\ncontent\n\n## B\ny\n";
        // lines: 0="# Root" 1="" 2="## Watchmaking" 3="content" 4="" 5="## B" 6="y"
        assert_eq!(section_lookup(body, "Watchmaking"), Ok(Some(2)));
        assert_eq!(section_lookup(body, "watchmaking"), Ok(Some(2)));
        assert_eq!(section_lookup(body, "WATCHMAKING"), Ok(Some(2)));
        // and the title path is case-insensitive too
        assert_eq!(section_lookup(body, "root > WATCHMAKING"), Ok(Some(2)));
    }

    #[test]
    fn t7_lookup_heading_at_last_line() {
        // lines: 0="# Root" 1="" 2="body" 3="## Last" 4="last body"
        // "Last" is a heading not at line 0 and not the deepest one — returns line 3
        let body = "# Root\n\nbody\n## Last\nlast body\n";
        assert_eq!(section_lookup(body, "Last"), Ok(Some(3)));
        assert_eq!(section_lookup(body, "1.1"), Ok(Some(3)));
        // a leaf that is also the final heading of a three-level page
        // lines: 0="# Root" 1="" 2="## Mid" 3="" 4="### Tail" 5="tail body"
        let tail = "# Root\n\n## Mid\n\n### Tail\ntail body\n";
        assert_eq!(section_lookup(tail, "Tail"), Ok(Some(4)));
        assert_eq!(section_lookup(tail, "1.1.1"), Ok(Some(4)));
    }

    #[test]
    fn t7_lookup_single_h1_resolves_to_line_zero() {
        // a page that is itself one H1 with no sub-structure still resolves
        assert_eq!(section_lookup("# Only Heading\nbody\n", "1"), Ok(Some(0)));
        assert_eq!(
            section_lookup("# Only Heading\nbody\n", "Only Heading"),
            Ok(Some(0))
        );
    }

    // ---- export_rows (the `sections` index rows) -------------------------

    #[test]
    fn export_rows_headingless_page_single_star_row_whole_body() {
        let t = "plain body line one\nplain body line two\n";
        let d = Doc::new(t.as_bytes().to_vec());
        let rows = d.export_rows("Plain");
        assert_eq!(rows.len(), 1, "a heading-less page exports exactly the '*' row");
        let r = &rows[0];
        assert_eq!(r.ref_, "*");
        assert_eq!(r.level, 0);
        assert_eq!(r.title, "Plain");
        assert_eq!(r.own_body, t, "'*' own_body is the whole body when there are no headings");
        assert_eq!(r.full_body, None, "no subsections ⇒ full_body is the (empty-encoding) None");
    }

    #[test]
    fn export_rows_preamble_only_not_whole_page() {
        // LOCKED: the `*` row is PRESENT-ONLY — `body[0..preamble_end]` — NOT
        // the whole-page superset. The H1 is already its own row.
        let t = "intro text before the heading\n\n# Top\n\nTop body\n\n## Sub\n\nSub body\n";
        let d = Doc::new(t.as_bytes().to_vec());
        let rows = d.export_rows("Top");
        assert_eq!(rows.len(), 3);
        let star = &rows[0];
        assert_eq!(star.ref_, "*");
        assert_eq!(star.own_body, "intro text before the heading\n\n");
        let top = &rows[1];
        assert_eq!(top.ref_, "1");
        assert_eq!(top.own_body, "# Top\n\nTop body\n\n");
        assert!(top.full_body.is_some(), "a parent row has subsections ⇒ Some full_body");
        // full_body for `1` = from `# Top` through the end of `## Sub`.
        let want_top_full = "# Top\n\nTop body\n\n## Sub\n\nSub body\n";
        assert_eq!(top.full_body.as_deref().unwrap(), want_top_full);
        let sub = &rows[2];
        assert_eq!(sub.ref_, "1.1");
        assert!(sub.full_body.is_none(), "a leaf row's full span == own span ⇒ None");
    }

    #[test]
    fn export_rows_heading_first_page_has_empty_star_body() {
        // The page opens with a heading ⇒ preamble_end == 0 AND sections
        // non-empty ⇒ the `*` row's own_body is EMPTY (its H1 is indexed as
        // a real row; nothing is lost).
        let t = "# One\nbody one\n\n## Two\nbody two\n";
        let d = Doc::new(t.as_bytes().to_vec());
        let rows = d.export_rows("One");
        assert_eq!(rows[0].ref_, "*");
        assert_eq!(rows[0].own_body, "");
        assert_eq!(rows[1].ref_, "1");
        assert_eq!(rows[1].title, "One");
    }

    #[test]
    fn export_rows_fenced_hash_is_not_a_heading() {
        // A `#` inside a fenced code block must stay body, not open a section.
        let t = "# Real\n\ntext with a fence:\n```\n# not a heading\n## still not\n```\n\n## Second\n";
        let d = Doc::new(t.as_bytes().to_vec());
        let rows = d.export_rows("Real");
        assert_eq!(rows.len(), 3, "* + Real + Second — the fenced # lines are sections");
        let first = &rows[1];
        assert!(
            first.own_body.contains("# not a heading"),
            "the fenced # line must be content of section 1, got: {:?}",
            first.own_body
        );
        assert_eq!(rows[2].ref_, "1.1");
    }

    #[test]
    fn show_star_ref_returns_preamble() {
        let t = "intro only\n\n# Top\n\nTop body\n";
        let d = Doc::new(t.as_bytes().to_vec());
        assert_eq!(d.own_bytes("*").unwrap(), b"intro only\n\n");
        // a heading-first page: `*` → empty span
        let d2 = Doc::new(b"# A\nbody\n".to_vec());
        assert_eq!(d2.own_bytes("*").unwrap(), b"");
        // a heading-less page: `*` → the whole body
        let d3 = Doc::new(b"no headings here\n".to_vec());
        assert_eq!(d3.own_bytes("*").unwrap(), b"no headings here\n");
    }

    #[test]
    fn t7_lookup_line_folds_ambiguity_to_none() {
        let body = "# Root\n\n## A\nx\n\n## A\ny\n";
        // title "A" is ambiguous → section_lookup_line collapses to None
        assert_eq!(section_lookup_line(body, "A"), None);
        // but the dotted ref 1.1 uniquely names the FIRST "A" → Ok(Some(line))
        // lines: 0="# Root" 1="" 2="## A" 3="x" 4="" 5="## A" 6="y"
        assert_eq!(section_lookup_line(body, "1.1"), Some(2));
        // clean (non-ambiguous) page → returns the line
        assert_eq!(
            section_lookup_line("# Root\n\n## A\nx\n\n## B\ny\n", "B"),
            Some(5)
        );
    }
}
