use std::path::{Component, Path, PathBuf};

/// Collapse `.` segments so paths rendered in logs and errors don't end up
/// with leading `./` noise when `wavedash.toml`'s `upload_dir` already starts
/// with `./`. Display-only — does not touch the filesystem and does not
/// resolve `..` (paths we consume come from `config_dir.join(upload_dir)`, so
/// the `..` case is vanishingly rare and correctly passed through to the
/// filesystem calls that follow).
pub fn clean_path(p: &Path) -> PathBuf {
    let cleaned: PathBuf = p
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    if cleaned.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        cleaned
    }
}
