//! The index and commits: staging, discarding, committing.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::ErrorKind;
use std::path::Path;

use super::diff::file_diff_options;
use super::{
    CommitOptions, DiffOptions, Error, LineSelection, Repository, Result, force_checkout,
    git_available, lossy, sha_of, trim_line_end,
};
use crate::types::{FileChange, FileStatus, LineOrigin, Sha};

impl Repository {
    /// Stages the files as they are on disk; a file that is gone is staged as a deletion.
    pub fn stage<S: AsRef<str>>(&self, paths: &[S]) -> Result<()> {
        let mut index = self.repo.index()?;
        for path in paths {
            let path = path.as_ref();
            // symlink_metadata: a dangling symlink is still a file to stage.
            match std::fs::symlink_metadata(self.workdir.join(path)) {
                Ok(_) => index.add_path(Path::new(path))?,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    index.remove_path(Path::new(path))?;
                }
                Err(source) => {
                    return Err(Error::Io {
                        path: path.to_owned(),
                        source,
                    });
                }
            }
        }
        index.write()?;
        Ok(())
    }

    /// Stages every change of the working directory: new files, modifications and deletions.
    pub fn stage_all(&self) -> Result<()> {
        let mut index = self.repo.index()?;
        let everything = std::iter::empty::<&str>;
        index.add_all(everything(), git2::IndexAddOption::DEFAULT, None)?;
        index.update_all(everything(), None)?;
        index.write()?;
        Ok(())
    }

    /// Puts each path back to HEAD's entry in the index, or takes it out when HEAD does not have it. The
    /// working directory is not touched.
    pub fn unstage<S: AsRef<str>>(&self, paths: &[S]) -> Result<()> {
        let mut index = self.repo.index()?;
        let Some(tree) = self.head_tree()? else {
            for path in paths {
                let _ = index.remove_path(Path::new(path.as_ref()));
            }
            index.write()?;
            return Ok(());
        };
        // By exact path: libgit2's reset reads the paths as wildmatch patterns ([ ] * ?).
        for path in paths {
            let path = path.as_ref();
            ignore_not_found(index.conflict_remove(Path::new(path)))?;
            match tree.get_path(Path::new(path)) {
                Ok(entry) => index.add(&git2::IndexEntry {
                    ctime: git2::IndexTime::new(0, 0),
                    mtime: git2::IndexTime::new(0, 0),
                    dev: 0,
                    ino: 0,
                    mode: u32::try_from(entry.filemode()).unwrap_or_default(),
                    uid: 0,
                    gid: 0,
                    file_size: 0,
                    id: entry.id(),
                    flags: 0,
                    flags_extended: 0,
                    path: path.as_bytes().to_vec(),
                })?,
                Err(error) if error.code() == git2::ErrorCode::NotFound => {
                    ignore_not_found(index.remove_path(Path::new(path)))?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        index.write()?;
        Ok(())
    }

    /// Unstages everything that is staged (both names of a staged rename).
    pub fn unstage_all(&self) -> Result<()> {
        let status = self.load_status()?;
        let paths: Vec<&str> = status
            .staged
            .iter()
            .flat_map(|change| std::iter::once(&change.path).chain(&change.old_path))
            .map(String::as_str)
            .collect();
        if paths.is_empty() {
            return Ok(());
        }
        self.unstage(&paths)
    }

    /// Throws away unstaged changes: untracked files are deleted, the others are written again from the index.
    /// A nested git repository (an untracked folder with its own `.git`) is never deleted: everything else is
    /// discarded, then the call fails naming what was left alone.
    pub fn discard(&self, changes: &[FileChange]) -> Result<()> {
        let index = self.repo.index()?;
        let mut tracked: Vec<&str> = Vec::new();
        let mut kept: Vec<String> = Vec::new();
        for change in changes {
            if change.status == FileStatus::Untracked {
                remove_untracked(
                    &self.workdir,
                    Path::new(change.path.trim_end_matches('/')),
                    &mut kept,
                );
                remove_empty_parents(&self.workdir, Path::new(change.path.trim_end_matches('/')));
            } else if let Some(old_path) = &change.old_path
                && *old_path != change.path
                && index.get_path(Path::new(&change.path), 0).is_none()
            {
                // An unstaged rename: the new name is not in the index, so it goes and the old name comes back.
                let _ = std::fs::remove_file(self.workdir.join(&change.path));
                tracked.push(old_path);
            } else {
                tracked.push(&change.path);
            }
        }
        if !tracked.is_empty() {
            // The paths are literal, not patterns: [ ] * ? may be part of a file name.
            let mut checkout = force_checkout();
            checkout.recreate_missing(true).disable_pathspec_match(true);
            for path in tracked {
                checkout.path(path);
            }
            self.repo.checkout_index(None, Some(&mut checkout))?;
        }
        if kept.is_empty() {
            Ok(())
        } else {
            Err(Error::message(format!(
                "not deleted, a nested git repository: {}",
                kept.join(", ")
            )))
        }
    }

    /// Reset All: staged changes too, new files included (everything is unstaged first, then discarded).
    pub fn discard_all(&self) -> Result<()> {
        self.unstage_all()?;
        let status = self.load_status()?;
        self.discard(&status.unstaged)
    }

    /// Commits the index on top of `parents` and moves HEAD's branch there. `author` (name, email and date)
    /// when it is not the committer: the original author of a commit replayed.
    pub(super) fn commit_index(
        &self,
        message: &str,
        parents: &[git2::Oid],
        author: Option<&git2::Signature<'_>>,
    ) -> Result<git2::Oid> {
        let repo = &self.repo;
        let tree = repo.find_tree(repo.index()?.write_tree()?)?;
        let committer = self.default_signature()?;
        let parents = parents
            .iter()
            .map(|&parent| repo.find_commit(parent))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let parents: Vec<&git2::Commit<'_>> = parents.iter().collect();
        Ok(repo.commit(
            Some("HEAD"),
            author.unwrap_or(&committer),
            &committer,
            message,
            &tree,
            &parents,
        )?)
    }

    /// Commits the index; `amend` replaces HEAD's commit instead (its tree and message, keeping its author and
    /// parents). In the middle of a merge the commit gets the merged heads as further parents, and the commit
    /// concludes the merge, cherry-pick or revert in progress (a cherry-pick's commit keeps the picked
    /// commit's author). No hook runs: see [`Self::commit_with_git`].
    pub fn commit(&self, message: &str, amend: bool) -> Result<Sha> {
        let repo = &self.repo;
        let head = self.head_commit()?;
        if amend {
            let head = head.ok_or_else(|| Error::message("nothing to amend"))?;
            let tree = repo.find_tree(repo.index()?.write_tree()?)?;
            let committer = self.default_signature()?;
            let id = head.amend(
                Some("HEAD"),
                None,
                Some(&committer),
                None,
                Some(message),
                Some(&tree),
            )?;
            return Ok(sha_of(id));
        }
        let mut parents: Vec<git2::Oid> = head.iter().map(git2::Commit::id).collect();
        if repo.state() == git2::RepositoryState::Merge {
            // MERGE_HEAD as the second parent.
            parents.extend(self.merge_heads());
        }
        // A conflicted cherry-pick keeps the picked commit's author, as `git cherry-pick --continue` does.
        let picked = matches!(
            repo.state(),
            git2::RepositoryState::CherryPick | git2::RepositoryState::CherryPickSequence
        )
        .then(|| repo.refname_to_id("CHERRY_PICK_HEAD").ok())
        .flatten()
        .and_then(|id| repo.find_commit(id).ok());
        let id = self.commit_index(
            message,
            &parents,
            picked.as_ref().map(git2::Commit::author).as_ref(),
        )?;
        self.finish_operation_state();
        Ok(sha_of(id))
    }

    /// The same through `git commit -F -`: hooks run (unless skipped) and the commit can be GPG-signed. The
    /// author is the one [`Self::commit`] would use (the profile's identity in repositories without their
    /// own).
    pub fn commit_with_git(&self, message: &str, options: &CommitOptions) -> Result<Sha> {
        if !git_available() {
            return Err(Error::message("git was not found on PATH"));
        }
        let identity = self.default_signature()?;
        let mut args = vec![
            "-c".to_owned(),
            format!("user.name={}", lossy(identity.name_bytes())),
            "-c".to_owned(),
            format!("user.email={}", lossy(identity.email_bytes())),
            "commit".to_owned(),
            "-F".to_owned(),
            "-".to_owned(),
        ];
        let mut flag = |flag: &str| args.push(flag.to_owned());
        if options.amend {
            flag("--amend");
        }
        flag(if options.sign { "-S" } else { "--no-gpg-sign" });
        if options.skip_hooks {
            flag("--no-verify");
        }
        if options.strip_comments {
            flag("--cleanup=strip");
        }
        let output = self.git(&args, message);
        self.refresh_index();
        if !output.success() {
            let text = trim_line_end(&output.text);
            return Err(Error::Cli {
                output: if text.is_empty() {
                    "git commit failed".to_owned()
                } else {
                    text.to_owned()
                },
            });
        }
        Ok(sha_of(self.repo.refname_to_id("HEAD")?))
    }

    /// Whether [`Self::commit`] would skip something `git commit` does: a pre-commit / prepare-commit-msg /
    /// commit-msg / post-commit hook in the hooks directory (`core.hooksPath` honoured).
    #[must_use]
    pub fn has_commit_hooks(&self) -> bool {
        let hooks = match self.config_string("core.hooksPath") {
            Some(configured) => self.workdir.join(configured),
            None => self.repo.commondir().join("hooks"),
        };
        [
            "pre-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-commit",
        ]
        .iter()
        .any(|name| hooks.join(name).is_file())
    }

    /// Stages (or, `unstage`, unstages) only the given changed lines of `path`: a patch of them is applied to
    /// the index with `git apply --cached`. Unselected deletions stay, unselected additions are left out.
    pub fn apply_lines(&self, path: &str, lines: &[LineSelection], unstage: bool) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        if !git_available() {
            return Err(Error::message("git was not found on PATH"));
        }
        // The diff the lines were picked from, with git's 3 lines of context: HEAD -> index to unstage, index
        // -> working directory to stage.
        let mut options = file_diff_options(path, DiffOptions::default());
        options
            .include_untracked(true)
            .show_untracked_content(true)
            .recurse_untracked_dirs(true);
        let diff = self.workdir_diff(unstage, &mut options)?;
        if diff.deltas().len() == 0 {
            return Err(Error::message("the file has no changes left"));
        }
        let patch = git2::Patch::from_diff(&diff, 0)?
            .ok_or_else(|| Error::message("could not build patch"))?;
        let delta = patch.delta();
        if delta.flags().contains(git2::DiffFlags::BINARY) {
            return Err(Error::message("lines of a binary file cannot be staged"));
        }
        let selected = |origin: LineOrigin, lineno: Option<u32>| -> bool {
            lineno.is_some_and(|lineno| lines.contains(&LineSelection { origin, lineno }))
        };

        // The hunks with their changed lines told apart: picked or not.
        let mut hunks: Vec<(git2::DiffHunk<'_>, Vec<PatchLine<'_>>)> = Vec::new();
        for hunk_index in 0..patch.num_hunks() {
            let (hunk, line_count) = patch.hunk(hunk_index)?;
            let mut hunk_lines = Vec::with_capacity(line_count);
            for line_index in 0..line_count {
                let line = patch.line_in_hunk(hunk_index, line_index)?;
                let (origin, picked) = match line.origin_value() {
                    git2::DiffLineType::Context => (LineOrigin::Context, false),
                    git2::DiffLineType::Addition => (
                        LineOrigin::Added,
                        selected(LineOrigin::Added, line.new_lineno()),
                    ),
                    git2::DiffLineType::Deletion => (
                        LineOrigin::Deleted,
                        selected(LineOrigin::Deleted, line.old_lineno()),
                    ),
                    _ => continue, // the end-of-file newline markers: PatchLine::write adds them
                };
                hunk_lines.push(PatchLine {
                    origin,
                    picked,
                    content: Cow::Borrowed(line.content()),
                });
            }
            hunks.push((hunk, hunk_lines));
        }
        if let Some((_, last_hunk)) = hunks.last_mut() {
            if unstage {
                keep_unterminated_last_line_in_index(last_hunk);
            } else {
                complete_unterminated_last_line(last_hunk);
            }
        }

        // Every change picked: the whole file (new, deleted and mode changes included) the usual way.
        let all = hunks
            .iter()
            .flat_map(|(_, hunk_lines)| hunk_lines)
            .all(|line| line.origin == LineOrigin::Context || line.picked);
        if all {
            return if unstage {
                self.unstage(&[path])
            } else {
                self.stage(&[path])
            };
        }

        // Staging applies the patch to the index as is (its old side is the index); unstaging applies it in
        // reverse (its new side is the index). Each hunk keeps that side's start and derives the other one.
        let mut body: Vec<u8> = Vec::new();
        let mut offset = 0_i64; // the new side's start minus the old side's, from the hunks before
        for (hunk, hunk_lines) in &hunks {
            let mut text: Vec<u8> = Vec::new();
            let (mut old_count, mut new_count) = (0_i64, 0_i64);
            let mut changes = false;
            for line in hunk_lines {
                let origin = match (line.origin, line.picked) {
                    (LineOrigin::Context, _) => LineOrigin::Context,
                    (origin, true) => {
                        changes = true;
                        origin
                    }
                    // Not picked: an addition stays in the index when unstaging, a deletion when staging.
                    (LineOrigin::Added, false) if unstage => LineOrigin::Context,
                    (LineOrigin::Deleted, false) if !unstage => LineOrigin::Context,
                    _ => continue,
                };
                line.write(origin, &mut text);
                old_count += i64::from(origin != LineOrigin::Added);
                new_count += i64::from(origin != LineOrigin::Deleted);
            }
            if !changes {
                continue;
            }
            // A side with no lines starts at the line before (git's convention), one with lines at its first.
            let anchor_start = i64::from(if unstage {
                hunk.new_start()
            } else {
                hunk.old_start()
            });
            let (anchor_count, other_count) = if unstage {
                (new_count, old_count)
            } else {
                (old_count, new_count)
            };
            let mut other_start = if unstage {
                anchor_start - offset
            } else {
                anchor_start + offset
            };
            if anchor_count == 0 && other_count > 0 {
                other_start += 1;
            } else if anchor_count > 0 && other_count == 0 {
                other_start -= 1;
            }
            let (old_start, new_start) = if unstage {
                (other_start, anchor_start)
            } else {
                (anchor_start, other_start)
            };
            body.extend_from_slice(
                format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@\n").as_bytes(),
            );
            body.extend_from_slice(&text);
            offset += new_count - old_count;
        }
        if body.is_empty() {
            return Ok(());
        }

        // A new file keeps its "new file" header when staged in part, and a staged deletion its "deleted file"
        // header when unstaged in part (applied in reverse it creates the file in the index); a partly staged
        // deletion is a plain change.
        let mut patch_text = format!("diff --git a/{path} b/{path}\n");
        let created =
            !unstage && matches!(delta.status(), git2::Delta::Untracked | git2::Delta::Added);
        let deleted = unstage && delta.status() == git2::Delta::Deleted;
        if created {
            let mode = u32::from(delta.new_file().mode());
            let _ = writeln!(patch_text, "new file mode {mode:o}\n--- /dev/null");
        } else if deleted {
            let mode = u32::from(delta.old_file().mode());
            let _ = writeln!(patch_text, "deleted file mode {mode:o}\n--- a/{path}");
        } else {
            let _ = writeln!(patch_text, "--- a/{path}");
        }
        let _ = writeln!(
            patch_text,
            "+++ {}",
            if deleted {
                "/dev/null".to_owned()
            } else {
                format!("b/{path}")
            }
        );
        let mut patch = patch_text.into_bytes();
        patch.extend_from_slice(&body);

        let mut args = vec!["apply", "--cached", "--whitespace=nowarn"];
        if unstage {
            args.push("--reverse");
        }
        args.push("-");
        // Bytes: the lines are not necessarily UTF-8, and must reach the index as they are.
        let output = self.git_bytes(args, &patch);
        self.refresh_index();
        if !output.success() {
            return Err(Error::cli(&output, "git apply failed"));
        }
        Ok(())
    }

    /// Changes only HEAD's message, keeping its tree.
    pub fn reword_head(&self, message: &str) -> Result<Sha> {
        let head = self
            .head_commit()?
            .ok_or_else(|| Error::message("no commit to reword"))?;
        let committer = self.default_signature()?;
        let id = head.amend(
            Some("HEAD"),
            None,
            Some(&committer),
            None,
            Some(message),
            None,
        )?;
        Ok(sha_of(id))
    }
}

/// A line of the diff a partial patch is built from.
struct PatchLine<'a> {
    origin: LineOrigin,
    /// A changed line the selection holds.
    picked: bool,
    /// With its line ending, when it has one.
    content: Cow<'a, [u8]>,
}

