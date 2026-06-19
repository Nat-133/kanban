use std::path::PathBuf;

/// Replace `{task_id}` and `{repo}` placeholders. Missing repo -> empty string.
pub fn substitute(template: &str, task_id: &str, repo: Option<&str>) -> String {
    template.replace("{task_id}", task_id).replace("{repo}", repo.unwrap_or(""))
}

/// Expand `~/` to $HOME (only a leading `~/`).
pub(crate) fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    s.to_string()
}

/// Expand one `agents.baseDirs` entry into concrete existing directories.
/// Supports `~` expansion and a trailing `/*` (immediate subdirectories).
/// Returns only paths that exist and are directories.
pub fn expand_base_dir(entry: &str) -> Vec<PathBuf> {
    let expanded = expand_tilde(entry);
    if let Some(parent) = expanded.strip_suffix("/*") {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(parent) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() { out.push(p); }
            }
        }
        out
    } else {
        let p = PathBuf::from(&expanded);
        if p.is_dir() { vec![p] } else { Vec::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_task_id_and_repo() {
        assert_eq!(substitute(".kanban/sessions/{task_id}", "task-0001", Some("~/vcs/x")), ".kanban/sessions/task-0001");
        assert_eq!(substitute("{repo}", "task-0001", Some("/home/u/vcs/x")), "/home/u/vcs/x");
        assert_eq!(substitute("{repo}", "task-0001", None), "");
    }

    #[test]
    fn expands_base_dir_glob() {
        let dir = tempfile::tempdir().unwrap();
        let vcs = dir.path().join("vcs");
        std::fs::create_dir_all(vcs.join("repo-a")).unwrap();
        std::fs::create_dir_all(vcs.join("repo-b")).unwrap();
        std::fs::write(vcs.join("loose.txt"), "x").unwrap(); // not a dir -> excluded
        let pattern = format!("{}/*", vcs.display());
        let mut got = expand_base_dir(&pattern);
        got.sort();
        assert_eq!(got, vec![vcs.join("repo-a"), vcs.join("repo-b")]);
    }

    #[test]
    fn non_glob_base_dir_returns_itself_if_exists() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().to_path_buf();
        assert_eq!(expand_base_dir(&p.display().to_string()), vec![p]);
        assert!(expand_base_dir("/no/such/path/xyz").is_empty());
    }
}
