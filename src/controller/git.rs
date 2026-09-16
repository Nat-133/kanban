use std::path::Path;

/// Remove every git worktree directly under `dir`.
///
/// The agent creates its own worktree; nobody is left to remove it, because by
/// the time a card is archived the session is gone. A fixed location under the
/// sandbox is what lets the controller find one without the agent having
/// recorded anything.
///
/// A worktree holding uncommitted changes is left alone: `git worktree remove`
/// refuses it, and forcing past that would trade a stray directory for lost
/// work. Committed work is never at risk either way — removing a worktree does
/// not delete its branch.
pub fn prune_worktrees(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        // A linked worktree's `.git` is a file pointing at the real gitdir, so
        // this skips both ordinary directories and a repo the agent cloned.
        if !path.join(".git").is_file() {
            continue;
        }
        let out = std::process::Command::new("git")
            .current_dir(&path)
            .args(["worktree", "remove", "--", &path.display().to_string()])
            .output();
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => tracing::warn!(
                worktree = %path.display(),
                stderr = %String::from_utf8_lossy(&o.stderr).trim(),
                "leaving worktree in place",
            ),
            Err(e) => tracing::warn!(worktree = %path.display(), error = %e, "git worktree remove failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    fn git(cwd: &Path, args: &[&str]) {
        let out = Command::new("git").current_dir(cwd).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A repo with one commit, plus an empty `work/` beside it.
    fn repo_and_work() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("f"), "one").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);
        (dir, repo, work)
    }

    fn add_worktree(repo: &Path, work: &Path, branch: &str) -> PathBuf {
        let tree = work.join("repo");
        git(repo, &["worktree", "add", "-q", "-b", branch, &tree.display().to_string()]);
        tree
    }

    #[test]
    fn removes_a_clean_worktree() {
        let (_d, repo, work) = repo_and_work();
        let tree = add_worktree(&repo, &work, "task-0001-x");
        prune_worktrees(&work);
        assert!(!tree.exists(), "clean worktree should be removed");
        let out = std::process::Command::new("git")
            .current_dir(&repo).args(["worktree", "list"]).output().unwrap();
        let list = String::from_utf8_lossy(&out.stdout);
        assert!(!list.contains("task-0001-x"), "worktree should be deregistered: {list}");
    }

    /// Committed work survives archival even though the worktree does not: the
    /// branch is left behind, so the agent's commits are still reachable.
    #[test]
    fn keeps_the_branch_after_removing_the_worktree() {
        let (_d, repo, work) = repo_and_work();
        let tree = add_worktree(&repo, &work, "task-0001-x");
        std::fs::write(tree.join("f"), "two").unwrap();
        git(&tree, &["commit", "-qam", "work"]);
        prune_worktrees(&work);
        let out = std::process::Command::new("git")
            .current_dir(&repo).args(["rev-parse", "--verify", "task-0001-x"]).output().unwrap();
        assert!(out.status.success(), "branch must outlive its worktree");
    }

    #[test]
    fn leaves_a_worktree_with_uncommitted_work_in_place() {
        let (_d, repo, work) = repo_and_work();
        let tree = add_worktree(&repo, &work, "task-0001-x");
        std::fs::write(tree.join("f"), "edited, never committed").unwrap();
        prune_worktrees(&work);
        assert!(tree.join("f").exists(), "dirty worktree must not be destroyed");
        assert_eq!(std::fs::read_to_string(tree.join("f")).unwrap(), "edited, never committed");
    }

    /// Most tasks never take a worktree at all, and the sandbox is full of
    /// ordinary files; pruning must not touch them.
    #[test]
    fn ignores_plain_directories_and_files() {
        let (_d, _repo, work) = repo_and_work();
        std::fs::create_dir_all(work.join("scratch")).unwrap();
        std::fs::write(work.join("scratch/notes"), "x").unwrap();
        std::fs::write(work.join("handoff.md"), "x").unwrap();
        prune_worktrees(&work);
        assert!(work.join("scratch/notes").exists());
        assert!(work.join("handoff.md").exists());
    }
}
