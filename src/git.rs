//! Branch and repository root, read from `.git` without running git (§5).

use std::fs;
use std::path::{Component, Path, PathBuf};

/// Branch checked out in the repository containing `cwd`; `None` outside a
/// repository or when HEAD is detached. Follows worktree `.git` files.
pub fn branch(cwd: &Path) -> Option<String> {
    let found = find(cwd)?;
    let head = fs::read_to_string(found.git_dir.join("HEAD")).ok()?;
    let name = head.trim().strip_prefix("ref: refs/heads/")?;
    (!name.is_empty()).then(|| name.to_string())
}

/// Main working tree of the repository containing `cwd`. A worktree folds
/// into its main repo: follow the `.git` file to its `gitdir`, then that
/// gitdir's `commondir`; the main working tree is the common dir's parent.
/// `None` outside a repository.
pub fn repo_root(cwd: &Path) -> Option<PathBuf> {
    let found = find(cwd)?;
    if !found.linked {
        return Some(found.work_tree);
    }
    // No `commondir`: a submodule or an old-style worktree; its own repo.
    let Ok(common) = fs::read_to_string(found.git_dir.join("commondir")) else {
        return Some(found.work_tree);
    };
    let common = normalize(&found.git_dir.join(common.trim()));
    // A common dir not named `.git` is a bare repo: no main working tree.
    if common.file_name().is_some_and(|n| n == ".git")
        && let Some(main) = common.parent()
    {
        return Some(main.to_path_buf());
    }
    Some(found.work_tree)
}

/// Where the walk up from `cwd` found `.git`.
struct Found {
    /// The folder holding `.git`.
    work_tree: PathBuf,
    /// `.git` itself, or the target of a `.git` file.
    git_dir: PathBuf,
    /// `.git` is a file (`gitdir: <path>`): a worktree or submodule.
    linked: bool,
}

fn find(cwd: &Path) -> Option<Found> {
    let cwd = normalize(&std::path::absolute(cwd).ok()?);
    for dir in cwd.ancestors() {
        let dot_git = dir.join(".git");
        let Ok(meta) = fs::metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return Some(Found {
                work_tree: dir.to_path_buf(),
                git_dir: dot_git,
                linked: false,
            });
        }
        let text = fs::read_to_string(&dot_git).ok()?;
        let target = text.trim().strip_prefix("gitdir:")?.trim();
        return Some(Found {
            work_tree: dir.to_path_buf(),
            git_dir: normalize(&dir.join(target)),
            linked: true,
        });
    }
    None
}

/// Resolves `.` and `..` by text alone; symlinks are left as they are.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // `/..` is `/`.
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            part => out.push(part),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// A main repo at `<tmp>/main` on branch `main`.
    fn main_repo() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main");
        write(&main.join(".git/HEAD"), "ref: refs/heads/main\n");
        (tmp, main)
    }

    /// Adds a linked worktree `name` at `dir`, as `git worktree add` does.
    fn add_worktree(main: &Path, dir: &Path, name: &str, gitdir_line: &str) {
        let gitdir = main.join(".git/worktrees").join(name);
        write(&gitdir.join("HEAD"), &format!("ref: refs/heads/{name}\n"));
        write(&gitdir.join("commondir"), "../..\n");
        write(&dir.join(".git"), &format!("gitdir: {gitdir_line}\n"));
    }

    #[test]
    fn plain_repo_and_subdir() {
        let (_tmp, main) = main_repo();
        let sub = main.join("a/b");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(branch(&main).as_deref(), Some("main"));
        assert_eq!(branch(&sub).as_deref(), Some("main"));
        assert_eq!(repo_root(&main), Some(main.clone()));
        assert_eq!(repo_root(&sub), Some(main.clone()));
        assert_eq!(repo_root(&sub.join("../b/./")), Some(main));
    }

    #[test]
    fn detached_head() {
        let (_tmp, main) = main_repo();
        write(
            &main.join(".git/HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        );
        assert_eq!(branch(&main), None);
        assert_eq!(repo_root(&main), Some(main));
    }

    #[test]
    fn worktree_folds_into_main_repo() {
        let (_tmp, main) = main_repo();
        // Absolute gitdir, as git writes by default.
        let wt = main.join(".claude/worktrees/feat-x");
        let abs = main.join(".git/worktrees/feat-x");
        add_worktree(&main, &wt, "feat-x", abs.to_str().unwrap());
        let sub = wt.join("src");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(branch(&sub).as_deref(), Some("feat-x"));
        assert_eq!(repo_root(&wt), Some(main.clone()));
        assert_eq!(repo_root(&sub), Some(main.clone()));

        // Relative gitdir (`worktree.useRelativePaths`), outside the repo.
        let wt2 = main.parent().unwrap().join("main-wt2");
        add_worktree(&main, &wt2, "wt2", "../main/.git/worktrees/wt2");
        assert_eq!(branch(&wt2).as_deref(), Some("wt2"));
        assert_eq!(repo_root(&wt2), Some(main));
    }

    #[test]
    fn git_file_without_commondir() {
        // A submodule: `.git` file into the parent's `.git/modules`.
        let (_tmp, main) = main_repo();
        let sub = main.join("vendor/lib");
        write(&main.join(".git/modules/lib/HEAD"), "ref: refs/heads/dev\n");
        write(&sub.join(".git"), "gitdir: ../../.git/modules/lib\n");
        assert_eq!(branch(&sub).as_deref(), Some("dev"));
        assert_eq!(repo_root(&sub.join("x")), Some(sub));
    }

    #[test]
    fn worktree_of_bare_repo() {
        let tmp = TempDir::new().unwrap();
        let bare = tmp.path().join("repo.git");
        let wt = tmp.path().join("wt");
        let gitdir = bare.join("worktrees/wt");
        write(&gitdir.join("HEAD"), "ref: refs/heads/wt\n");
        write(&gitdir.join("commondir"), "../..\n");
        write(&wt.join(".git"), &format!("gitdir: {}\n", gitdir.display()));
        assert_eq!(branch(&wt).as_deref(), Some("wt"));
        assert_eq!(repo_root(&wt), Some(wt));
    }

    #[test]
    fn not_a_repository() {
        let tmp = TempDir::new().unwrap();
        // The temp dir could sit inside a repository; check only if not.
        if !tmp.path().ancestors().any(|d| d.join(".git").exists()) {
            assert_eq!(branch(tmp.path()), None);
            assert_eq!(repo_root(tmp.path()), None);
        }
        assert_eq!(repo_root(Path::new("/proc/self")), None);
    }

    #[test]
    fn bad_git_file() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join(".git"), "not a gitdir line\n");
        assert_eq!(branch(tmp.path()), None);
        assert_eq!(repo_root(tmp.path()), None);
    }

    #[test]
    fn normalizes_by_text() {
        let n = |p: &str| normalize(Path::new(p));
        assert_eq!(n("/a/b/../c/./d"), PathBuf::from("/a/c/d"));
        assert_eq!(n("/a/.git/worktrees/x/../.."), PathBuf::from("/a/.git"));
        assert_eq!(n("/../a"), PathBuf::from("/a"));
        assert_eq!(n("../a/../../b"), PathBuf::from("../../b"));
    }
}