impl PatchLine<'_> {
    /// Appends the line to a patch as an `origin` line.
    fn write(&self, origin: LineOrigin, patch: &mut Vec<u8>) {
        patch.extend_from_slice(origin.as_char().encode_utf8(&mut [0; 4]).as_bytes());
        patch.extend_from_slice(&self.content);
        if !self.content.ends_with(b"\n") {
            patch.extend_from_slice(b"\n\\ No newline at end of file\n");
        }
    }
}

/// Staging only some lines of a file whose last line has no line ending: that line, rewritten with one, shows
/// as a deletion and an addition of the same text, and a line added after it needs the ending. Applied
/// without it, the new line would be glued to the old one (`b` + `c` is `bc`). So when an addition after the
/// unpicked deletion is picked, the deletion is picked too, with the addition that re-adds the line (made up
/// when the diff has none), and the file ends up with the line ending the picked lines need.
fn complete_unterminated_last_line(hunk_lines: &mut Vec<PatchLine<'_>>) {
    let Some(deletion) = hunk_lines.iter().rposition(|line| {
        line.origin == LineOrigin::Deleted && !line.picked && !line.content.ends_with(b"\n")
    }) else {
        return;
    };
    let after = &hunk_lines[deletion + 1..];
    // Not the last line of the old file: its ending is not in question.
    if after.iter().any(|line| line.origin == LineOrigin::Context)
        || !after
            .iter()
            .any(|line| line.origin == LineOrigin::Added && line.picked)
    {
        return;
    }
    let mut terminated = hunk_lines[deletion].content.to_vec();
    terminated.push(b'\n');
    hunk_lines[deletion].picked = true;
    match hunk_lines[deletion + 1..]
        .iter_mut()
        .find(|line| line.origin == LineOrigin::Added && *line.content == *terminated)
    {
        Some(twin) => twin.picked = true,
        None => hunk_lines.insert(
            deletion + 1,
            PatchLine {
                origin: LineOrigin::Added,
                picked: true,
                content: Cow::Owned(terminated),
            },
        ),
    }
}

