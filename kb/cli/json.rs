use crate::*;

/// The **single owner** of all `--json` document construction in the CLI body.
///
/// Two kinds of value are built here:
///
/// * `JObject` — an ordered `{ "k": v, … }`. Every *string* value goes through
///   the one `jstr` escaper; *nested* values (objects / arrays / numbers) are
///   passed in pre-rendered. Insertion order is preserved, so a test doing
///   `stdout.contains("\"id\": ")` sees the key in its documented position.
/// * the two array helpers — one owner of the empty case. `inline_array`
///   renders `["a","b"]` (or the canonical `[]`); `block_array` renders the
///   one-object-per-line form (or the canonical `[]`). Both collapse to the
///   **identical** `[]` when empty — so the `[ ]` / `[{}]` / `[]`
///   inconsistency (F6 / C3) can no longer be per-emitter.
///
/// The byte-level guarantees a test or agent may rely on:
///   * `inline_array(&[]) == "[]"` and `block_array(vec![]) == "[]"` — one owner.
///   * every string value is escaped by `jstr` once — a missed `\` or `\u0001`
///     can't exist in one site and not another, because the escaper is the only
///     thing that ever runs.
///   * `JObject` is `{}` when empty; keys are `{"k": v` in insertion order.
///
/// Not `serde_json` — we keep the no-new-runtime-dependency constraint. A
/// `JObject` is a `Vec<(&str, String)>` + one `render`.
#[derive(Debug)]
pub(crate) struct JObject {
    /// (key, already-rendered value), in insertion order.
    pairs: Vec<(&'static str, String)>,
}

impl JObject {
    pub(crate) fn new() -> Self {
        JObject { pairs: Vec::new() }
    }

    /// A **string** value — escaped through the single `jstr` owner.
    pub(crate) fn s(&mut self, k: &'static str, v: &str) {
        self.pairs.push((k, jstr(v)));
    }

    /// A **string-or-null** field — `null` (not the word `"null"`) when `None`,
    /// preserving `jstr_opt` semantics.
    pub(crate) fn opt(&mut self, k: &'static str, v: &Option<String>) {
        self.pairs.push((k, jstr_opt(v)));
    }

    /// An **integer** literal (no quotes).
    pub(crate) fn i(&mut self, k: &'static str, v: i64) {
        self.pairs.push((k, v.to_string()));
    }

    /// A **number** literal, one decimal place (e.g. the `mtime` seconds).
    pub(crate) fn n1(&mut self, k: &'static str, v: f64) {
        self.pairs.push((k, format!("{v:.1}")));
    }

    /// A **boolean** literal.
    pub(crate) fn b(&mut self, k: &'static str, v: bool) {
        self.pairs.push((k, jbool(v)));
    }

    /// A **pre-rendered** value — a nested `JObject`/array/number the caller
    /// rendered (or `null`). The caller is responsible for it being valid JSON.
    pub(crate) fn raw(&mut self, k: &'static str, v: String) {
        self.pairs.push((k, v));
    }

    /// `{"k":v,"k2":v2}` — **compact** one-line form, `{}` when empty. Used by
    /// `search` result rows, whose shape is pinned byte-for-byte in
    /// `m7_multifile` (the `\"id\":\"wiki:…\"` key:value with no space). The
    /// values are still escaped by the one `jstr` owner — only the layout
    /// differs from `render()`.
    pub(crate) fn render_compact(&self) -> String {
        if self.pairs.is_empty() {
            return "{}".to_string();
        }
        let body = self
            .pairs
            .iter()
            .map(|(k, v)| format!("\"{k}\":{v}"))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{{body}}}")
    }

    /// `{ "k": v, … }` pretty-printed: `{` first, every `key: value` indented
    /// two spaces, `}` back at column 0. `{}` when empty. Insertion order is
    /// preserved — a test doing `stdout.contains("\"id\": ")` sees the field
    /// exactly where it was pushed.
    pub(crate) fn render(&self) -> String {
        if self.pairs.is_empty() {
            return "{}".to_string();
        }
        let body = self
            .pairs
            .iter()
            .map(|(k, v)| format!("  \"{k}\": {v}"))
            .collect::<Vec<_>>()
            .join(",\n");
        format!("{{\n{body}\n}}")
    }
}

/// `[ "a","b" ]` → `["a","b"]` for a set of **string** values (each escaped by
/// the single `jstr` owner); the canonical empty list `[]` when empty.
pub(crate) fn inline_array(values: &[String]) -> String {
    if values.is_empty() {
        return "[]".to_string();
    }
    let inner = values
        .iter()
        .map(|v| jstr(v))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{inner}]")
}

/// One object-per-line form for a set of **pre-rendered** object/array items
/// (the whole-document result lists); the canonical empty list `[]` when empty.
pub(crate) fn block_array(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".to_string();
    }
    let body = items.iter().map(|it| format!("  {it}")).collect::<Vec<_>>().join(",\n");
    format!("[\n{body}\n]")
}

/// A **top-level result array** (stdout is the whole list): `[]` when empty,
/// otherwise the items joined with `,` — whitespace-agnostic by construction,
/// the exact contract the agent tests rely on (`json.loads` must see the list).
pub(crate) fn top_array(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".to_string();
    }
    format!("[{}]", items.join(","))
}

/// A single page record as a JSON object. Emits the *union* of the fields an
/// agent might want; the optional ones are omitted when absent so the output
/// stays honest (no `null` for a field that simply does not apply). Shared by
/// `browse`, `tag`, `info`, and `search` so the page shape is one thing.
pub(crate) struct PageJson {
    pub(crate) id: String,
    pub(crate) pt: String,
    pub(crate) title: Option<String>,
    pub(crate) abs: String,
    pub(crate) rel: String,
    pub(crate) tags: Vec<String>,
    pub(crate) gates: String,
    /// `None` = omit the field (e.g. `search` has no link count).
    pub(crate) links: Option<i64>,
    /// `None` = omit (non-label pages have no expansion).
    pub(crate) refs: Option<Vec<String>>,
    /// `None` = omit (`browse`/`tag` do not show the description; `info` does).
    pub(crate) description: Option<String>,
}

impl PageJson {
    pub(crate) fn render(&self) -> String {
        let mut o = JObject::new();
        o.s("id", &self.id);
        o.s("type", &self.pt);
        if !self.title.as_deref().unwrap_or("").is_empty() {
            o.s("title", self.title.as_deref().unwrap_or(""));
        }
        o.s("abs_path", &self.abs);
        if !self.rel.is_empty() {
            o.s("rel_path", &self.rel);
        }
        if let Some(d) = &self.description {
            if !d.is_empty() {
                o.s("description", d);
            }
        }
        if !self.tags.is_empty() {
            o.raw("tags", inline_array(&self.tags));
        }
        if !self.gates.is_empty() {
            o.s("search_required_phrase", &self.gates);
        }
        if let Some(n) = self.links {
            o.i("links", n);
        }
        if let Some(refs) = &self.refs {
            o.raw("refs", inline_array(refs));
        }
        o.render()
    }
}
