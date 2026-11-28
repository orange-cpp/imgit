//! The workflows: branch cleanup and Gitflow.
//!
//! Branch cleanup deletes the local branches merged into the default branch or whose upstream is gone, after a
//! review; Gitflow starts and finishes features, releases and hotfixes. Both run git through the application
//! (libgit2 for the branch deletions, the git CLI for the gitflow merges, so a conflict leaves the repository in
//! git's usual merge state). The pure logic (which branches qualify, which commands run) is in
//! [`imgit_app::workflows`].

use std::time::{Duration, Instant};

use dear_imgui_rs::{ChildFlags, Key, StyleColor, StyleVar};
use imgit_app::RepoState;
use imgit_app::repo_state::UndoEntry;
use imgit_app::workflows::cleanup::{self, BranchFacts};
use imgit_app::workflows::gitflow::{self, Branch, Kind};
use imgit_git::{FileStatus, Ref, RefType, Repository};

use crate::app::application::Application;
use crate::app::dialog::{Check, DialogKind};
use crate::app::toasts::ToastKind;
use crate::ui::Cx;
use crate::ui::geom::vec2;
use crate::ui::menu::{MenuItems, menu_item};
use crate::ui::panels::dialogs::{DeletedBranch, leading_dash_error, restore_branches};
use crate::ui::panels::tabs_bar::open_text_prompt;

#[derive(Default)]
pub struct State {}

/// How long the repository's `gitflow.*` configuration is trusted before it is read again.
const CONFIG_MAX_AGE: Duration = Duration::from_secs(1);

/// The repository's `gitflow.*` configuration, read again at most once a second (the left panel and the menus
/// ask for it every frame).
fn gitflow_config(state: &RepoState) -> gitflow::Config {
    let mut cache = state.gitflow_config_cache.borrow_mut();
    if let Some((read_at, config)) = cache.as_ref()
        && read_at.elapsed() <= CONFIG_MAX_AGE
    {
        return config.clone();
    }
    let value = |key: &str, fallback: &str| {
        state
            .repo
            .config_string(key)
            .unwrap_or_else(|| fallback.to_owned())
    };
    let config = gitflow::Config {
        master: state
            .repo
            .config_string("gitflow.branch.master")
            .unwrap_or_default(),
        develop: state
            .repo
            .config_string("gitflow.branch.develop")
            .unwrap_or_default(),
        feature_prefix: value("gitflow.prefix.feature", "feature/"),
        release_prefix: value("gitflow.prefix.release", "release/"),
        hotfix_prefix: value("gitflow.prefix.hotfix", "hotfix/"),
        version_tag: state
            .repo
            .config_string("gitflow.prefix.versiontag")
            .unwrap_or_default(),
    };
    *cache = Some((Instant::now(), config.clone()));
    config
}

fn has_branch(state: &RepoState, name: &str) -> bool {
    state.find_ref(&format!("refs/heads/{name}")).is_some()
}

/// Gitflow merges and checkouts need a working directory without changes (untracked files aside); a toast says
/// so when there are.
fn working_directory_clean(app: &mut Application, state: &RepoState, what: &str) -> bool {
    let status = &state.status;
    let dirty = !status.staged.is_empty()
        || !status.conflicted.is_empty()
        || status
            .unstaged
            .iter()
            .any(|change| change.status != FileStatus::Untracked);
    if dirty {
        app.toasts.push_with_detail(
            ToastKind::Error,
            format!("{what} failed"),
            "The working directory has changes. Commit or stash them first.",
        );
    }
    !dirty
}

/// Why the branch `full` (prefix and name) cannot be started, `None` when it can: git would read a name that
/// starts with '-' as an option. Only the branch is checked here: `gitflow::Config::check_name` also looks at the
/// tag a release or hotfix would get, which starting a branch does not need.
fn start_name_error(full: &str) -> Option<String> {
    leading_dash_error("branch", full)
}