/// Unstaging only some lines of a file whose last line has no line ending in HEAD: the index has it rewritten
/// with one (a deletion and an addition of the same text) plus lines after it. Putting the old line back
/// unterminated is only possible when nothing stays after it, or the next line would be glued to it (`b` + `c`
/// is `bc`, and a re-added twin would be duplicated). So when the deletion is picked but an addition after
/// it stays in the index (the twin included), the deletion is dropped from the selection, with the twin
/// when that is picked: the index keeps the line with its ending.
fn keep_unterminated_last_line_in_index(hunk_lines: &mut [PatchLine<'_>]) {
    let Some(deletion) = hunk_lines.iter().rposition(|line| {
        line.origin == LineOrigin::Deleted && line.picked && !line.content.ends_with(b"\n")
    }) else {
        return;
    };
    let after = &hunk_lines[deletion + 1..];
    if after
        .iter()
        .all(|line| line.origin == LineOrigin::Added && line.picked)
    {
        return;
    }
    let mut terminated = hunk_lines[deletion].content.to_vec();
    terminated.push(b'\n');
    hunk_lines[deletion].picked = false;
    if let Some(twin) = hunk_lines[deletion + 1..]
        .iter_mut()
        .find(|line| line.origin == LineOrigin::Added && *line.content == *terminated)
    {
        twin.picked = false;
    }
}

fn ignore_not_found(result: std::result::Result<(), git2::Error>) -> Result<()> {
    match result {
        Err(error) if error.code() != git2::ErrorCode::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// Whether `dir` or a folder below it has a `.git` entry (a repository of its own, or a worktree's link).
fn holds_repository(dir: &Path) -> bool {
    if dir.join(".git").symlink_metadata().is_ok() {
        return true;
    }
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir()) && holds_repository(&entry.path())
        })
    })
}

