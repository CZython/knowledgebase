//! Markdown intermediate representation + parser (PLAN-GUI §7, G3).
//!
//! Two-stage pipeline:
//!
//! 1. **Parse** — `pulldown-cmark` events → an owned IR (`Doc` arena +
//!    recursive inline subtrees). Pure, no egui. Unit-tested headlessly.
//! 2. **Paint** — walk the IR, emit egui widgets. See `render.rs`.
//!
//! ## Parser shape
//!
//! A *frame stack* of `usize` (arena indices of blocks currently open)
//! plus a *slot stack* of `Vec<Inline>` (inline buffers currently open).
//!
//! The invariant is:
//!
//!   `slots.len() >= 1` iff a block is open.
//!   `slots.first()` is the block's own inline vec (the "bottom" of the
//!   stack). `slots.last()` (if any) is the innermost inline wrapper
//!   (Strong / Emphasis / Strikethrough / Link label / Image alt / cell).
//!
//! `Text` / `Code` events push into `slots.last()`. Inline-wrapper closes
//! pop the slot, wrap into the appropriate `Inline`, and push back into
//! `slots.last()` — so nesting works naturally. Block closes pop the slot
//! (the bottom one) and attach it to the block.
//!
//! Container blocks (`BlockQuote`, `ListItem`, `List`) hold their children
//! as arena indices, so a block never owns any sibling / child by value.

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A complete document: an arena of blocks plus the root order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Doc {
    /// All blocks in definition order, referenced by `root` and by
    /// container blocks' child index vectors.
    pub blocks: Vec<Block>,
    /// Top-level order of `blocks`. Empty for an empty document.
    pub root: Vec<usize>,
}

/// One block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
    },
    Paragraph {
        inlines: Vec<Inline>,
    },
    Code {
        /// Language tag from the fence (e.g. `rust`). `None` for indented.
        lang: Option<String>,
        text: String,
    },
    List {
        ordered: bool,
        /// 1-based starting number for `ordered`.
        start: u64,
        /// Arena indices of child items, in source order.
        items: Vec<usize>,
    },
    ListItem {
        /// `Some(true)` checked / `Some(false)` unchecked / `None` plain.
        task: Option<bool>,
        /// Direct inline content (tight list items emit inlines directly under
        /// the item with no Paragraph wrapper). Empty for loose items, whose
        /// text lives in `Paragraph` children instead.
        inlines: Vec<Inline>,
        /// Arena indices of child blocks (paragraphs, nested lists, etc.).
        children: Vec<usize>,
    },
    Quote {
        /// Arena indices of child blocks.
        children: Vec<usize>,
    },
    Table {
        /// Per-column alignment (one per column).
        aligns: Vec<Align>,
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
    Html {
        text: String,
    },
}

/// Column alignment for a table cell (mirrors pulldown-cmark's `Alignment`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    None,
    Left,
    Center,
    Right,
}

/// An inline (leaf or wrapper) text fragment. Recursive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Code(String),
    /// `href` may be a URL, a relative path, or a `wiki:` slug. The renderer
    /// maps `href` to internal navigation when it matches a known corpus
    /// page (PLAN-GUI §6.16), otherwise opens in the system browser.
    Link {
        href: String,
        label: Vec<Inline>,
    },
    /// A Markdown image. The renderer paints it as an `egui::Image` when
    /// local, as a placeholder when remote / unresolvable.
    Image {
        src: String,
        alt: String,
    },
    Strong(Vec<Inline>),
    Emphasis(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    /// `true` = hard break (newline), `false` = soft break (space).
    Break(bool /* hard */),
}

/// Flatten an inline tree to plain text (for search / a11y / fallback).
pub fn flat_text(inlines: &[Inline]) -> String {
    let mut out = String::with_capacity(64);
    flatten_into(inlines, &mut out);
    out
}

