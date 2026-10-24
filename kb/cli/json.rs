use crate::*;

/// A single page record as a JSON object. Emits the *union* of the fields an
/// agent might want; the optional ones are omitted when absent so the output
/// stays honest (no `null` for a field that simply does not apply). Shared by
/// `browse`, `tag`, `info`, and `search` so the page shape is one thing.
pub(crate) struct PageJson {
    pub(crate) id:String,
    pub(crate) pt:String,
    pub(crate) title:Option<String>,
    pub(crate) abs:String,
    pub(crate) rel:String,
    pub(crate) tags:Vec<String>,
    pub(crate) gates:String,
    /// `None` = omit the field (e.g. `search` has no link count).
    pub(crate) links:Option<i64>,
    /// `None` = omit (non-label pages have no expansion).
    pub(crate) refs:Option<Vec<String>>,
    /// `None` = omit (`browse`/`tag` do not show the description; `info` does).
    pub(crate) description:Option<String>,
}


impl PageJson {
    pub(crate) fn render(&self) -> String {
        let mut o = String::with_capacity(256);
        o.push_str(&format!("\n  {{\n    \"id\": {}", jstr(&self.id)));
        o.push_str(&format!(",\n    \"type\": {}", jstr(&self.pt)));
        match &self.title {
            Some(t) if !t.is_empty() => o.push_str(&format!(",\n    \"title\": {}", jstr(t))),
            _ => {}
        }
        o.push_str(&format!(",\n    \"abs_path\": {}", jstr(&self.abs)));
        if !self.rel.is_empty() {
            o.push_str(&format!(",\n    \"rel_path\": {}", jstr(&self.rel)));
        }
        if let Some(d) = &self.description {
            if !d.is_empty() {
                o.push_str(&format!(",\n    \"description\": {}", jstr(d)));
            }
        }
        if !self.tags.is_empty() {
            let items: Vec<String> = self.tags.iter().map(|t| jstr(t)).collect();
            o.push_str(&format!(",\n    \"tags\": [{}]", items.join(",")));
        }
        if !self.gates.is_empty() {
            o.push_str(&format!(",\n    \"search_required_phrase\": {}", jstr(&self.gates)));
        }
        if let Some(n) = self.links {
            o.push_str(&format!(",\n    \"links\": {n}"));
        }
        if let Some(refs) = &self.refs {
            let items: Vec<String> = refs.iter().map(|r| jstr(r)).collect();
            o.push_str(&format!(",\n    \"refs\": [{}]", items.join(",")));
        }
        o.push_str("\n  }\n");
        o
    }
}

// ---- browse ----

