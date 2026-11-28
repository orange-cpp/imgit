//! Moving and rewriting history: reset, cherry-pick, revert, merge, the interactive rebase, conflicts.

use std::path::Path;

use super::{
    ConflictSides, Error, Outcome, Repository, Result, conflict_checkout, force_checkout,
    git_available, lossy, parse_oid, safe_checkout, sha_of, short_sha,
};
use crate::types::{RebaseAction, RebaseStep, ResetMode, Sha};

impl Repository {
    /// Moves HEAD's branch to `sha`: `Soft` keeps the index and the working directory, `Mixed` resets the
    /// index, `Hard` the working directory too (local changes are overwritten).
    pub fn reset(&self, sha: &str, mode: ResetMode) -> Result<()> {
        let commit = self.lookup_commit(sha)?;
        let kind = match mode {
            ResetMode::Soft => git2::ResetType::Soft,
            ResetMode::Mixed => git2::ResetType::Mixed,
            ResetMode::Hard => git2::ResetType::Hard,
        };
        self.repo
            .reset(commit.as_object(), kind, Some(&mut force_checkout()))?;
        Ok(())
    }

    /// Commits what a cherry-pick or revert left in the index, unless it left conflicts. A result that is
    /// HEAD's own tree (the change is already in the branch) is not committed, as `git` does not: the
    /// operation's state is cleaned up and the error says so.
    fn finish_sequencer_commit(
        &self,
        message: &str,
        author: Option<&git2::Signature<'_>>,
    ) -> Result<Outcome> {
        let mut index = self.repo.index()?;
        if index.has_conflicts() {
            return Ok(Outcome::Conflicts);
        }
        let head = self.head_commit()?;
        if let Some(head) = &head
            && index.write_tree()? == head.tree_id()
        {
            self.finish_operation_state();
            return Err(Error::message(
                "The change is already in the current branch",
            ));
        }
        let parents: Vec<git2::Oid> = head.iter().map(git2::Commit::id).collect();
        self.commit_index(message, &parents, author)?;
        self.finish_operation_state();
        Ok(Outcome::Done)
    }

    /// Applies a commit's changes on top of HEAD and commits them with its message and author (date
    /// included). On conflicts nothing is committed and the repository stays in the cherry-pick state.
    pub fn cherry_pick(&self, sha: &str) -> Result<Outcome> {
        let commit = self.lookup_commit(sha)?;
        let mut options = git2::CherrypickOptions::new();
        options.checkout_builder(conflict_checkout());
        self.repo.cherrypick(&commit, Some(&mut options))?;
        self.finish_sequencer_commit(&lossy(commit.message_bytes()), Some(&commit.author()))
    }

    /// Undoes a commit's changes on top of HEAD and commits that as `Revert "<summary>"`. On conflicts nothing
    /// is committed and the repository stays in the revert state.
    pub fn revert(&self, sha: &str) -> Result<Outcome> {
        let commit = self.lookup_commit(sha)?;
        let mut options = git2::RevertOptions::new();
        options.checkout_builder(conflict_checkout());
        self.repo.revert(&commit, Some(&mut options))?;
        let summary = commit.summary_bytes().map(lossy).unwrap_or_default();
        let message = format!("Revert \"{summary}\"\n\nThis reverts commit {sha}.\n");
        self.finish_sequencer_commit(&message, None)
    }