fn flatten_into(inlines: &[Inline], out: &mut String) {
    for i in inlines {
        match i {
            Inline::Text(t) => out.push_str(t),
            Inline::Code(c) => out.push_str(c),
            Inline::Link { label, .. } => flatten_into(label, out),
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::Strong(v) => flatten_into(v, out),
            Inline::Emphasis(v) => flatten_into(v, out),
            Inline::Strikethrough(v) => flatten_into(v, out),
            Inline::Break(h) => out.push(if *h { '\n' } else { ' ' }),
        }
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

impl Block {
    /// Resolve this block's byte range in `body` (a frontmatter-less body) —
    /// the WYSIWYG span-splice target (PLAN-GUI §8.2 / §8.6). The block is
    /// anchored to its own source line(s) by its verbatim content, which is
    /// unambiguous for the splicible kinds (Heading / Paragraph / Code).
    ///
    /// Returns `None` if the anchor is not found (a degenerate document); the
    /// caller falls back to the read-only rendering for that block.
    pub fn range(&self, body: &str) -> Option<std::ops::Range<usize>> {
        let lines = lines_of(body);
        match self {
            Block::Heading { level, inlines } => {
                let text = ws_strip(&flat_text(inlines));
                let hashes = "#".repeat(*level as usize);
                let idx = lines.iter().position(|(_, line)| {
                    let rest = line.get(hashes.len()..).unwrap_or("");
                    rest.starts_with([' ', '\t']) && ws_strip(&plain_in(&rest[1..])) == text
                })?;
                Some(lines[idx].0..end_of(&lines, idx))
            }
            Block::Paragraph { inlines } => {
                let text = ws_strip(&flat_text(inlines));
                for i in 0..lines.len() {
                    if lines[i].1.trim().is_empty() {
                        continue;
                    }
                    let j_max = run_end_index(&lines, i);
                    for j in i..=j_max {
                        let run = lines[i..=j]
                            .iter()
                            .map(|(_, l)| *l)
                            .collect::<Vec<_>>()
                            .join("\n");
                        if ws_strip(&plain_in(&run)) == text {
                            return Some(lines[i].0..end_of(&lines, j));
                        }
                    }
                }
                None
            }
            Block::Code { text, lang } => {
                let idx = find_code_start(&lines, text, lang.as_deref())?;
                let end = if lang.is_some() {
                    let close = lines
                        .iter()
                        .enumerate()
                        .skip(idx + 1)
                        .find(|(_, (_, l))| {
                            let t = l.trim_start();
                            t.starts_with("```") || t.starts_with("~~~")
                        })
                        .map(|p| p.0)
                        .unwrap_or(lines.len() - 1);
                    end_of(&lines, close)
                } else {
                    let indent = text
                        .lines()
                        .next()
                        .map(|l| l.chars().take_while(|c| *c == ' ').count())
                        .unwrap_or(0);
                    let pad = " ".repeat(indent);
                    let mut j = idx;
                    while j + 1 < lines.len() {
                        let l = lines[j + 1].1;
                        if l.starts_with(&pad) && !l.trim().is_empty() {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    end_of(&lines, j)
                };
                Some(lines[idx].0..end)
            }
            _ => None,
        }
    }
}

/// Extract the content of a single-line Markdown list item from a raw source
/// line (`"- apple"`, `"1. apple"`, `"  - [ ] apple"`). Returns `None` if
/// the line is not a top-level list item marker.
pub fn list_item_text_from_line(line: &str) -> Option<String> {
    let t = line.trim_start();
    if let Some(r) = t.strip_prefix("- [ ] ") {
        return Some(r.to_string());
    }
    if let Some(r) = t.strip_prefix("- [x] ") {
        return Some(r.to_string());
    }
    if let Some(r) = t.strip_prefix("- [X] ") {
        return Some(r.to_string());
    }
    if let Some(r) = t.strip_prefix("* [ ] ") {
        return Some(r.to_string());
    }
    if let Some(r) = t.strip_prefix("* [x] ") {
        return Some(r.to_string());
    }
    if let Some(r) = t.strip_prefix("- ") {
        return Some(r.to_string());
    }
    if let Some(r) = t.strip_prefix("* ") {
        return Some(r.to_string());
    }
    if let Some(r) = t.strip_prefix("+ ") {
        return Some(r.to_string());
    }
    if let Some(pos) = t.find(". ") {
        let prefix = &t[..pos];
        if !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_digit()) {
            return Some(t[pos + 2..].to_string());
        }
    }
    None
}

/// Each line of `body` as `(byte_offset_of_line_start, &str)`, where the
/// offset is *exclusive* of the terminating `\n` (i.e. the start of the
/// content). This is the unit the span resolver works in.
pub fn lines_of(body: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    for line in body.lines() {
        out.push((off, line));
        off += line.len() + 1;
    }
    out
}

/// End offset (exclusive) of the line at index `i` into `lines`: past its
/// terminating `\n` if present, else the start of *next* line. Total.
pub fn end_of(lines: &[(usize, &str)], i: usize) -> usize {
    let (start, line) = lines[i];
    if i + 1 < lines.len() {
        lines[i + 1].0
    } else {
        start + line.len()
    }
}

/// Index of the last consecutive non-blank line starting at `i` (the run
/// boundary for a paragraph). If `i` is blank, returns `i`.
pub fn run_end_index(lines: &[(usize, &str)], i: usize) -> usize {
    let mut j = i;
    while j + 1 < lines.len() && !lines[j + 1].1.trim().is_empty() {
        j += 1;
    }
    j
}

/// Line-index of the first line of a `Code` block by content anchor: the
/// opening fence carrying the language tag (fenced), or the first source line
/// whose trimmed content equals the block's first line (indented).
fn find_code_start(lines: &[(usize, &str)], text: &str, lang: Option<&str>) -> Option<usize> {
    let first = ws_strip(text.lines().next().unwrap_or(""));
    if lang.is_some() {
        let want = lang.unwrap_or("");
        lines.iter().position(|(_, l)| {
            let t = l.trim_start();
            t.starts_with("```") && t["```".len()..].trim() == want
        })
    } else {
        lines
            .iter()
            .position(|(_, l)| ws_strip(l.trim_end()) == first)
    }
}

/// Flatten pulldown events in `text` to plain content (markers removed) —
/// so IR plain text (which already drops markers) matches an anchor line that
/// *carries* the markers (inline code, emphasis, links). Collects `Text` and
/// `Code` leaves inline; emits a space for `HardBreak` / `SoftBreak` so
/// adjacent inline fragments on the source side line up with the IR side.
pub fn plain_in(text: &str) -> String {
    let mut out = String::new();
    for ev in Parser::new_ext(
        text,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    ) {
        match ev {
            Event::Text(t) => out.push_str(&t),
            Event::Code(c) => out.push_str(&c),
            Event::HardBreak | Event::SoftBreak => out.push(' '),
            _ => {}
        }
    }
    out
}

/// Collapse internal whitespace runs to a single space and trim — used to
/// normalize IR plain text against a source anchor (which may carry differing
/// indentation, hard-break spacing, or trailing punctuation noise).
pub fn ws_strip(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// Parse a Markdown string into a `Doc` (the G3 renderer's input).
///
/// Enables GFM extras (tables, strikethrough, task lists). CommonMark is
/// the base dialect.
pub fn parse(md: &str) -> Doc {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(md, opts);
    let events: Vec<Event> = parser.collect();

    let mut doc = Doc::default();
    let mut stack: Vec<usize> = Vec::new(); // open block frames
    let mut slots: Vec<Vec<Inline>> = Vec::new(); // open inline slots (bottom = block's inlines)
    let mut hrefs: Vec<String> = Vec::new(); // href/src in flight for Link / Image
    let mut img: Vec<bool> = Vec::new(); // parallel: is the slot an Image?
    let mut table: Option<TableBuilder> = None;

    for ev in events {
        match ev {
            // ---------- block starts ---------------------------------------
            Event::Start(Tag::Heading { level, .. }) => {
                let idx = doc.blocks.len();
                doc.blocks.push(Block::Heading {
                    level: level as u8,
                    inlines: Vec::new(),
                });
                attach(&mut doc, &stack, idx);
                stack.push(idx);
                slots.push(Vec::new());
            }
            Event::Start(Tag::Paragraph) => {
                let idx = doc.blocks.len();
                doc.blocks.push(Block::Paragraph {
                    inlines: Vec::new(),
                });
                attach(&mut doc, &stack, idx);
                stack.push(idx);
                slots.push(Vec::new());
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Indented => None,
                    CodeBlockKind::Fenced(s) => {
                        let t = s.trim();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t.to_string())
                        }
                    }
                };
                let idx = doc.blocks.len();
                doc.blocks.push(Block::Code {
                    lang,
                    text: String::new(),
                });
                attach(&mut doc, &stack, idx);
                stack.push(idx);
                // No inline slot: text lands directly in the block's `text`.
            }
            Event::Start(Tag::List(number)) => {
                let idx = doc.blocks.len();
                doc.blocks.push(Block::List {
                    ordered: number.is_some(),
                    start: number.unwrap_or(1),
                    items: Vec::new(),
                });
                attach(&mut doc, &stack, idx);
                stack.push(idx);
            }
            Event::Start(Tag::Item) => {
                let idx = doc.blocks.len();
                doc.blocks.push(Block::ListItem {
                    task: None,
                    inlines: Vec::new(),
                    children: Vec::new(),
                });
                attach(&mut doc, &stack, idx);
                stack.push(idx);
                slots.push(Vec::new());
            }
            Event::Start(Tag::BlockQuote(_)) => {
                let idx = doc.blocks.len();
                doc.blocks.push(Block::Quote {
                    children: Vec::new(),
                });
                attach(&mut doc, &stack, idx);
                stack.push(idx);
            }
            Event::Start(Tag::Table(aligns)) => {
                table = Some(TableBuilder {
                    aligns: aligns.iter().map(align_of).collect(),
                    header: Vec::new(),
                    rows: Vec::new(),
                    row: Vec::new(),
                });
            }
            Event::Start(Tag::TableRow) => {
                if let Some(t) = table.as_mut() {
                    t.row = Vec::new();
                }
            }
            Event::Start(Tag::TableCell) => {
                slots.push(Vec::new());
            }
            Event::Start(Tag::HtmlBlock) => {
                let idx = doc.blocks.len();
                doc.blocks.push(Block::Html {
                    text: String::new(),
                });
                attach(&mut doc, &stack, idx);
                stack.push(idx);
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                hrefs.push(dest_url.to_string());
                img.push(false);
                slots.push(Vec::new());
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                hrefs.push(dest_url.to_string());
                img.push(true);
                slots.push(Vec::new());
            }
            Event::Start(Tag::Strong) => {
                slots.push(Vec::new());
            }
            Event::Start(Tag::Emphasis) => {
                slots.push(Vec::new());
            }
            Event::Start(Tag::Strikethrough) => {
                slots.push(Vec::new());
            }

            // ---------- inline leaves --------------------------------------
            Event::Text(t) => {
                let txt = t.into_string();
                if let Some(&i) = stack.last() {
                    if let Block::Code { text, .. } = doc.blocks.get_mut(i).expect("frame") {
                        text.push_str(&txt);
                    } else if let Some(slot) = slots.last_mut() {
                        slot.push(Inline::Text(txt));
                    }
                } else if let Some(slot) = slots.last_mut() {
                    slot.push(Inline::Text(txt));
                }
            }
            Event::Code(c) => {
                if let Some(slot) = slots.last_mut() {
                    slot.push(Inline::Code(c.to_string()));
                }
            }
            Event::InlineHtml(h) => {
                if let Some(slot) = slots.last_mut() {
                    slot.push(Inline::Text(format!("<{h}>")));
                }
            }
            Event::SoftBreak => {
                if let Some(slot) = slots.last_mut() {
                    slot.push(Inline::Break(false));
                }
            }
            Event::HardBreak => {
                if let Some(slot) = slots.last_mut() {
                    slot.push(Inline::Break(true));
                }
            }
            Event::TaskListMarker(checked) => {
                for &i in stack.iter().rev() {
                    if let Block::ListItem { task, .. } = doc.blocks.get_mut(i).expect("item") {
                        *task = Some(checked);
                        break;
                    }
                }
            }
            Event::Html(h) => {
                if stack.last().is_some_and(|i| {
                    matches!(doc.blocks.get(*i).expect("frame"), Block::Html { .. })
                }) {
                    if let Some(&i) = stack.last() {
                        if let Block::Html { text } = doc.blocks.get_mut(i).expect("frame") {
                            text.push_str(&h);
                        }
                    }
                }
            }
            Event::Rule => {
                let idx = doc.blocks.len();
                doc.blocks.push(Block::Rule);
                attach(&mut doc, &stack, idx);
            }
            Event::FootnoteReference(_) | Event::InlineMath(_) | Event::DisplayMath(_) => {}

            // ---------- ends -----------------------------------------------
            Event::End(TagEnd::Heading(_)) => {
                let slot = slots.pop().expect("heading slot");
                let idx = stack.pop().expect("heading frame");
                if let Block::Heading { inlines, .. } = &mut doc.blocks[idx] {
                    *inlines = slot;
                }
            }
            Event::End(TagEnd::Paragraph) => {
                let slot = slots.pop().expect("paragraph slot");
                let idx = stack.pop().expect("paragraph frame");
                if let Block::Paragraph { inlines, .. } = &mut doc.blocks[idx] {
                    *inlines = slot;
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                stack.pop();
            }
            Event::End(TagEnd::Item) => {
                let slot = slots.pop().expect("item slot");
                let idx = stack.pop().expect("item frame");
                if let Block::ListItem { inlines, .. } = &mut doc.blocks[idx] {
                    *inlines = slot;
                }
            }
            Event::End(TagEnd::List(_)) => {
                stack.pop();
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                stack.pop();
            }
            Event::End(TagEnd::Table) => {
                let t = table.take().expect("table");
                let idx = doc.blocks.len();
                doc.blocks.push(Block::Table {
                    aligns: t.aligns,
                    header: t.header,
                    rows: t.rows,
                });
                attach(&mut doc, &stack, idx);
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(t) = table.as_mut() {
                    t.header = std::mem::take(&mut t.row);
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(t) = table.as_mut() {
                    t.rows.push(std::mem::take(&mut t.row));
                }
            }
            Event::End(TagEnd::TableCell) => {
                let slot = slots.pop().expect("cell slot");
                if let Some(t) = table.as_mut() {
                    t.row.push(slot);
                }
            }
            Event::End(TagEnd::Link) => {
                let label = slots.pop().expect("link slot");
                let href = hrefs.pop().unwrap_or_default();
                let _ = img.pop();
                slots
                    .last_mut()
                    .expect("parent slot")
                    .push(Inline::Link { href, label });
            }
            Event::End(TagEnd::Image) => {
                let slot = slots.pop().expect("image slot");
                let src = hrefs.pop().unwrap_or_default();
                let _is_img = img.pop();
                slots.last_mut().expect("parent slot").push(Inline::Image {
                    src,
                    alt: flat_text(&slot),
                });
            }
            Event::End(TagEnd::HtmlBlock) => {
                stack.pop();
            }
            Event::End(TagEnd::Strong) => {
                let slot = slots.pop().expect("strong slot");
                slots
                    .last_mut()
                    .expect("parent slot")
                    .push(Inline::Strong(slot));
            }
            Event::End(TagEnd::Emphasis) => {
                let slot = slots.pop().expect("emph slot");
                slots
                    .last_mut()
                    .expect("parent slot")
                    .push(Inline::Emphasis(slot));
            }
            Event::End(TagEnd::Strikethrough) => {
                let slot = slots.pop().expect("strike slot");
                slots
                    .last_mut()
                    .expect("parent slot")
                    .push(Inline::Strikethrough(slot));
            }
            _ => {}
        }
    }

    doc
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn align_of(a: &Alignment) -> Align {
    match a {
        Alignment::Left => Align::Left,
        Alignment::Center => Align::Center,
        Alignment::Right => Align::Right,
        _ => Align::None,
    }
}

/// Attach a new block into the current container (if any), or into `root`.
fn attach(doc: &mut Doc, stack: &[usize], idx: usize) {
    if let Some(&top) = stack.last() {
        match doc.blocks.get_mut(top).expect("frame") {
            Block::Quote { children, .. } | Block::ListItem { children, .. } => {
                children.push(idx);
                return;
            }
            Block::List { items, .. } => {
                items.push(idx);
                return;
            }
            _ => {}
        }
    }
    doc.root.push(idx);
}

struct TableBuilder {
    aligns: Vec<Align>,
    header: Vec<Vec<Inline>>,
    rows: Vec<Vec<Vec<Inline>>>,
    row: Vec<Vec<Inline>>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regression_real_corpus_file() {
        let Some(p) = std::env::var_os("KB_GUI_REAL_FILE") else {
            return;
        };
        let s = std::fs::read_to_string(p).unwrap();
        let _ = parse(&s);
    }

    #[test]
    fn tight_list_item_carries_inlines() {
        // Tight list: inlines land directly under Item (no Paragraph wrapper),
        // including emphasis / code inside the item.
        let d = parse("- first item\n- an *emphasis* and `code`");
        assert_eq!(d.root.len(), 1);
        match &d.blocks[d.root[0]] {
            Block::List { items, .. } => {
                assert_eq!(items.len(), 2);
                match &d.blocks[items[1]] {
                    Block::ListItem { inlines, .. } => {
                        assert!(!inlines.is_empty(), "tight item inlines empty");
                        assert!(
                            inlines.iter().any(|i| matches!(i, Inline::Emphasis(_))),
                            "emphasis missing: {inlines:?}"
                        );
                    }
                    other => panic!("expected item, got {other:?}"),
                }
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn empty_md_is_empty() {
        let d = parse("");
        assert!(d.root.is_empty());
    }

    #[test]
    fn h1_is_a_heading_inline() {
        let d = parse("# hi  there");
        assert_eq!(d.root.len(), 1);
        match &d.blocks[d.root[0]] {
            Block::Heading { level, inlines } => {
                assert_eq!(*level, 1);
                assert!(flat_text(inlines).contains("hi"));
            }
            other => panic!("expected heading, got {other:?}"),
        }
    }

    #[test]
    fn paragraph_with_strong_and_link() {
        let d = parse("see **bold** [x](https://x) for more");
        assert_eq!(d.root.len(), 1);
        match &d.blocks[d.root[0]] {
            Block::Paragraph { inlines } => {
                let has_strong = inlines.iter().any(|i| {
                    matches!(
                        i,
                        Inline::Strong(v)
                            if v.iter().any(|x| matches!(x, Inline::Text(t) if t == "bold"))
                    )
                });
                let has_link = inlines.iter().any(|i| {
                    matches!(
                        i,
                        Inline::Link { label, .. }
                            if label.iter().any(|x| matches!(x, Inline::Text(t) if t == "x"))
                    )
                });
                assert!(has_strong, "strong not found in {inlines:?}");
                assert!(has_link, "link not found in {inlines:?}");
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn code_block() {
        let d = parse("```rust\nfn main() {}\n```");
        assert_eq!(d.root.len(), 1);
        match &d.blocks[d.root[0]] {
            Block::Code { lang, text } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert!(text.contains("fn main"), "text: {text:?}");
            }
            other => panic!("expected code block, got {other:?}"),
        }
    }

    #[test]
    fn task_list_items() {
        let d = parse("- [ ] to do\n- [x] done");
        assert_eq!(d.root.len(), 1);
        match &d.blocks[d.root[0]] {
            Block::List { ordered, items, .. } => {
                assert!(!ordered);
                assert_eq!(items.len(), 2);
                match &d.blocks[items[0]] {
                    Block::ListItem { task, .. } => assert_eq!(task, &Some(false)),
                    other => panic!("expected item, got {other:?}"),
                }
                match &d.blocks[items[1]] {
                    Block::ListItem { task, .. } => assert_eq!(task, &Some(true)),
                    other => panic!("expected item, got {other:?}"),
                }
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn blockquote_with_paragraph() {
        let d = parse("> quoted line");
        assert_eq!(d.root.len(), 1);
        match &d.blocks[d.root[0]] {
            Block::Quote { children } => {
                assert_eq!(children.len(), 1);
                match &d.blocks[children[0]] {
                    Block::Paragraph { inlines } => {
                        assert!(flat_text(inlines).contains("quoted"));
                    }
                    other => panic!("expected paragraph, got {other:?}"),
                }
            }
            other => panic!("expected blockquote, got {other:?}"),
        }
    }

    #[test]
    fn table_with_header_and_row() {
        let d = parse("| a | b |\n|---|---|\n| 1 | 2 |");
        assert_eq!(d.root.len(), 1);
        match &d.blocks[d.root[0]] {
            Block::Table { header, rows, .. } => {
                assert_eq!(header.len(), 2);
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].len(), 2);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn flat_text_joins_wrappers() {
        let v = vec![
            Inline::Text("a ".into()),
            Inline::Strong(vec![Inline::Text("b".into())]),
            Inline::Text(" c".into()),
        ];
        assert_eq!(flat_text(&v), "a b c");
    }

    // ======================================================================
    // Span resolution tests (G4 §8.2)
    // ======================================================================

    /// Convention: when a non-last line is the end of a block, the span
    /// **consumes** its trailing `\n` (so a single-line replacement keeps the
    /// following blank-line / section separator in place).
    #[test]
    fn span_h1_resolves_its_own_line() {
        let body = "# Top heading\n\ntext below";
        let d = parse(body);
        let h1 = &d.blocks[d.root[0]];
        let r = h1.range(body).expect("h1 span");
        assert_eq!(&body[r.clone()], "# Top heading\n");
    }

    #[test]
    fn span_h3_among_many() {
        let body = "# A\npara one\n\n### C\n\n# D";
        let d = parse(body);
        let h3 = &d.blocks[d.root[2]];
        let r = h3.range(body).expect("h3 span");
        assert_eq!(&body[r.clone()], "### C\n");
    }

    #[test]
    fn span_paragraph_single_line() {
        let body = "# H\njust a line\n\n## H2";
        let d = parse(body);
        let para = &d.blocks[d.root[1]];
        let r = para.range(body).expect("para span");
        assert_eq!(&body[r.clone()], "just a line\n");
    }

    #[test]
    fn span_paragraph_hard_break() {
        let body = "# H\nline one  \nline two\n\n# next";
        let d = parse(body);
        let para = &d.blocks[d.root[1]];
        let r = para.range(body).expect("para span");
        assert_eq!(&body[r.clone()], "line one  \nline two\n");
    }

    #[test]
    fn span_para_does_not_gobble_headings() {
        let body = "first\n# H\nsecond";
        let d = parse(body);
        let first = &d.blocks[d.root[0]];
        let r = first.range(body).expect("span");
        assert_eq!(&body[r.clone()], "first\n");
        // H itself gets its own line
        let h = &d.blocks[d.root[1]];
        let r2 = h.range(body).expect("span");
        assert_eq!(&body[r2.clone()], "# H\n");
    }

    #[test]
    fn span_para_ignoring_trivially_identical_headings() {
        // A H1 "Para" and a Paragraph "Para": they cannot be distinguished by
        // content alone. The H1 should resolve to the # line, the Paragraph to
        // its own line. If a user's paragraph is literally "# Para" with a
        // leading space, it still resolves to the plain line (the `starts_with`
        // + space check rejects the # line for the paragraph because the #
        // line's "rest" begins with the second #, not a space).
        let body = "# Para\n# Para\n\nPlain Para body";
        let d = parse(body);
        let h1_first = &d.blocks[d.root[0]];
        let r1 = h1_first.range(body).expect("h1 span");
        assert_eq!(&body[r1.clone()], "# Para\n");
        let h1_second = &d.blocks[d.root[1]];
        let r2 = h1_second.range(body).expect("h1b span");
        assert_eq!(&body[r2.clone()], "# Para\n");
        // The third block is a paragraph whose content is "Plain Para body".
        let para = &d.blocks[d.root[2]];
        let r3 = para.range(body).expect("para span");
        assert_eq!(&body[r3.clone()], "Plain Para body");
    }

    #[test]
    fn span_fenced_code_block_resolves_full_fence_plus_body() {
        let body = "# H\n```rust\nfn main() {\n    let x = 1;\n}\n```\n# H2";
        let d = parse(body);
        let code = &d.blocks[d.root[1]];
        let r = code.range(body).expect("code span");
        assert_eq!(
            &body[r.clone()],
            "```rust\nfn main() {\n    let x = 1;\n}\n```\n"
        );
    }

    #[test]
    fn span_fenced_code_block_ending_at_eof() {
        let body = "```rust\nlet x = 1;\n```";
        let d = parse(body);
        let code = &d.blocks[d.root[0]];
        let r = code.range(body).expect("code span");
        assert_eq!(&body[r.clone()], "```rust\nlet x = 1;\n```");
    }

    #[test]
    fn span_indented_code_block() {
        let body = "para\n\n    indented line one\n    indented line two\n\nnext";
        let d = parse(body);
        let code = &d.blocks[d.root[1]];
        let r = code.range(body).expect("code span");
        assert_eq!(
            &body[r.clone()],
            "    indented line one\n    indented line two\n"
        );
    }

    #[test]
    fn span_non_splicible_blocks_return_none() {
        let body = "para\n\n---\n\n> quote\n\n- item\n\n<table>\n  </table>";
        let d = parse(body);
        for &idx in &d.root {
            match &d.blocks[idx] {
                Block::Heading { .. } | Block::Paragraph { .. } | Block::Code { .. } => {}
                // Rule, Quote, List, Table, Html all return None.
                other => {
                    assert!(
                        other.range(body).is_none(),
                        "{other:?} unexpectedly resolved a span"
                    );
                }
            }
        }
    }

    #[test]
    fn span_paragraph_with_inline_code() {
        let body = "use `kb_core::frontmatter`;";
        let d = parse(body);
        let para = &d.blocks[d.root[0]];
        let r = para.range(body).expect("span");
        assert_eq!(&body[r.clone()], "use `kb_core::frontmatter`;");
    }
}
