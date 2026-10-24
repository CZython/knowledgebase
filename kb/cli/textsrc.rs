use crate::*;

pub(crate) fn enumerate_non_recursive(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return out,
    };
    for entry in rd.flatten() {
        out.push(entry.path().to_string_lossy().to_string());
    }
    out.sort();
    out
}