    /// Whether `ancestor` is reachable from `descendant` (false for the same commit).
    #[must_use]
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        let (Ok(ancestor), Ok(descendant)) = (parse_oid(ancestor), parse_oid(descendant)) else {
            return false;
        };
        self.repo
            .graph_descendant_of(descendant, ancestor)
            .unwrap_or(false)
    }

    /// The best common ancestor of two commits (`None` when they share no history).
    #[must_use]
    pub fn merge_base(&self, a: &str, b: &str) -> Option<Sha> {
        let base = self.repo.merge_base(parse_oid(a).ok()?, parse_oid(b).ok()?);
        base.ok().map(sha_of)
    }

    /// Merges a branch (a local one, else a remote-tracking one of that name) into HEAD: a fast-forward when
    /// HEAD can (unless `no_fast_forward` or `merge.ff = false` ask for a merge commit), else a merge commit
    /// "Merge <branch> into <HEAD's branch>". On conflicts nothing is committed and the repository stays in
    /// the merge state.
    pub fn merge(&self, branch_name: &str, no_fast_forward: bool) -> Result<Outcome> {
        let repo = &self.repo;
        let branch = repo
            .find_branch(branch_name, git2::BranchType::Local)
            .or_else(|_| repo.find_branch(branch_name, git2::BranchType::Remote))?;
        let annotated = repo.reference_to_annotated_commit(branch.get())?;
        let target = annotated.id();

        let (analysis, preference) = repo.merge_analysis(&[&annotated])?;
        if analysis.is_unborn() {
            // An unborn HEAD (reported with FASTFORWARD): the tree is checked out, then HEAD's branch is
            // created at the incoming commit.
            let head = repo.find_reference("HEAD")?;
            let head_branch = head
                .symbolic_target_bytes()
                .map(lossy)
                .ok_or_else(|| Error::message("HEAD is not on a branch"))?;
            let object = repo.find_object(target, Some(git2::ObjectType::Commit))?;
            repo.checkout_tree(&object, Some(&mut safe_checkout()))?;
            repo.reference(&head_branch, target, true, "merge: fast-forward")?;
            repo.set_head(&head_branch)?;
            return Ok(Outcome::Done);
        }
        if analysis.is_up_to_date() {
            return Ok(Outcome::Done);
        }
        if analysis.is_fast_forward() && !preference.is_no_fast_forward() && !no_fast_forward {
            let object = repo.find_object(target, Some(git2::ObjectType::Commit))?;
            repo.checkout_tree(&object, Some(&mut safe_checkout()))?;
            repo.head()?.set_target(target, "merge: fast-forward")?;
            return Ok(Outcome::Done);
        }

        repo.merge(&[&annotated], None, Some(&mut conflict_checkout()))?;
        if repo.index()?.has_conflicts() {
            return Ok(Outcome::Conflicts);
        }
        let head = self
            .head_commit()?
            .ok_or_else(|| Error::message("could not resolve HEAD"))?;
        let head_name = repo
            .head()
            .map(|head| lossy(head.shorthand_bytes()))
            .unwrap_or_else(|_| "HEAD".to_owned());
        self.commit_index(
            &format!("Merge {branch_name} into {head_name}"),
            &[head.id(), target],
            None,
        )?;
        let _ = repo.cleanup_state();
        Ok(Outcome::Done)
    }

    /// Gives up the merge, cherry-pick, revert or rebase in progress: HEAD's files come back (local changes
    /// are lost) and the operation's state is cleared.
    pub fn abort_operation(&self) -> Result<()> {
        use git2::RepositoryState as State;
        let repo = &self.repo;
        // A rebase and a multi-commit cherry-pick / revert are git's to abort: resetting to HEAD and cleaning
        // up would leave HEAD detached (rebase) or the sequence half done.
        let verb = match repo.state() {
            State::Rebase | State::RebaseInteractive | State::RebaseMerge => Some("rebase"),
            State::CherryPickSequence => Some("cherry-pick"),
            State::RevertSequence => Some("revert"),
            _ => None,
        };
        if let Some(verb) = verb {
            if !git_available() {
                return Err(Error::message(format!("git is needed to abort a {verb}")));
            }
            let output = self.git([verb, "--abort"], "");
            self.refresh_index();
            if !output.success() {
                return Err(Error::cli(&output, &format!("could not abort the {verb}")));
            }
            return Ok(());
        }
        if let Some(head) = self.head_commit()? {
            repo.reset(
                head.as_object(),
                git2::ResetType::Hard,
                Some(&mut force_checkout()),
            )?;
        }
        repo.cleanup_state()?;
        Ok(())
    }

    /// The three versions of a conflicted file the index holds; fails when the file has no conflict, or when
    /// a version is not UTF-8 text (binary files go to an external merge tool).
    pub fn conflict_sides(&self, path: &str) -> Result<ConflictSides> {
        let conflict = self.repo.index()?.conflict_get(Path::new(path))?;
        // Lossy text would make the merge tool write corrupted bytes: only UTF-8 text is handed out.
        let read = |entry: Option<git2::IndexEntry>| -> Result<Option<String>> {
            let Some(entry) = entry else {
                return Ok(None);
            };
            let blob = self.repo.find_blob(entry.id)?;
            let text = std::str::from_utf8(blob.content())
                .ok()
                .filter(|text| !text.contains('\0'))
                .ok_or_else(|| {
                    Error::message(format!(
                        "{path} is not UTF-8 text: resolve it with an external merge tool"
                    ))
                })?;
            Ok(Some(text.to_owned()))
        };
        Ok(ConflictSides {
            base: read(conflict.ancestor)?,
            ours: read(conflict.our)?,
            theirs: read(conflict.their)?,
        })
    }

    /// The commit coming in: `MERGE_HEAD`, else `CHERRY_PICK_HEAD` or `REVERT_HEAD`; `None` when there is
    /// none.
    #[must_use]
    pub fn incoming_head(&self) -> Option<Sha> {
        self.merge_heads()
            .first()
            .copied()
            .or_else(|| {
                ["CHERRY_PICK_HEAD", "REVERT_HEAD"]
                    .iter()
                    .find_map(|name| self.repo.refname_to_id(name).ok())
            })
            .map(sha_of)
    }

    /// Rebuilds the current branch on top of `base` from `steps` (oldest first): the interactive rebase,
    /// squash and drop. Each kept commit is cherry-picked with its author and date; a `Squash` is folded into
    /// the commit before it (the first kept step has none and is picked). Needs a clean working directory;
    /// conflicts roll everything back.
    pub fn rewrite_history(&self, base: &str, steps: &[RebaseStep]) -> Result<()> {
        let repo = &self.repo;
        if !self.load_status()?.is_empty() {
            return Err(Error::message(
                "The working directory must be clean. Commit or stash your changes first.",
            ));
        }
        let original = self
            .head_commit()?
            .ok_or_else(|| Error::message("nothing to rebase"))?
            .id();
        self.reset(base, ResetMode::Hard)?;

        let replayed = self.replay(steps);
        replayed.map_err(|why| {
            if let Ok(original) = repo.find_commit(original) {
                let _ = repo.reset(
                    original.as_object(),
                    git2::ResetType::Hard,
                    Some(&mut force_checkout()),
                );
            }
            let _ = repo.cleanup_state();
            Error::Message(format!("{why} The branch was restored."))
        })
    }

    /// Cherry-picks the steps onto HEAD; the error says why it stopped.
    fn replay(&self, steps: &[RebaseStep]) -> std::result::Result<(), String> {
        let repo = &self.repo;
        let sentence = |error: Error| format!("{error}.");
        let mut have_previous = false;
        for step in steps {
            if step.action == RebaseAction::Drop {
                continue;
            }
            let short = short_sha(&step.sha);
            let commit = self.lookup_commit(&step.sha).map_err(sentence)?;
            let mut options = git2::CherrypickOptions::new();
            options.checkout_builder(conflict_checkout());
            repo.cherrypick(&commit, Some(&mut options))
                .map_err(|_| format!("Could not apply {short}."))?;
            let mut index = repo
                .index()
                .ok()
                .filter(|index| !index.has_conflicts())
                .ok_or_else(|| format!("Conflicts while applying {short}."))?;
            let message = lossy(commit.message_bytes());
            let head = self
                .head_commit()
                .ok()
                .flatten()
                .ok_or("Lost HEAD during the rebase.")?;

            match &step.action {
                RebaseAction::Squash(combined) if have_previous => {
                    let tree_id = index
                        .write_tree()
                        .map_err(|_| "Could not write the squashed tree.")?;
                    let tree = repo
                        .find_tree(tree_id)
                        .map_err(|_| "Could not read the squashed tree.")?;
                    let combined = match combined.as_deref().filter(|text| !text.is_empty()) {
                        Some(combined) => combined.to_owned(),
                        None => {
                            let previous = lossy(head.message_bytes());
                            format!("{}\n\n{message}", previous.trim_end_matches('\n'))
                        }
                    };
                    let committer = self.default_signature().map_err(sentence)?;
                    head.amend(
                        Some("HEAD"),
                        None,
                        Some(&committer),
                        None,
                        Some(&combined),
                        Some(&tree),
                    )
                    .map_err(|_| format!("Could not squash {short}."))?;
                }
                action => {
                    let message = match action {
                        RebaseAction::Reword(Some(reworded)) if !reworded.is_empty() => reworded,
                        _ => &message,
                    };
                    // The author, with its date, stays the original commit's.
                    self.commit_index(message, &[head.id()], Some(&commit.author()))
                        .map_err(sentence)?;
                }
            }
            let _ = repo.cleanup_state();
            have_previous = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::ScratchRepository;

    #[test]
    fn conflicting_history_rewrite_restores_branch_index_and_files() {
        let scratch = ScratchRepository::new();
        let repo = &scratch.repo;
        let base = scratch.commit_file("tracked.txt", b"base\n", "Base");
        let original = scratch.commit_file("tracked.txt", b"original\n", "Original work");
        repo.create_branch("incoming", &base, true).unwrap();
        let incoming = scratch.commit_file("tracked.txt", b"incoming\n", "Incoming work");
        repo.checkout_branch("main").unwrap();

        let error = repo
            .rewrite_history(
                &incoming,
                &[RebaseStep {
                    sha: original.clone(),
                    action: RebaseAction::Pick,
                }],
            )
            .unwrap_err();
        assert!(error.to_string().contains("Conflicts while applying"));
        assert!(error.to_string().contains("The branch was restored."));
        assert_eq!(repo.repo.head().unwrap().name().unwrap(), "refs/heads/main");
        assert_eq!(
            repo.head_commit().unwrap().unwrap().id(),
            parse_oid(&original).unwrap()
        );
        assert_eq!(
            repo.repo.refname_to_id("refs/heads/incoming").unwrap(),
            parse_oid(&incoming).unwrap(),
        );
        assert_eq!(
            std::fs::read(repo.workdir().join("tracked.txt")).unwrap(),
            b"original\n"
        );
        assert_eq!(repo.repo.state(), git2::RepositoryState::Clean);
        assert_eq!(repo.incoming_head(), None);
        assert!(repo.load_status().unwrap().is_empty());
    }

    #[test]
    fn reverting_creates_a_commit_and_conflicting_reverts_can_be_aborted() {
        let scratch = ScratchRepository::new();
        let repo = &scratch.repo;
        let base = scratch.commit_file("tracked.txt", b"base\n", "Base");
        let changed = scratch.commit_file("tracked.txt", b"change\n", "Edit file");
        assert_eq!(repo.revert(&changed).unwrap(), Outcome::Done);
        let revert = repo.head_commit().unwrap().unwrap();
        assert_eq!(revert.parent_id(0).unwrap(), parse_oid(&changed).unwrap());
        assert_eq!(
            revert.tree_id(),
            repo.lookup_commit(&base).unwrap().tree_id()
        );
        assert_eq!(revert.summary().unwrap(), Some("Revert \"Edit file\""));
        assert!(repo.head_commit_message().unwrap().contains(&changed));
        assert_eq!(repo.incoming_head(), None);

        repo.reset(&changed, ResetMode::Hard).unwrap();
        let recent = scratch.commit_file("tracked.txt", b"later\n", "Edit the same line");
        assert_eq!(repo.revert(&changed).unwrap(), Outcome::Conflicts);
        assert_eq!(repo.incoming_head().as_deref(), Some(changed.as_str()));
        let parts = repo.conflict_sides("tracked.txt").unwrap();
        assert_eq!(parts.base.as_deref(), Some("change\n"));
        assert_eq!(parts.ours.as_deref(), Some("later\n"));
        assert_eq!(parts.theirs.as_deref(), Some("base\n"));

        repo.abort_operation().unwrap();
        assert_eq!(repo.incoming_head(), None);
        assert_eq!(
            repo.head_commit().unwrap().unwrap().id(),
            parse_oid(&recent).unwrap()
        );
        assert_eq!(
            std::fs::read(repo.workdir().join("tracked.txt")).unwrap(),
            b"later\n"
        );
        assert!(repo.load_status().unwrap().is_empty());
    }

    #[test]
    fn merging_fast_forwards_existing_and_unborn_branches() {
        let scratch = ScratchRepository::new();
        let repo = &scratch.repo;
        let base = scratch.commit_file("tracked.txt", b"base\n", "Base");
        repo.create_branch("topic", &base, true).unwrap();
        let topic = scratch.commit_file("tracked.txt", b"topic\n", "Topic work");
        repo.checkout_branch("main").unwrap();

        assert_eq!(repo.merge("topic", false).unwrap(), Outcome::Done);
        let target = parse_oid(&topic).unwrap();
        assert_eq!(repo.repo.refname_to_id("refs/heads/main").unwrap(), target);
        assert_eq!(
            std::fs::read(repo.workdir().join("tracked.txt")).unwrap(),
            b"topic\n"
        );

        repo.repo.set_head("refs/heads/new").unwrap();
        assert!(repo.head_commit().unwrap().is_none());
        std::fs::remove_file(repo.workdir().join("tracked.txt")).unwrap();
        let mut index = repo.repo.index().unwrap();
        index.clear().unwrap();
        index.write().unwrap();
        assert_eq!(repo.merge("topic", false).unwrap(), Outcome::Done);
        assert_eq!(repo.repo.head().unwrap().name().unwrap(), "refs/heads/new");
        assert_eq!(repo.repo.refname_to_id("refs/heads/new").unwrap(), target);
        assert_eq!(
            std::fs::read(repo.workdir().join("tracked.txt")).unwrap(),
            b"topic\n"
        );
        assert!(repo.load_status().unwrap().is_empty());
    }

    #[test]
    fn conflict_sides_support_missing_entries_and_reject_non_text() {
        let scratch = ScratchRepository::new();
        let repo = &scratch.repo;
        let base = scratch.commit_file("root.txt", b"base\n", "Base");
        scratch.commit_file("added.txt", b"main\n", "Add on main");
        scratch.commit_file("binary.dat", b"\xff\0", "Add binary on main");
        repo.create_branch("topic", &base, true).unwrap();
        scratch.commit_file("added.txt", b"topic\n", "Add on topic");
        let topic = scratch.commit_file("binary.dat", b"\x80\0", "Add binary on topic");
        repo.checkout_branch("main").unwrap();

        assert_eq!(repo.merge("topic", false).unwrap(), Outcome::Conflicts);
        assert_eq!(repo.incoming_head(), Some(topic));
        let parts = repo.conflict_sides("added.txt").unwrap();
        assert_eq!(parts.base, None);
        assert_eq!(parts.ours.as_deref(), Some("main\n"));
        assert_eq!(parts.theirs.as_deref(), Some("topic\n"));
        let error = repo.conflict_sides("binary.dat").unwrap_err();
        assert!(error.to_string().contains("external merge tool"));
        assert!(repo.conflict_sides("root.txt").is_err());
    }

    #[test]
    fn aborting_a_cli_rebase_reattaches_head_to_its_original_branch() {
        if !git_available() {
            return;
        }
        let scratch = ScratchRepository::new();
        let repo = &scratch.repo;
        let base = scratch.commit_file("tracked.txt", b"base\n", "Base");
        let original = scratch.commit_file("tracked.txt", b"main\n", "Main work");
        repo.create_branch("topic", &base, true).unwrap();
        scratch.commit_file("tracked.txt", b"topic\n", "Topic work");
        repo.checkout_branch("main").unwrap();

        let output = repo.git(
            ["-c", "rebase.autoStash=false", "rebase", "--merge", "topic"],
            "",
        );
        assert!(!output.success(), "{output:?}");
        assert!(
            matches!(
                repo.repo.state(),
                git2::RepositoryState::RebaseMerge | git2::RepositoryState::RebaseInteractive
            ),
            "{output:?}",
        );
        assert!(repo.repo.head_detached().unwrap());

        repo.abort_operation().unwrap();
        assert_eq!(repo.repo.state(), git2::RepositoryState::Clean);
        assert_eq!(repo.repo.head().unwrap().name().unwrap(), "refs/heads/main");
        assert_eq!(
            repo.head_commit().unwrap().unwrap().id(),
            parse_oid(&original).unwrap()
        );
        assert_eq!(
            std::fs::read(repo.workdir().join("tracked.txt")).unwrap(),
            b"main\n"
        );
        assert!(repo.load_status().unwrap().is_empty());
    }
}