/// Asks for the name of the new branch of `kind`, then starts it.
fn start(app: &mut Application, state: &mut RepoState, kind: Kind) {
    let config = gitflow_config(state);
    let title = format!("Start {kind}");
    let base = config.base_branch(kind);
    if !has_branch(state, base) {
        app.toasts.push_with_detail(
            ToastKind::Error,
            format!("{title} failed"),
            format!("There is no local branch '{base}'."),
        );
        return;
    }
    let workdir = state.repo.workdir().to_path_buf();
    open_text_prompt(
        app,
        format!("Name of the new {kind}:"),
        "",
        false,
        move |_cx, app, text| {
            let name = text.trim_matches(' ');
            if name.is_empty() {
                return;
            }
            app.with_repo(&workdir, |app, tab| {
                let branch = config.branch_name(kind, name);
                // `start_runs` has no commands for a name git would read as an option: say why, instead of
                // doing nothing. (The same check is in `imgit_app::workflows::gitflow`, which only refuses.)
                if let Some(error) = start_name_error(&branch) {
                    app.toasts
                        .push_with_detail(ToastKind::Error, format!("{title} failed"), error);
                    return;
                }
                if has_branch(tab, &branch) {
                    app.toasts.push_with_detail(
                        ToastKind::Error,
                        format!("{title} failed"),
                        format!("The branch '{branch}' exists already."),
                    );
                    return;
                }
                if !working_directory_clean(app, tab, &title) {
                    return;
                }
                let runs = config.start_runs(kind, name);
                if runs.is_empty() {
                    // A configured main / develop / prefix that starts with '-' (the name's own was checked).
                    app.toasts.push_with_detail(
                        ToastKind::Error,
                        format!("{title} failed"),
                        "A branch name cannot start with '-'",
                    );
                    return;
                }
                app.run_git_cli_sequence(tab, &format!("{title} {name}"), runs);
            });
        },
    );
}

/// The question the Finish dialog asks.
fn finish_message(
    config: &gitflow::Config,
    branch: &Branch,
    full: &str,
    tag: &str,
    tag_exists: bool,
) -> String {
    let mut message = if branch.kind == Kind::Feature {
        format!("Merge '{full}' into '{}' and delete it?", config.develop)
    } else {
        format!(
            "Merge '{full}' into '{}', {}merge it into '{}' and delete it?",
            config.master,
            if tag_exists {
                String::new()
            } else {
                format!("tag it '{tag}', ")
            },
            config.develop
        )
    };
    message.push_str(
        " A merge conflict stops the finish there; resolve and commit it, then finish again.",
    );
    message
}

/// Asks for confirmation, then merges the gitflow `branch` back and deletes it.
fn finish(app: &mut Application, state: &mut RepoState, branch: &Branch) {
    let config = gitflow_config(state);
    let kind = branch.kind;
    let full = config.branch_name(kind, &branch.name);
    let title = format!("Finish {kind} {}", branch.name);
    for target in [&config.master, &config.develop] {
        if !has_branch(state, target) {
            app.toasts.push_with_detail(
                ToastKind::Error,
                format!("{title} failed"),
                format!("There is no local branch '{target}'."),
            );
            return;
        }
    }
    if !working_directory_clean(app, state, &title) {
        return;
    }
    let tag = config.tag_name(&branch.name);
    let tag_exists = state.find_ref(&format!("refs/tags/{tag}")).is_some();
    let message = finish_message(&config, branch, &full, &tag, tag_exists);
    let workdir = state.repo.workdir().to_path_buf();
    app.dialog.open(DialogKind::Confirm);
    app.dialog.workdir = Some(workdir.clone());
    app.dialog.title = format!("Finish {kind}");
    app.dialog.message = message;
    app.dialog.confirm_label = format!("Finish {}", kind.title());
    let branch = branch.clone();
    app.dialog.on_confirm = Some(Box::new(move |_cx, app| {
        app.with_repo(&workdir, |app, tab| {
            let runs = config.finish_runs(branch.kind, &branch.name, tag_exists);
            if runs.is_empty() {
                // `finish_runs` has no commands for a branch, tag or configured branch name that starts with
                // '-' (git would read it as an option).
                app.toasts.push_with_detail(
                    ToastKind::Error,
                    format!("{title} failed"),
                    "A branch or tag name cannot start with '-'",
                );
                return;
            }
            app.run_git_cli_sequence(tab, &title, runs);
        });
    }));
}

// ---------------------------------------------------------------------------------------------------------
// Gitflow