/// Removes the folders above `relative` (to `workdir`) that are empty now, from the nearest up to, not
/// including, `workdir`. A folder that still holds anything (a nested repository included) stays, and so do
/// the ones above it.
fn remove_empty_parents(workdir: &Path, relative: &Path) {
    let mut folder = relative.parent();
    while let Some(parent) = folder.filter(|parent| !parent.as_os_str().is_empty()) {
        // remove_dir fails for a folder that is not empty.
        if std::fs::remove_dir(workdir.join(parent)).is_err() {
            break;
        }
        folder = parent.parent();
    }
}

/// Deletes the untracked file or folder `relative` (to `workdir`); nothing happens when it cannot be
/// deleted. A folder that is a repository, or holds one, is not deleted as a whole: what is not a repository
/// goes, the repositories stay with the folders leading to them, and their paths are added to `kept`.
fn remove_untracked(workdir: &Path, relative: &Path, kept: &mut Vec<String>) {
    let path = workdir.join(relative);
    let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir());
    if !is_dir {
        let _ = std::fs::remove_file(&path);
    } else if path.join(".git").symlink_metadata().is_ok() {
        kept.push(relative.to_string_lossy().replace('\\', "/"));
    } else if !holds_repository(&path) {
        let _ = std::fs::remove_dir_all(&path);
    } else if let Ok(entries) = std::fs::read_dir(&path) {
        for entry in entries.flatten() {
            remove_untracked(workdir, &relative.join(entry.file_name()), kept);
        }
        // Gone unless a repository is inside.
        let _ = std::fs::remove_dir(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn repository() -> (tempfile::TempDir, Repository) {
        let scratch = tempfile::tempdir().unwrap();
        let repo = Repository::init(scratch.path()).unwrap();
        repo.set_local_config("user.name", Some("Index test"))
            .unwrap();
        repo.set_local_config("user.email", Some("index@example.invalid"))
            .unwrap();
        repo.set_local_config("core.autocrlf", Some("false"))
            .unwrap();
        (scratch, repo)
    }

    fn write(repo: &Repository, path: &str, bytes: &[u8]) {
        fs::write(repo.workdir().join(path), bytes).unwrap();
    }

    fn stage_commit(repo: &Repository, path: &str, bytes: &[u8]) -> Sha {
        write(repo, path, bytes);
        repo.stage(&[path]).unwrap();
        repo.commit("Base", false).unwrap()
    }

    fn staged(repo: &Repository, path: &str) -> Option<Vec<u8>> {
        let entry = repo.repo.index().unwrap().get_path(Path::new(path), 0)?;
        Some(repo.repo.find_blob(entry.id).unwrap().content().to_vec())
    }

    const fn added(lineno: u32) -> LineSelection {
        LineSelection {
            origin: LineOrigin::Added,
            lineno,
        }
    }

    const fn deleted(lineno: u32) -> LineSelection {
        LineSelection {
            origin: LineOrigin::Deleted,
            lineno,
        }
    }

    #[test]
    fn discarding_all_restores_tracked_entries_and_keeps_nested_repositories() {
        let (_scratch, repo) = repository();
        let root = repo.workdir();
        let head = stage_commit(&repo, "original.txt", b"HEAD\n");
        fs::rename(root.join("original.txt"), root.join("renamed.txt")).unwrap();
        fs::create_dir(root.join("mixed")).unwrap();
        let nested = Repository::init(root.join("mixed").join("nested")).unwrap();
        write(&nested, "keep.txt", b"Nested work\n");
        write(&repo, "mixed/loose.txt", b"Remove this\n");
        write(&repo, "added.txt", b"New file\n");
        repo.stage(&["original.txt", "renamed.txt", "added.txt"])
            .unwrap();

        repo.unstage_all().unwrap();
        let status = repo.load_status().unwrap();
        assert!(status.staged.is_empty());
        assert_eq!(
            staged(&repo, "original.txt").as_deref(),
            Some(b"HEAD\n".as_slice())
        );
        assert!(staged(&repo, "renamed.txt").is_none());
        assert!(root.join("renamed.txt").is_file());
        assert!(!root.join("original.txt").exists());

        repo.stage(&["original.txt", "renamed.txt", "added.txt"])
            .unwrap();
        let remaining = repo.discard_all().unwrap_err().to_string();
        assert!(remaining.contains("mixed/nested"), "{remaining}");
        assert_eq!(
            repo.load_history(&Default::default())
                .unwrap()
                .head_sha
                .as_deref(),
            Some(head.as_str())
        );
        assert_eq!(fs::read(root.join("original.txt")).unwrap(), b"HEAD\n");
        assert!(!root.join("renamed.txt").exists());
        assert!(!root.join("added.txt").exists());
        assert!(!root.join("mixed").join("loose.txt").exists());
        assert_eq!(
            fs::read(nested.workdir().join("keep.txt")).unwrap(),
            b"Nested work\n"
        );
        assert!(repo.load_status().unwrap().staged.is_empty());
    }

    #[test]
    fn an_unborn_index_can_be_unstaged_and_cannot_be_amended() {
        let (_scratch, repo) = repository();
        assert!(
            repo.commit("Amend", true)
                .unwrap_err()
                .to_string()
                .contains("nothing to amend")
        );
        write(&repo, "new.txt", b"Initial\n");
        repo.stage(&["new.txt"]).unwrap();
        assert!(staged(&repo, "new.txt").is_some());
        repo.unstage_all().unwrap();
        repo.unstage_all().unwrap();
        assert!(staged(&repo, "new.txt").is_none());
        assert_eq!(
            fs::read(repo.workdir().join("new.txt")).unwrap(),
            b"Initial\n"
        );
    }

    #[test]
    fn line_staging_and_unstaging_preserve_the_final_unterminated_line() {
        let (_scratch, repo) = repository();
        let file = "partial.txt";
        stage_commit(&repo, file, b"a\nb");
        write(&repo, file, b"a\nx\ny\n");
        // The index keeps 'b' with a newly added separator before the selected 'y'.
        repo.apply_lines(file, &[added(3)], false).unwrap();
        assert_eq!(
            staged(&repo, file).as_deref(),
            Some(b"a\nb\ny\n".as_slice())
        );
        assert_eq!(fs::read(repo.workdir().join(file)).unwrap(), b"a\nx\ny\n");

        // Unstaging the original deletion and its terminated replacement while
        // 'y' stays must keep the separator in the index.
        repo.apply_lines(file, &[deleted(2), added(2)], true)
            .unwrap();
        assert_eq!(
            staged(&repo, file).as_deref(),
            Some(b"a\nb\ny\n".as_slice())
        );
        // With no other selected lines, a stale selection is a no-op.
        repo.apply_lines(file, &[added(50)], true).unwrap();
        assert_eq!(
            staged(&repo, file).as_deref(),
            Some(b"a\nb\ny\n".as_slice())
        );

        repo.apply_lines(file, &[deleted(2), added(2), added(3)], true)
            .unwrap();
        assert_eq!(staged(&repo, file).as_deref(), Some(b"a\nb".as_slice()));
        assert_eq!(fs::read(repo.workdir().join(file)).unwrap(), b"a\nx\ny\n");
        let stale = repo
            .apply_lines(file, &[added(3)], true)
            .unwrap_err()
            .to_string();
        assert!(stale.contains("no changes left"), "{stale}");
    }

    #[test]
    fn unstaging_one_line_of_a_staged_deletion_recreates_only_that_line_in_the_index() {
        let (_scratch, repo) = repository();
        let file = "deleted.txt";
        stage_commit(&repo, file, b"one\ntwo\nthree\n");
        fs::remove_file(repo.workdir().join(file)).unwrap();
        repo.stage(&[file]).unwrap();
        assert!(staged(&repo, file).is_none());

        repo.apply_lines(file, &[deleted(2)], true).unwrap();
        assert_eq!(staged(&repo, file).as_deref(), Some(b"two\n".as_slice()));
        assert!(!repo.workdir().join(file).exists());
        assert_eq!(
            repo.load_status().unwrap().staged[0].status,
            FileStatus::Modified
        );
        repo.unstage_all().unwrap();
        assert_eq!(
            staged(&repo, file).as_deref(),
            Some(b"one\ntwo\nthree\n".as_slice())
        );
        assert!(repo.load_status().unwrap().staged.is_empty());
    }

    #[test]
    fn native_amend_updates_the_tree_and_message_while_keeping_the_existing_parent_and_author() {
        let (_scratch, repo) = repository();
        let file = "tracked.txt";
        let parent = stage_commit(&repo, file, b"Parent\n");
        write(&repo, file, b"Original tip\n");
        repo.stage(&[file]).unwrap();
        let old = repo.commit("Old tip", false).unwrap();
        let author = repo.load_commit(&old).unwrap().author;
        repo.set_local_config("user.name", Some("Different committer"))
            .unwrap();
        write(&repo, file, b"Amended tip\n");
        repo.stage(&[file]).unwrap();
        let amended = repo.commit("New tip", true).unwrap();
        let replacement = repo.load_commit(&amended).unwrap();
        assert_ne!(old, amended);
        assert_eq!(replacement.parents, [parent]);
        assert_eq!(replacement.author, author);
        assert_eq!(replacement.summary, "New tip");
        assert_eq!(
            staged(&repo, file).as_deref(),
            Some(b"Amended tip\n".as_slice())
        );
        assert!(repo.load_status().unwrap().is_empty());
    }

    #[test]
    fn git_commit_uses_the_configured_hooks_and_can_amend_with_hooks_skipped() {
        let (_scratch, repo) = repository();
        let file = "tracked.txt";
        let base = stage_commit(&repo, file, b"Before\n");
        let hooks = repo.git_dir().join("configured-hooks");
        fs::create_dir(&hooks).unwrap();
        repo.set_local_config("core.hooksPath", Some(".git/configured-hooks"))
            .unwrap();
        assert!(!repo.has_commit_hooks());
        let hook = hooks.join("pre-commit");
        fs::write(&hook, "#!/bin/sh\necho test-hook-rejected\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(repo.has_commit_hooks());
        write(&repo, file, b"After\n");
        repo.stage(&[file]).unwrap();

        let error = repo
            .commit_with_git("Subject", &CommitOptions::default())
            .unwrap_err();
        assert!(error.to_string().contains("test-hook-rejected"), "{error}");
        assert_eq!(
            repo.load_history(&Default::default())
                .unwrap()
                .head_sha
                .as_deref(),
            Some(base.as_str())
        );
        assert_eq!(staged(&repo, file).as_deref(), Some(b"After\n".as_slice()));
        let amended = repo
            .commit_with_git(
                "Subject\n\n# Comment\nBody\n",
                &CommitOptions {
                    amend: true,
                    skip_hooks: true,
                    strip_comments: true,
                    ..CommitOptions::default()
                },
            )
            .unwrap();
        assert_ne!(amended, base);
        assert_eq!(repo.head_commit_message().unwrap(), "Subject\n\nBody\n");
        assert!(repo.load_commit(&amended).unwrap().parents.is_empty());
        assert!(repo.load_status().unwrap().is_empty());
    }
}