/// Whether Initialize Gitflow has set the repository up (`gitflow.branch.master` is set).
#[must_use]
pub fn gitflow_initialized(state: &RepoState) -> bool {
    gitflow_config(state).initialized()
}

/// Start Feature / Release / Hotfix, each asking for the name in the prompt bar.
#[must_use]
pub fn gitflow_start_items(
    _cx: &mut Cx<'_>,
    _app: &mut Application,
    state: &mut RepoState,
) -> MenuItems {
    if !gitflow_initialized(state) {
        return MenuItems::new();
    }
    let workdir = state.repo.workdir().to_path_buf();
    Kind::ALL
        .into_iter()
        .map(|kind| {
            let workdir = workdir.clone();
            menu_item(format!("Start {}", kind.title()), move |_cx, app| {
                app.with_repo(&workdir, |app, state| start(app, state, kind));
            })
        })
        .collect()
}

/// "Finish feature <name>" when `branch` is a gitflow branch (empty otherwise).
#[must_use]
pub fn gitflow_finish_items(
    _cx: &mut Cx<'_>,
    _app: &mut Application,
    state: &mut RepoState,
    branch: &str,
) -> MenuItems {
    let Some(flow) = gitflow_config(state).classify(branch) else {
        return MenuItems::new();
    };
    let workdir = state.repo.workdir().to_path_buf();
    vec![menu_item(
        format!("Finish {} {}", flow.kind, flow.name),
        move |_cx, app| {
            app.with_repo(&workdir, |app, state| finish(app, state, &flow));
        },
    )]
}

/// The local gitflow branches (feature/..., release/..., hotfix/...), sorted.
#[must_use]
pub fn gitflow_branches(state: &RepoState) -> Vec<&Ref> {
    let config = gitflow_config(state);
    let mut branches: Vec<&Ref> = state
        .history
        .refs
        .iter()
        .filter(|r| r.ref_type == RefType::LocalBranch && config.classify(&r.name).is_some())
        .collect();
    branches.sort_by(|a, b| a.name.cmp(&b.name));
    branches
}

// ---------------------------------------------------------------------------------------------------------
// Branch cleanup

/// Branch cleanup: the review list of the local branches merged into the default branch or whose upstream is
/// gone (never the checked-out, default, or gitflow main / develop branch); the ticked ones are deleted.
/// Opens `app.dialog` as `DialogKind::BranchCleanup`.
pub fn open_branch_cleanup(_cx: &mut Cx<'_>, app: &mut Application, state: &mut RepoState) {
    let remote = state
        .history
        .remotes
        .first()
        .map_or("origin", |remote| remote.name.as_str());
    let default_branch = state.repo.default_branch(remote).unwrap_or_default();
    let default_ref = (!default_branch.is_empty())
        .then(|| state.find_ref(&format!("refs/heads/{default_branch}")))
        .flatten();
    let branches: Vec<BranchFacts> = state
        .history
        .refs
        .iter()
        .filter(|r| r.ref_type == RefType::LocalBranch)
        .map(|r| BranchFacts {
            name: r.name.clone(),
            is_head: r.is_head || state.repo.branch_checked_out(&r.name),
            merged: default_ref.is_some_and(|default| {
                r.target == default.target || state.repo.is_ancestor(&r.target, &default.target)
            }),
            gone: state.repo.upstream_gone(&r.name),
        })
        .collect();
    let mut protected_branches = vec![default_branch.clone()];
    let config = gitflow_config(state);
    if config.initialized() {
        protected_branches.push(config.master);
        protected_branches.push(config.develop);
    }
    let found = cleanup::candidates(&branches, &protected_branches);
    if found.is_empty() {
        app.toasts.push_with_detail(
            ToastKind::Info,
            "No branches to clean up",
            if default_branch.is_empty() {
                "No local branch has lost its upstream.".to_owned()
            } else {
                format!("No local branch is merged into {default_branch} or has lost its upstream.")
            },
        );
        return;
    }
    app.dialog.open(DialogKind::BranchCleanup);
    app.dialog.workdir = Some(state.repo.workdir().to_path_buf());
    app.dialog.text_a = default_branch;
    app.dialog.checks = found
        .iter()
        .map(|branch| Check::new(branch.name.as_str(), branch.reason()))
        .collect();
}

/// "Delete 1 Branch" / "Delete 3 Branches": the confirm button's label.
fn delete_label(ticked: usize) -> String {
    if ticked == 1 {
        "Delete 1 Branch".to_owned()
    } else {
        format!("Delete {ticked} Branches")
    }
}

/// The toast after a successful cleanup: "Deleted 1 branch" / "Deleted 3 branches".
fn deleted_message(count: usize) -> String {
    if count == 1 {
        "Deleted 1 branch".to_owned()
    } else {
        format!("Deleted {count} branches")
    }
}

/// The intro line above the review list.
fn intro(default_branch: &str) -> String {
    let reason = if default_branch.is_empty() {
        "These local branches have lost their upstream.".to_owned()
    } else {
        format!(
            "These local branches are merged into {default_branch} or have lost their upstream."
        )
    };
    format!("{reason} Untick the ones to keep.")
}

/// Deletes the branches again (redo).
fn delete_branches(repo: &Repository, deleted: &[DeletedBranch]) -> imgit_git::Result<()> {
    deleted
        .iter()
        .try_for_each(|branch| repo.delete_branch(&branch.name))
}

/// The `BranchCleanup` dialog's contents (`app.dialog`), inside its modal. The C++ also took the dialog; here
/// it is `app.dialog`.
pub fn draw_branch_cleanup(cx: &mut Cx<'_>, app: &mut Application, state: &mut RepoState) {
    let ui = cx.ui;
    let p = cx.palette;
    {
        let _wrap = ui.push_text_wrap_pos(0.0);
        ui.text(intro(&app.dialog.text_a));
    }
    ui.dummy([0.0, cx.px(6.0)]);
    if cx.link_text("Select all") {
        app.dialog.checks.iter_mut().for_each(|c| c.checked = true);
    }
    ui.same_line_with_spacing(0.0, cx.px(16.0));
    if cx.link_text("Select none") {
        app.dialog.checks.iter_mut().for_each(|c| c.checked = false);
    }
    ui.dummy([0.0, cx.px(4.0)]);
    let row_h = ui.frame_height_with_spacing();
    let list_h = cx
        .px(280.0)
        .min(row_h * app.dialog.checks.len() as f32 + cx.px(12.0));
    {
        let _background = ui.push_style_color(StyleColor::ChildBg, p.input_bg);
        ui.child_window("##cleanup_list")
            .size([-1.0, list_h])
            .child_flags(ChildFlags::BORDERS)
            .build(ui, || {
                for (index, check) in app.dialog.checks.iter_mut().enumerate() {
                    let _id = ui.push_id(index);
                    cx.checkbox(&check.label, &mut check.checked);
                    ui.same_line_with_spacing(0.0, cx.px(10.0));
                    // On the label's baseline: checkbox() frames its box with 2px padding.
                    {
                        let _padding =
                            ui.push_style_var(StyleVar::FramePadding([cx.px(2.0), cx.px(2.0)]));
                        ui.align_text_to_frame_padding();
                    }
                    ui.text_colored(p.text_secondary.to_array(), &check.note);
                }
            });
    }

    let ticked = app.dialog.checks.iter().filter(|c| c.checked).count();
    let label = delete_label(ticked);
    ui.dummy([0.0, cx.px(8.0)]);
    let confirm_w = cx.px(150.0);
    let cancel_w = cx.px(110.0);
    // GetContentRegionMax().x: the right edge of the content in window coordinates.
    let content_max_x = ui.cursor_pos_x() + cx.avail().x;
    // SAFETY: `ui` proves a frame is open, so the style exists; one float is read.
    let spacing_x = unsafe { (*dear_imgui_rs::sys::igGetStyle()).ItemSpacing.x };
    ui.set_cursor_pos_x(content_max_x - confirm_w - cancel_w - spacing_x);
    let cancel = cx.default_button("Cancel", vec2(cancel_w, cx.px(32.0)), true);
    ui.same_line();
    let confirm = cx.danger_button(&label, vec2(confirm_w, cx.px(32.0)), ticked > 0);
    if cancel || ui.is_key_pressed(Key::Escape) {
        ui.close_current_popup();
        return;
    }
    if !confirm || app.refuse_while_busy(state) {
        return;
    }

    // Each branch deleted with its tip and its upstream remembered: one undo entry puts them all back.
    let mut deleted: Vec<DeletedBranch> = Vec::new();
    let mut error = None;
    for check in app.dialog.checks.iter().filter(|c| c.checked) {
        let Some(branch) = DeletedBranch::of(state, &check.label) else {
            continue;
        };
        if let Err(e) = state.repo.delete_branch(&check.label) {
            error = Some(format!("{}: {e}", check.label));
            break;
        }
        deleted.push(branch);
    }
    let count = deleted.len();
    if !deleted.is_empty() {
        let label = match deleted.as_slice() {
            [branch] => format!("delete branch {}", branch.name),
            all => format!("delete {} branches", all.len()),
        };
        let for_redo = deleted.clone();
        // A branch the user made under one of these names since is not moved onto the old tip: undo skips it
        // and says so.
        state.push_undo(
            UndoEntry::new(label, move |repo| restore_branches(repo, &deleted))
                .with_redo(move |repo| delete_branches(repo, &for_redo)),
        );
    }
    match error {
        Some(error) => {
            app.toasts
                .push_with_detail(ToastKind::Error, "Branch cleanup failed", error);
        }
        None => app.toasts.push(ToastKind::Success, deleted_message(count)),
    }
    state.refresh();
    ui.close_current_popup();
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::shell::test_frame::{TestFrames, window_rect};

    fn gitflow_repo() -> (tempfile::TempDir, Repository, Application, String, String) {
        let scratch = tempfile::tempdir().unwrap();
        let repo = Repository::init(scratch.path().join("repo")).unwrap();
        repo.set_local_config("user.name", Some("Workflow test"))
            .unwrap();
        repo.set_local_config("user.email", Some("workflow@example.invalid"))
            .unwrap();
        repo.set_local_config("commit.gpgSign", Some("false"))
            .unwrap();
        repo.set_local_config("tag.gpgSign", Some("false")).unwrap();
        std::fs::write(repo.workdir().join("file.txt"), "Base\n").unwrap();
        repo.stage(&["file.txt"]).unwrap();
        let base = repo.commit("Base", false).unwrap();
        let main = repo
            .load_history(&Default::default())
            .unwrap()
            .head_branch
            .unwrap();
        let develop = "integration";
        assert_ne!(main, develop);
        repo.create_branch(develop, &base, false).unwrap();
        repo.set_local_config("gitflow.branch.master", Some(&main))
            .unwrap();
        repo.set_local_config("gitflow.branch.develop", Some(develop))
            .unwrap();
        repo.set_local_config("gitflow.prefix.versiontag", Some("v"))
            .unwrap();
        let mut app = Application::with_config_directory(scratch.path().join("config"));
        app.open_repository(&repo.workdir().to_string_lossy());
        (scratch, repo, app, main, base)
    }

    fn select(
        frames: &mut TestFrames,
        app: &mut Application,
        label: &str,
        menu: impl FnOnce(&mut Cx<'_>, &mut Application, &mut RepoState) -> MenuItems,
    ) {
        frames.draw(|cx| {
            let clicked = app
                .with_active(|app, state| {
                    let item = menu(cx, app, state)
                        .into_iter()
                        .find(|item| item.label == label)
                        .unwrap();
                    assert!(item.enabled);
                    item.click.unwrap()
                })
                .unwrap();
            clicked(cx, app);
        });
    }

    fn input_name(frames: &mut TestFrames, app: &mut Application, name: &str) {
        for _ in 0..3 {
            frames.draw(|cx| super::super::dialogs::draw_dialogs(cx, app));
        }
        frames.type_text(name);
        frames.draw(|cx| super::super::dialogs::draw_dialogs(cx, app));
        assert_eq!(app.dialog.text_a, name);
        frames.set_key(Key::Enter, true);
        frames.draw(|cx| super::super::dialogs::draw_dialogs(cx, app));
        frames.set_key(Key::Enter, false);
        frames.draw(|cx| super::super::dialogs::draw_dialogs(cx, app));
        assert!(!app.dialog.is_open());
    }

    fn wait_for_git(app: &mut Application, title: &str) {
        assert_eq!(
            app.active().unwrap().async_op.as_ref().unwrap().title,
            title
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while app.active().unwrap().busy() {
            assert!(Instant::now() < deadline, "{title} did not finish");
            app.poll_async();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_ne!(
            app.toasts.history()[0].kind,
            ToastKind::Error,
            "{title} failed"
        );
    }

    #[test]
    fn gitflow_release_starts_from_integration_and_finishes_on_both_branches() {
        let (_scratch, repo, mut app, main, base) = gitflow_repo();
        let mut frames = TestFrames::new();
        select(&mut frames, &mut app, "Start Release", gitflow_start_items);
        assert!(app.dialog.is(DialogKind::TextPrompt));
        input_name(&mut frames, &mut app, "1.2.0");
        wait_for_git(&mut app, "Start release 1.2.0");
        let state = app.active().unwrap();
        assert_eq!(state.history.head_branch.as_deref(), Some("release/1.2.0"));
        assert_eq!(state.history.head_sha.as_deref(), Some(base.as_str()));

        std::fs::write(repo.workdir().join("release.txt"), "Release notes\n").unwrap();
        repo.stage(&["release.txt"]).unwrap();
        let release = repo.commit("Release change", false).unwrap();
        app.active_mut().unwrap().refresh();
        select(
            &mut frames,
            &mut app,
            "Finish release 1.2.0",
            |cx, app, state| gitflow_finish_items(cx, app, state, "release/1.2.0"),
        );
        assert!(app.dialog.is(DialogKind::Confirm));
        assert!(app.dialog.message.contains("tag it 'v1.2.0'"));
        let confirm = app.dialog.on_confirm.take().unwrap();
        app.dialog.close();
        frames.draw(|cx| confirm(cx, &mut app));
        wait_for_git(&mut app, "Finish release 1.2.0");

        let state = app.active().unwrap();
        assert!(state.find_ref("refs/heads/release/1.2.0").is_none());
        let main_tip = &state
            .find_ref(&format!("refs/heads/{main}"))
            .unwrap()
            .target;
        let tag = state.find_ref("refs/tags/v1.2.0").unwrap();
        assert_eq!(&tag.target, main_tip);
        assert!(repo.is_ancestor(&release, main_tip));
        assert_eq!(state.history.head_branch.as_deref(), Some("integration"));
        assert!(repo.is_ancestor(&release, state.history.head_sha.as_deref().unwrap()));
        assert!(repo.workdir().join("release.txt").is_file());
    }

    #[test]
    fn gitflow_refuses_to_start_a_feature_with_modified_tracked_files() {
        let (_scratch, repo, mut app, main, base) = gitflow_repo();
        std::fs::write(repo.workdir().join("file.txt"), "Local work\n").unwrap();
        app.active_mut().unwrap().refresh();
        let mut frames = TestFrames::new();
        select(&mut frames, &mut app, "Start Feature", gitflow_start_items);
        input_name(&mut frames, &mut app, "protected");
        let state = app.active().unwrap();
        assert!(!state.busy());
        assert!(state.find_ref("refs/heads/feature/protected").is_none());
        assert_eq!(state.history.head_branch.as_deref(), Some(main.as_str()));
        assert_eq!(state.history.head_sha.as_deref(), Some(base.as_str()));
        assert_eq!(app.toasts.history()[0].title, "Start feature failed");
        assert_eq!(
            std::fs::read_to_string(repo.workdir().join("file.txt")).unwrap(),
            "Local work\n"
        );
    }

    #[test]
    fn branch_cleanup_deletes_only_checked_candidates_and_supports_undo_and_redo() {
        let (_scratch, repo, mut app, main, base) = gitflow_repo();
        for branch in ["delete-merged", "keep-merged"] {
            repo.create_branch(branch, &base, false).unwrap();
        }
        repo.create_branch("unmerged", &base, true).unwrap();
        let unmerged = repo.commit("Unmerged", false).unwrap();
        repo.checkout_branch(&main).unwrap();
        app.active_mut().unwrap().refresh();

        let mut frames = TestFrames::new();
        frames.draw(|cx| {
            app.with_active(|app, state| open_branch_cleanup(cx, app, state))
                .unwrap()
        });
        assert!(app.dialog.is(DialogKind::BranchCleanup));
        let labels: Vec<_> = app
            .dialog
            .checks
            .iter()
            .map(|check| check.label.as_str())
            .collect();
        assert_eq!(labels, ["delete-merged", "keep-merged"]);
        app.dialog
            .checks
            .iter_mut()
            .find(|check| check.label == "keep-merged")
            .unwrap()
            .checked = false;

        let mut delete_at = crate::ui::Vec2::ZERO;
        for _ in 0..3 {
            frames.draw(|cx| {
                super::super::dialogs::draw_dialogs(cx, &mut app);
                delete_at = window_rect(cx, c"Clean Up Branches").max - vec2(65.0, 30.0);
            });
        }
        frames.click_left(delete_at, |cx| {
            super::super::dialogs::draw_dialogs(cx, &mut app)
        });
        assert!(!app.dialog.is_open());
        assert_eq!(app.toasts.history()[0].title, "Deleted 1 branch");
        app.with_active(|app, state| {
            assert!(state.find_ref("refs/heads/delete-merged").is_none());
            assert!(state.find_ref("refs/heads/keep-merged").is_some());
            assert!(state.find_ref("refs/heads/integration").is_some());
            assert_eq!(
                state.find_ref("refs/heads/unmerged").unwrap().target,
                unmerged
            );

            app.undo(state);
            assert_eq!(
                state.find_ref("refs/heads/delete-merged").unwrap().target,
                base
            );
            app.redo(state);
            assert!(state.find_ref("refs/heads/delete-merged").is_none());
            assert_eq!(state.history.head_sha.as_deref(), Some(base.as_str()));
        })
        .unwrap();
    }

    #[test]
    fn labels_count_the_branches() {
        assert_eq!(delete_label(0), "Delete 0 Branches");
        assert_eq!(delete_label(1), "Delete 1 Branch");
        assert_eq!(delete_label(4), "Delete 4 Branches");
        assert_eq!(deleted_message(1), "Deleted 1 branch");
        assert_eq!(deleted_message(2), "Deleted 2 branches");
    }

    #[test]
    fn a_start_name_that_git_would_read_as_an_option_says_why() {
        let mut config = config();
        config.feature_prefix = "feature/".to_owned();
        // Behind the prefix a dash is harmless: the runs exist.
        let behind_prefix = config.branch_name(Kind::Feature, "-x");
        assert_eq!(start_name_error(&behind_prefix), None);
        assert!(!config.start_runs(Kind::Feature, "-x").is_empty());
        // With no prefix the name starts with it: nothing would run, and the user is told why.
        config.feature_prefix = String::new();
        let bare = config.branch_name(Kind::Feature, "-x");
        assert_eq!(
            start_name_error(&bare).as_deref(),
            Some("A branch name cannot start with '-'")
        );
        assert!(config.start_runs(Kind::Feature, "-x").is_empty());
    }

    #[test]
    fn the_intro_names_the_default_branch() {
        assert_eq!(
            intro("main"),
            "These local branches are merged into main or have lost their upstream. Untick the ones to keep."
        );
        assert_eq!(
            intro(""),
            "These local branches have lost their upstream. Untick the ones to keep."
        );
    }

    fn config() -> gitflow::Config {
        gitflow::Config {
            master: "main".to_owned(),
            develop: "develop".to_owned(),
            version_tag: "v".to_owned(),
            ..gitflow::Config::default()
        }
    }

    #[test]
    fn the_finish_question_depends_on_the_kind() {
        let feature = Branch {
            kind: Kind::Feature,
            name: "x".to_owned(),
        };
        let message = finish_message(&config(), &feature, "feature/x", "vx", false);
        assert!(message.starts_with("Merge 'feature/x' into 'develop' and delete it?"));
        let release = Branch {
            kind: Kind::Release,
            name: "1.0".to_owned(),
        };
        let message = finish_message(&config(), &release, "release/1.0", "v1.0", false);
        assert!(message.starts_with(
            "Merge 'release/1.0' into 'main', tag it 'v1.0', merge it into 'develop' and delete it?"
        ));
        let message = finish_message(&config(), &release, "release/1.0", "v1.0", true);
        assert!(message.starts_with(
            "Merge 'release/1.0' into 'main', merge it into 'develop' and delete it?"
        ));
        assert!(message.ends_with("then finish again."));
    }
}
