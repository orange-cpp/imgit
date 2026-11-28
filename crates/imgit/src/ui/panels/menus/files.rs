//! The menus of files and folders: working directory files, files of a commit, several files, a folder, and a
//! commit of the File History view.

use std::io::Write as _;
use std::path::Path;

use imgit_app::preferences::preferences;
use imgit_app::repo_state::{DiffSource, DiffTarget};
use imgit_app::text::file_name;
use imgit_app::{RepoState, WIP_SHA};
use imgit_git::{FileChange, FileStatus, Sha};

use super::commits::git_args;
use super::shared::{
    OperationState, confirm, full_path, repo_item, repo_view_item, save_git_output,
    selected_commits, workdir_of,
};
use crate::app::application::Application;
use crate::app::dialog::DialogKind;
use crate::app::toasts::ToastKind;
use crate::ui::Cx;
use crate::ui::menu::{MenuItem, MenuItems, menu_item, menu_submenu, separated_menu};
use crate::ui::panels::file_editor::{navigate_from_editor, open_file_editor};

/// Escapes what `.gitignore` reads as syntax, so that `text` matches only itself: a backslash, the glob
/// characters `*`, `?` and `[`, a leading `#` (a comment) or `!` (a negation) and the trailing spaces git
/// would drop.
fn escape_ignore_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 2);
    for (index, ch) in text.chars().enumerate() {
        if matches!(ch, '\\' | '*' | '?' | '[') || (index == 0 && matches!(ch, '#' | '!')) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    // "Trailing spaces are ignored unless they are quoted with backslash".
    let kept = escaped.trim_end_matches(' ').len();
    let spaces = escaped.len() - kept;
    escaped.truncate(kept);
    escaped.push_str(&"\\ ".repeat(spaces));
    escaped
}

/// The `.gitignore` line for `path` (relative to the working directory, '/' or '\\' separated): the file, or
/// with `folder` the folder and everything in it. Escaped, and anchored with a leading '/' when it sits at the
/// root, so that only that file (folder) is ignored and not every one of that name below it; a path with a
/// folder in it is anchored by git already. `None` for a path no line can name (empty, or with a line break).
fn ignore_line(path: &str, folder: bool) -> Option<String> {
    let path = path.replace('\\', "/");
    let path = path.trim_matches('/');
    if path.is_empty() || path.contains(['\n', '\r']) {
        return None;
    }
    let mut line = String::new();
    if !path.contains('/') {
        line.push('/');
    }
    line.push_str(&escape_ignore_text(path));
    if folder {
        line.push('/');
    }
    Some(line)
}

/// The `.gitignore` line for every file with the extension `extension` (without its dot): `*.rs`.
fn ignore_extension_line(extension: &str) -> Option<String> {
    (!extension.is_empty() && !extension.contains(['\n', '\r', '/']))
        .then(|| format!("*.{}", escape_ignore_text(extension)))
}

/// What to append to a `.gitignore` whose bytes are `existing` so that it holds each of `lines`: the missing
/// ones only (a line that is there already, or an unanchored copy of it, is skipped), each ended like the
/// file's own lines (CRLF when it uses them), after a line break when the last line has none. Returns the
/// bytes and how many lines they hold.
fn gitignore_additions(existing: &[u8], lines: &[String]) -> (Vec<u8>, usize) {
    let eol: &[u8] = if existing.windows(2).any(|pair| pair == b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    };
    let mut present: std::collections::HashSet<Vec<u8>> = existing
        .split(|&byte| byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line).to_vec())
        .collect();
    let mut added = Vec::new();
    let mut count = 0;
    for line in lines {
        // `/name` is covered by `name`, which ignores every file of that name.
        let unanchored = line
            .strip_prefix('/')
            .filter(|rest| !rest.trim_end_matches('/').contains('/'));
        if present.contains(line.as_bytes())
            || unanchored.is_some_and(|rest| present.contains(rest.as_bytes()))
        {
            continue;
        }
        present.insert(line.as_bytes().to_vec());
        added.extend_from_slice(line.as_bytes());
        added.extend_from_slice(eol);
        count += 1;
    }
    if count > 0 && !existing.is_empty() && !existing.ends_with(b"\n") {
        let mut with_break = eol.to_vec();
        with_break.extend(added);
        added = with_break;
    }
    (added, count)
}

/// Appends `lines` to the repository's `.gitignore` (created when missing) as [`gitignore_additions`] says;
/// returns how many were added (0: they were all there).
fn append_to_gitignore(workdir: &Path, lines: &[String]) -> std::io::Result<usize> {
    let path = workdir.join(".gitignore");
    let existing = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let (added, count) = gitignore_additions(&existing, lines);
    if count > 0 {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(&added)?;
    }
    Ok(count)
}

/// The toast of an append: what was added, or that it was there.
fn report_ignored(app: &mut Application, result: std::io::Result<usize>, what: String) {
    match result {
        Ok(0) => app
            .toasts
            .push_with_detail(ToastKind::Info, "Already in .gitignore", what),
        Ok(_) => app
            .toasts
            .push_with_detail(ToastKind::Success, "Added to .gitignore", what),
        Err(error) => app.toasts.push_with_detail(
            ToastKind::Error,
            "Could not write .gitignore",
            error.to_string(),
        ),
    }
}

/// The item that adds the line `pattern` to `.gitignore` and says so.
fn ignore_item(label: String, workdir: &Path, pattern: String) -> MenuItem {
    repo_view_item(label, workdir, move |_cx, app, state| {
        let result = append_to_gitignore(state.repo.workdir(), std::slice::from_ref(&pattern));
        report_ignored(app, result, pattern);
        state.refresh_status();
    })
}

/// "Ignore": the file, its extension, its folder.
fn ignore_submenu(workdir: &Path, path: &str) -> MenuItem {
    let p = Path::new(path);
    let mut items = Vec::new();
    if let Some(line) = ignore_line(path, false) {
        items.push(ignore_item(
            format!(
                "Ignore '{}'",
                p.file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default()
            ),
            workdir,
            line,
        ));
    }
    if let Some(extension) = p.extension()
        && let Some(line) = ignore_extension_line(&extension.to_string_lossy())
    {
        items.push(ignore_item(
            format!(
                "All files with the extension '.{}'",
                extension.to_string_lossy()
            ),
            workdir,
            line,
        ));
    }
    if let Some(parent) = p.parent().filter(|parent| !parent.as_os_str().is_empty())
        && let Some(line) = ignore_line(&parent.to_string_lossy(), true)
    {
        items.push(ignore_item(
            format!(
                "All files in '{}/'",
                parent
                    .file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default()
            ),
            workdir,
            line,
        ));
    }
    menu_submenu("Ignore", items)
}

/// "Open in external editor", "Open file in default program", "Show in folder": for a file that exists.
fn open_file_items(workdir: &Path, path: &str) -> MenuItems {
    let full = std::path::PathBuf::from(full_path(workdir, path));
    if !full.is_file() {
        return MenuItems::new();
    }
    let (editor, default_program, folder) = (full.clone(), full.clone(), full);
    vec![
        menu_item("Open in external editor", move |_cx, app| {
            app.open_in_editor(&editor);
        }),
        menu_item("Open file in default program", move |_cx, _app| {
            let _ = imgit_platform::shell::open_path(&default_program);
        }),
        menu_item("Show in folder", move |_cx, _app| {
            let _ = imgit_platform::shell::show_in_folder(&folder);
        }),
    ]
}

/// "Edit file" and "Delete file", for a file that exists.
fn edit_delete_items(workdir: &Path, path: &str) -> MenuItems {
    let full = std::path::PathBuf::from(full_path(workdir, path));
    if !full.is_file() {
        return MenuItems::new();
    }
    let (edit_path, delete_path) = (path.to_owned(), path.to_owned());
    let delete_workdir = workdir.to_path_buf();
    vec![
        repo_view_item("Edit file", workdir, move |cx, app, state| {
            open_file_editor(cx, app, state, &edit_path);
        }),
        menu_item("Delete file", move |_cx, app| {
            let path = delete_path.clone();
            confirm(
                app,
                "Delete File",
                format!("Are you sure you want to delete {path}?"),
                "Delete",
                true,
                move |_cx, app| {
                    app.with_repo(&delete_workdir, |app, state| {
                        if let Err(error) = std::fs::remove_file(&full) {
                            app.toasts.push_with_detail(
                                ToastKind::Error,
                                format!("Could not delete {path}"),
                                error.to_string(),
                            );
                        }
                        state.refresh_status();
                    });
                },
            );
        }),
    ]
}

/// File History / File Blame: the File History view of `path` at `sha` (HEAD when empty: the working
/// directory's files).
fn history_items(workdir: &Path, path: &str, sha: &str) -> MenuItems {
    let from: Option<Sha> = Some(sha.to_owned()).filter(|sha| !sha.is_empty() && sha != WIP_SHA);
    [("File History", false), ("File Blame", true)]
        .into_iter()
        .map(|(label, blame)| {
            let (path, from) = (path.to_owned(), from.clone());
            repo_view_item(label, workdir, move |_cx, _app, state| {
                state.open_file_history(path, from.clone(), from, blame);
            })
        })
        .collect()
}

/// The path of the change and, for a rename, the one it came from.
fn paths_of(change: &FileChange) -> Vec<String> {
    let mut paths = vec![change.path.clone()];
    paths.extend(change.old_path.clone());
    paths
}

/// The staged (or unstaged) changes of `state` among `paths`.
fn changes_among(state: &RepoState, paths: &[String], staged: bool) -> Vec<FileChange> {
    let list = if staged {
        &state.status.staged
    } else {
        &state.status.unstaged
    };
    list.iter()
        .filter(|change| paths.contains(&change.path))
        .cloned()
        .collect()
}

/// `changes` that were staged until just now, as the working directory lists them once unstaged: a new file
/// (`Added`) is untracked then, which is what `Repository::discard` deletes.
fn as_unstaged(changes: impl IntoIterator<Item = FileChange>) -> Vec<FileChange> {
    changes
        .into_iter()
        .map(|mut change| {
            if change.status == FileStatus::Added {
                change.status = FileStatus::Untracked;
            }
            change
        })
        .collect()
}

/// A discard of the working directory changes of `paths` (unstaged first when they are staged).
fn discard_paths(app: &mut Application, state: &mut RepoState, paths: &[String], staged: bool) {
    let mut changes = changes_among(state, paths, staged);
    if staged {
        let unstaged = state.repo.unstage(paths);
        // A file that is still staged must not be deleted.
        if app.run(state, "Unstage", unstaged, None).is_none() {
            return;
        }
        changes = as_unstaged(changes);
    }
    let result = state.repo.discard(&changes);
    app.run(state, "Discard", result, None);
}

fn diff_args(staged: bool, paths: &[String]) -> Vec<String> {
    let mut args = git_args(&["diff"]);
    if staged {
        args.push("--cached".to_owned());
    }
    args.push("--".to_owned());
    args.extend(paths.iter().cloned());
    args
}

fn stash_args(paths: &[String]) -> Vec<String> {
    let mut args = git_args(&["stash", "push", "--"]);
    args.extend(paths.iter().cloned());
    args
}

#[must_use]
pub fn workdir_file_menu(
    _cx: &mut Cx<'_>,
    _app: &mut Application,
    state: &mut RepoState,
    change: &FileChange,
    staged: bool,
) -> MenuItems {
    let workdir = workdir_of(state);
    let path = change.path.clone();
    let copy = change.clone();
    let added = matches!(change.status, FileStatus::Added | FileStatus::Untracked);
    let conflicted = change.status == FileStatus::Conflicted;
    let mut ops = MenuItems::new();
    if conflicted {
        let target = change.clone();
        ops.push(repo_view_item(
            "Open in merge tool",
            &workdir,
            move |_cx, app, state| {
                navigate_from_editor(app, state, move |app, state| {
                    if let Err(error) = state.open_merge_tool(&target.path) {
                        app.toasts.push_with_detail(
                            ToastKind::Error,
                            "Could not open the merge tool",
                            error.to_string(),
                        );
                        let _ = state.open_diff(DiffTarget {
                            source: DiffSource::Unstaged,
                            path: target.path,
                            status: target.status,
                        });
                    }
                });
            },
        ));
        if preferences().external_merge_tool_enabled() {
            let path = path.clone();
            ops.push(repo_item(
                "Open in external merge tool",
                &workdir,
                move |_cx, app, state| {
                    app.run_git_cli(
                        state,
                        "External merge tool",
                        ["mergetool", "--no-prompt", "--", path.as_str()],
                    );
                },
            ));
        }
        let path = path.clone();
        ops.push(repo_item(
            "Mark resolved",
            &workdir,
            move |_cx, app, state| {
                let result = state.repo.stage(&[path]);
                app.run(state, "Mark resolved", result, None);
            },
        ));
    } else if staged {
        let paths = paths_of(change);
        ops.push(repo_item("Unstage", &workdir, move |_cx, app, state| {
            let result = state.repo.unstage(&paths);
            app.run(state, "Unstage", result, None);
        }));
    } else {
        let path = path.clone();
        ops.push(repo_item("Stage", &workdir, move |_cx, app, state| {
            let result = state.repo.stage(&[path]);
            app.run(state, "Stage", result, None);
        }));
    }
    let discard = |label: &str| {
        let copy = copy.clone();
        repo_item(label, &workdir, move |_cx, app, state| {
            let mut change = copy.clone();
            if staged {
                let paths = paths_of(&copy);
                if state.repo.unstage(&paths).is_err() {
                    return;
                }
                change = as_unstaged([change]).remove(0);
            }
            let message = format!("Discarded changes to {}", file_name(&copy.path));
            let result = state.repo.discard(std::slice::from_ref(&change));
            app.run(state, "Discard", result, Some(&message));
        })
    };
    if !conflicted {
        if added && staged {
            ops.push(discard("Unstage and delete file"));
        } else if !added {
            ops.push(discard("Discard changes"));
        }
    }
    if file_name(&path) != ".gitattributes" && file_name(&path) != ".gitignore" {
        ops.push(ignore_submenu(&workdir, &path));
    }
    if !conflicted && state.repo.state() != OperationState::Merge {
        let paths = [path.clone()];
        ops.push(repo_item("Stash file", &workdir, move |_cx, app, state| {
            app.run_git_cli(state, "Stash file", stash_args(&paths));
        }));
    }
    let history = if added {
        MenuItems::new()
    } else {
        history_items(&workdir, &path, "")
    };
    let mut open = MenuItems::new();
    if preferences().external_diff_tool_enabled() && !conflicted {
        let path = path.clone();
        open.push(repo_view_item(
            "Open in external diff tool",
            &workdir,
            move |_cx, app, state| {
                let mut args = git_args(&["difftool", "-y"]);
                if staged {
                    args.push("--cached".to_owned());
                }
                args.push("--".to_owned());
                args.push(path);
                app.run_git_cli_silent(state, "Open in diff tool", args);
            },
        ));
    }
    open.extend(open_file_items(&workdir, &path));
    let (copy_path, patch_path) = (path.clone(), path.clone());
    let copy_workdir = workdir.clone();
    separated_menu(vec![
        ops,
        history,
        open,
        vec![
            menu_item("Copy file path", move |cx, app| {
                app.copy_to_clipboard(cx, &full_path(&copy_workdir, &copy_path));
            }),
            repo_view_item(
                "Create patch from file changes",
                &workdir,
                move |cx, app, state| {
                    let args = diff_args(staged, std::slice::from_ref(&patch_path));
                    let workdir = state.repo.workdir().to_path_buf();
                    save_git_output(
                        cx,
                        app,
                        &workdir,
                        &format!("{}.patch", file_name(&patch_path)),
                        &args,
                    );
                },
            ),
        ],
        edit_delete_items(&workdir, &path),
    ])
}

#[must_use]
pub fn commit_file_menu(
    _cx: &mut Cx<'_>,
    _app: &mut Application,
    state: &mut RepoState,
    sha: &str,
    change: &FileChange,
) -> MenuItems {
    let workdir = workdir_of(state);
    let path = change.path.clone();
    let stash = state
        .commit_for(sha)
        .is_some_and(|commit| commit.node_type == imgit_git::NodeType::Stash);
    let checkout_file = |label: &str, title: &'static str| {
        let (sha, path) = (sha.to_owned(), path.clone());
        repo_item(label, &workdir, move |_cx, app, state| {
            app.run_git_cli(
                state,
                title,
                ["checkout", sha.as_str(), "--", path.as_str()],
            );
        })
    };
    let mut apply = MenuItems::new();
    if stash {
        apply.push(checkout_file("Apply stashed file", "Apply stashed file"));
    }
    let mut restore = MenuItems::new();
    if !stash && selected_commits(state).len() == 1 {
        restore.push(checkout_file(
            "Restore file from this commit",
            "Restore file",
        ));
    }
    let mut open = MenuItems::new();
    if preferences().external_diff_tool_enabled() {
        let (sha, path) = (sha.to_owned(), path.clone());
        open.push(repo_view_item(
            "Open in external diff tool",
            &workdir,
            move |_cx, app, state| {
                app.run_git_cli_silent(
                    state,
                    "Open in diff tool",
                    [
                        "difftool",
                        "-y",
                        format!("{sha}^!").as_str(),
                        "--",
                        path.as_str(),
                    ],
                );
            },
        ));
    }
    open.extend(open_file_items(&workdir, &path));
    let copy_path = path.clone();
    let copy_workdir = workdir.clone();
    separated_menu(vec![
        if stash {
            MenuItems::new()
        } else {
            history_items(&workdir, &path, sha)
        },
        open,
        vec![menu_item("Copy file path", move |cx, app| {
            app.copy_to_clipboard(cx, &full_path(&copy_workdir, &copy_path));
        })],
        edit_delete_items(&workdir, &path),
        apply,
        restore,
    ])
}

#[must_use]
pub fn multiple_files_menu(
    _cx: &mut Cx<'_>,
    _app: &mut Application,
    state: &mut RepoState,
    paths: Vec<String>,
    staged: bool,
) -> MenuItems {
    let workdir = workdir_of(state);
    let n = paths.len();
    let mut ops = MenuItems::new();
    let action_paths = paths.clone();
    if staged {
        ops.push(repo_item(
            format!("Unstage {n} files"),
            &workdir,
            move |_cx, app, state| {
                let result = state.repo.unstage(&action_paths);
                app.run(state, "Unstage", result, None);
            },
        ));
    } else {
        ops.push(repo_item(
            format!("Stage {n} files"),
            &workdir,
            move |_cx, app, state| {
                let result = state.repo.stage(&action_paths);
                app.run(state, "Stage", result, None);
            },
        ));
    }
    let discard_paths_list = paths.clone();
    ops.push(repo_item(
        format!("Discard {n} files"),
        &workdir,
        move |_cx, app, state| discard_paths(app, state, &discard_paths_list, staged),
    ));
    let ignore_paths = paths.clone();
    ops.push(repo_view_item(
        format!("Ignore {n} files"),
        &workdir,
        move |_cx, app, state| {
            let lines: Vec<String> = ignore_paths
                .iter()
                .filter_map(|path| ignore_line(path, false))
                .collect();
            let result = append_to_gitignore(state.repo.workdir(), &lines);
            report_ignored(app, result, format!("{} files", lines.len()));
            state.refresh_status();
        },
    ));
    if state.repo.state() != OperationState::Merge {
        let stash_paths = paths.clone();
        ops.push(repo_item(
            format!("Stash {n} files"),
            &workdir,
            move |_cx, app, state| app.run_git_cli(state, "Stash files", stash_args(&stash_paths)),
        ));
    }
    separated_menu(vec![
        ops,
        vec![repo_view_item(
            format!("Create patch from changes in {n} files"),
            &workdir,
            move |cx, app, state| {
                let workdir = state.repo.workdir().to_path_buf();
                save_git_output(
                    cx,
                    app,
                    &workdir,
                    "changes.patch",
                    &diff_args(staged, &paths),
                );
            },
        )],
    ])
}

#[must_use]
pub fn folder_menu(
    _cx: &mut Cx<'_>,
    _app: &mut Application,
    state: &mut RepoState,
    folder: &str,
    files: Vec<String>,
    staged: bool,
    uncommitted: bool,
) -> MenuItems {
    let workdir = workdir_of(state);
    let mut ops = MenuItems::new();
    let base = Path::new(folder)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if uncommitted {
        let stage_files = files.clone();
        if staged {
            ops.push(repo_item(
                "Unstage folder",
                &workdir,
                move |_cx, app, state| {
                    let result = state.repo.unstage(&stage_files);
                    app.run(state, "Unstage", result, None);
                },
            ));
        } else {
            ops.push(repo_item(
                "Stage folder",
                &workdir,
                move |_cx, app, state| {
                    let result = state.repo.stage(&stage_files);
                    app.run(state, "Stage", result, None);
                },
            ));
        }
        let discard_files = files.clone();
        ops.push(repo_item(
            "Discard all changes in folder",
            &workdir,
            move |_cx, app, state| discard_paths(app, state, &discard_files, staged),
        ));
        let ignore_folder = folder.to_owned();
        ops.push(repo_view_item(
            format!("Ignore all files in '{base}/'"),
            &workdir,
            move |_cx, app, state| {
                let lines: Vec<String> = ignore_line(&ignore_folder, true).into_iter().collect();
                let result = append_to_gitignore(state.repo.workdir(), &lines);
                report_ignored(app, result, lines.join(""));
                state.refresh_status();
            },
        ));
        if state.repo.state() != OperationState::Merge {
            let stash_files = files.clone();
            ops.push(repo_item(
                "Stash folder",
                &workdir,
                move |_cx, app, state| {
                    app.run_git_cli(state, "Stash folder", stash_args(&stash_files));
                },
            ));
        }
        ops.push(repo_view_item(
            "Create Patch from changes in directory",
            &workdir,
            move |cx, app, state| {
                let workdir = state.repo.workdir().to_path_buf();
                save_git_output(
                    cx,
                    app,
                    &workdir,
                    "changes.patch",
                    &diff_args(staged, &files),
                );
            },
        ));
    }
    let mut open = MenuItems::new();
    let full = std::path::PathBuf::from(full_path(&workdir, folder));
    if full.is_dir() {
        open.push(menu_item("Open folder", move |_cx, _app| {
            let _ = imgit_platform::shell::open_path(&full);
        }));
        let folder = folder.to_owned();
        open.push(menu_item(
            "Create a file in this folder",
            move |_cx, app| {
                app.dialog.open(DialogKind::CreateFile);
                app.dialog.text_b = folder;
            },
        ));
    }
    separated_menu(vec![ops, open])
}

/// A commit row of the File History view.
#[must_use]
pub fn file_history_commit_menu(
    _cx: &mut Cx<'_>,
    _app: &mut Application,
    state: &mut RepoState,
    sha: &str,
    path: &str,
) -> MenuItems {
    let workdir = workdir_of(state);
    let (jump_sha, copy_sha) = (sha.to_owned(), sha.to_owned());
    let (copy_path, copy_workdir) = (path.to_owned(), workdir.clone());
    separated_menu(vec![
        vec![
            repo_view_item("Show in graph", &workdir, move |_cx, _app, state| {
                state.close_file_history();
                super::jump_to_commit(state, &jump_sha);
            })
            .enabled(state.history.row_by_sha.contains_key(sha)),
        ],
        vec![
            menu_item("Copy commit sha", move |cx, app| {
                app.copy_to_clipboard(cx, &copy_sha);
            }),
            menu_item("Copy file path", move |cx, app| {
                app.copy_to_clipboard(cx, &full_path(&copy_workdir, &copy_path));
            }),
        ],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::test_frame::TestFrames;

    fn file_fixture() -> (
        tempfile::TempDir,
        imgit_git::Repository,
        Application,
        String,
    ) {
        let scratch = tempfile::tempdir().unwrap();
        let repo = imgit_git::Repository::init(scratch.path().join("repo")).unwrap();
        repo.set_local_config("user.name", Some("File menu test"))
            .unwrap();
        repo.set_local_config("user.email", Some("file-menu@example.invalid"))
            .unwrap();
        repo.set_local_config("core.autocrlf", Some("false"))
            .unwrap();
        std::fs::create_dir_all(repo.workdir().join("src")).unwrap();
        let tracked = ["src/first.txt", "src/second.txt", "outside.txt"];
        for path in tracked {
            std::fs::write(repo.workdir().join(path), "Original\n").unwrap();
        }
        repo.stage(&tracked).unwrap();
        let base = repo.commit("Original files", false).unwrap();
        let app = Application::with_config_directory(scratch.path().join("config"));
        (scratch, repo, app, base)
    }

    fn choose(
        frames: &mut TestFrames,
        app: &mut Application,
        label: &str,
        build: impl FnOnce(&mut Cx<'_>, &mut Application, &mut RepoState) -> MenuItems,
    ) {
        frames.draw(|cx| {
            let items = app.with_active(|app, state| build(cx, app, state)).unwrap();
            let item = items
                .into_iter()
                .find(|item| item.label == label)
                .unwrap_or_else(|| panic!("missing menu item {label:?}"));
            assert!(item.enabled, "{label:?} is disabled");
            item.click.unwrap()(cx, app);
        });
    }

    #[test]
    fn file_menus_stage_unstage_and_ignore_without_touching_other_files() {
        let (_scratch, repo, mut app, _) = file_fixture();
        let staged = "src/first.txt";
        let ignored = "src/new-fixture.imtest";
        for path in [staged, ignored] {
            std::fs::write(repo.workdir().join(path), "Working copy\n").unwrap();
        }
        app.open_repository(&repo.workdir().to_string_lossy());
        let mut frames = TestFrames::new();
        let menu =
            |cx: &mut Cx<'_>, app: &mut Application, state: &mut RepoState, staged_side: bool| {
                let list = if staged_side {
                    &state.status.staged
                } else {
                    &state.status.unstaged
                };
                let change = list
                    .iter()
                    .find(|change| change.path == staged)
                    .unwrap()
                    .clone();
                workdir_file_menu(cx, app, state, &change, staged_side)
            };

        choose(&mut frames, &mut app, "Stage", |cx, app, state| {
            menu(cx, app, state, false)
        });
        let state = app.active().unwrap();
        assert_eq!(state.status.staged.len(), 1);
        assert_eq!(state.status.staged[0].path, staged);
        assert_eq!(state.status.unstaged[0].path, ignored);
        choose(&mut frames, &mut app, "Unstage", |cx, app, state| {
            menu(cx, app, state, true)
        });
        assert!(app.active().unwrap().status.staged.is_empty());

        choose(
            &mut frames,
            &mut app,
            "Ignore 'new-fixture.imtest'",
            |cx, app, state| {
                let file = state
                    .status
                    .unstaged
                    .iter()
                    .find(|change| change.path == ignored)
                    .unwrap()
                    .clone();
                workdir_file_menu(cx, app, state, &file, false)
                    .into_iter()
                    .find(|item| item.label == "Ignore")
                    .unwrap()
                    .submenu
            },
        );
        let state = app.active().unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.workdir().join(".gitignore")).unwrap(),
            "src/new-fixture.imtest\n"
        );
        assert!(
            state
                .status
                .unstaged
                .iter()
                .all(|change| change.path != ignored)
        );
        assert!(
            state
                .status
                .unstaged
                .iter()
                .any(|change| change.path == staged)
        );
        assert!(repo.workdir().join(ignored).exists());
    }

    #[test]
    fn folder_and_multi_file_commands_leave_outside_changes_intact() {
        let (_scratch, repo, mut app, _) = file_fixture();
        for path in ["src/first.txt", "src/second.txt", "outside.txt"] {
            std::fs::write(repo.workdir().join(path), "Working copy\n").unwrap();
        }
        app.open_repository(&repo.workdir().to_string_lossy());
        let mut frames = TestFrames::new();
        let paths = || vec!["src/first.txt".to_owned(), "src/second.txt".to_owned()];
        choose(&mut frames, &mut app, "Stage folder", |cx, app, state| {
            folder_menu(cx, app, state, "src", paths(), false, true)
        });
        assert_eq!(app.active().unwrap().status.staged.len(), 2);

        choose(
            &mut frames,
            &mut app,
            "Unstage 2 files",
            |cx, app, state| multiple_files_menu(cx, app, state, paths(), true),
        );
        assert!(app.active().unwrap().status.staged.is_empty());

        choose(&mut frames, &mut app, "Stage 2 files", |cx, app, state| {
            multiple_files_menu(cx, app, state, paths(), false)
        });
        assert_eq!(app.active().unwrap().status.staged.len(), 2);
        choose(
            &mut frames,
            &mut app,
            "Discard all changes in folder",
            |cx, app, state| folder_menu(cx, app, state, "src", paths(), true, true),
        );
        let state = app.active().unwrap();
        assert!(state.status.staged.is_empty());
        assert_eq!(state.status.unstaged.len(), 1);
        assert_eq!(state.status.unstaged[0].path, "outside.txt");
        for path in paths() {
            assert_eq!(
                std::fs::read_to_string(repo.workdir().join(path)).unwrap(),
                "Original\n"
            );
        }
        assert_eq!(
            std::fs::read_to_string(repo.workdir().join("outside.txt")).unwrap(),
            "Working copy\n"
        );
    }

    #[test]
    fn commit_file_commands_restore_one_file_and_open_blame_at_the_requested_revision() {
        let (_scratch, repo, mut app, base) = file_fixture();
        let path = "src/first.txt";
        let other = "src/second.txt";
        for name in [path, other] {
            std::fs::write(repo.workdir().join(name), "Second revision\n").unwrap();
        }
        repo.stage(&[path, other]).unwrap();
        let head = repo.commit("Second revision", false).unwrap();
        app.open_repository(&repo.workdir().to_string_lossy());
        app.active_mut().unwrap().select_only(&base);
        let mut frames = TestFrames::new();
        choose(
            &mut frames,
            &mut app,
            "Restore file from this commit",
            |cx, app, state| {
                commit_file_menu(cx, app, state, &base, &change(path, FileStatus::Added))
            },
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.active().unwrap().busy() {
            assert!(
                std::time::Instant::now() < deadline,
                "restore did not finish"
            );
            app.poll_async();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            std::fs::read_to_string(repo.workdir().join(path)).unwrap(),
            "Original\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.workdir().join(other)).unwrap(),
            "Second revision\n"
        );
        assert_eq!(app.active().unwrap().status.staged[0].path, path);
        assert_eq!(
            app.active().unwrap().history.head_sha.as_deref(),
            Some(head.as_str())
        );

        choose(&mut frames, &mut app, "File Blame", |cx, app, state| {
            commit_file_menu(cx, app, state, &base, &change(path, FileStatus::Added))
        });
        let view = app.active().unwrap().file_history.as_ref().unwrap();
        assert_eq!(view.path, path);
        assert_eq!(view.from.as_deref(), Some(base.as_str()));
        assert!(view.blame);
        choose(&mut frames, &mut app, "Show in graph", |cx, app, state| {
            file_history_commit_menu(cx, app, state, &head, path)
        });
        let state = app.active().unwrap();
        assert!(state.file_history.is_none());
        assert_eq!(state.selected, [head]);
    }

    #[test]
    fn conflict_menus_open_text_conflicts_and_fall_back_to_the_binary_diff() {
        let (_scratch, repo, mut app, base) = file_fixture();
        let branch = repo
            .load_history(&imgit_git::HistoryOptions::default())
            .unwrap()
            .head_branch
            .unwrap();
        let text = "src/first.txt";
        let binary = "image.bin";
        let commit_side = |label, bytes| {
            std::fs::write(repo.workdir().join(text), label).unwrap();
            std::fs::write(repo.workdir().join(binary), bytes).unwrap();
            repo.stage(&[text, binary]).unwrap();
            repo.commit(label, false).unwrap()
        };
        let ours = commit_side("our version", &[0u8, 1][..]);
        repo.create_branch("incoming", &base, true).unwrap();
        let theirs = commit_side("their version", &[0u8, 2][..]);
        repo.checkout_branch(&branch).unwrap();
        assert_eq!(
            repo.merge("incoming", false).unwrap(),
            imgit_git::repository::Outcome::Conflicts
        );
        app.open_repository(&repo.workdir().to_string_lossy());
        let mut frames = TestFrames::new();

        choose(
            &mut frames,
            &mut app,
            "Open in merge tool",
            |cx, app, state| {
                let conflict = state
                    .status
                    .conflicted
                    .iter()
                    .find(|change| change.path == text)
                    .unwrap()
                    .clone();
                let items = workdir_file_menu(cx, app, state, &conflict, false);
                assert!(items.iter().all(|item| !matches!(
                    item.label.as_str(),
                    "Stash file" | "Discard changes" | "Stage"
                )));
                items
            },
        );
        let tool = app.active().unwrap().merge_tool.as_ref().unwrap();
        assert_eq!(tool.path, text);
        assert_eq!(tool.ours_sha, ours);
        assert_eq!(tool.theirs_sha, theirs);

        choose(
            &mut frames,
            &mut app,
            "Open in merge tool",
            |cx, app, state| {
                workdir_file_menu(
                    cx,
                    app,
                    state,
                    &change(binary, FileStatus::Conflicted),
                    false,
                )
            },
        );
        assert_eq!(
            app.toasts.history()[0].title,
            "Could not open the merge tool"
        );
        let target = app.active().unwrap().diff_target.as_ref().unwrap();
        assert_eq!(target.path, binary);
        assert_eq!(target.source, DiffSource::Unstaged);

        std::fs::write(repo.workdir().join(text), "merged content\n").unwrap();
        choose(&mut frames, &mut app, "Mark resolved", |cx, app, state| {
            workdir_file_menu(cx, app, state, &change(text, FileStatus::Conflicted), false)
        });
        let state = app.active().unwrap();
        assert_eq!(state.status.conflicted.len(), 1);
        assert_eq!(state.status.conflicted[0].path, binary);
        assert!(state.status.staged.iter().any(|change| change.path == text));
        assert_eq!(
            std::fs::read_to_string(repo.workdir().join(text)).unwrap(),
            "merged content\n"
        );
    }

    #[test]
    fn editor_and_delete_actions_target_the_file_the_menu_was_opened_for() {
        let (_scratch, repo, mut app, _) = file_fixture();
        let path = "src/first.txt";
        let file = repo.workdir().join(path);
        std::fs::write(&file, "Edited\n").unwrap();
        app.open_repository(&repo.workdir().to_string_lossy());
        let mut frames = TestFrames::new();
        let menu = |cx: &mut Cx<'_>, app: &mut Application, state: &mut RepoState| {
            workdir_file_menu(cx, app, state, &change(path, FileStatus::Modified), false)
        };
        choose(&mut frames, &mut app, "Edit file", menu);
        let editor = app.active().unwrap().file_edit.as_ref().unwrap();
        assert_eq!(editor.path, path);
        assert_eq!(editor.text, "Edited\n");
        choose(&mut frames, &mut app, "Delete file", menu);
        assert!(app.dialog.is(DialogKind::Confirm));
        assert!(app.dialog.danger);
        assert!(app.dialog.message.contains(path));
        assert!(file.is_file());

        let confirmed = app.dialog.on_confirm.take().unwrap();
        app.dialog.close();
        frames.draw(|cx| confirmed(cx, &mut app));
        assert!(!file.exists());
        let state = app.active().unwrap();
        assert!(
            state
                .status
                .unstaged
                .iter()
                .any(|change| change.path == path && change.status == FileStatus::Deleted)
        );
        assert_eq!(
            std::fs::read_to_string(repo.workdir().join("src/second.txt")).unwrap(),
            "Original\n"
        );

        std::fs::write(&file, "Recreated\n").unwrap();
        choose(&mut frames, &mut app, "Delete file", menu);
        let confirmed = app.dialog.on_confirm.take().unwrap();
        app.dialog.close();
        std::fs::remove_file(&file).unwrap();
        frames.draw(|cx| confirmed(cx, &mut app));
        assert_eq!(
            app.toasts.history()[0].title,
            "Could not delete src/first.txt"
        );

        frames.draw(|cx| {
            app.with_active(|app, state| {
                let items = menu(cx, app, state);
                assert!(items.iter().all(|item| !matches!(
                    item.label.as_str(),
                    "Edit file" | "Delete file" | "Open file in default program"
                )));
            })
            .unwrap();
        });
    }

    #[test]
    fn bulk_ignore_appends_only_selected_paths_and_reports_noop_or_write_error() {
        let (_scratch, repo, mut app, _) = file_fixture();
        let paths = vec!["src/generated.log".to_owned(), "outside.tmp".to_owned()];
        for path in &paths {
            std::fs::write(repo.workdir().join(path), "Generated\n").unwrap();
        }
        let file = repo.workdir().join(".gitignore");
        std::fs::create_dir(&file).unwrap();
        app.open_repository(&repo.workdir().to_string_lossy());
        let mut frames = TestFrames::new();
        let invoke = |frames: &mut TestFrames, app: &mut Application| {
            choose(frames, app, "Ignore 2 files", |cx, app, state| {
                multiple_files_menu(cx, app, state, paths.clone(), false)
            });
        };
        invoke(&mut frames, &mut app);
        assert_eq!(app.toasts.history()[0].title, "Could not write .gitignore");
        assert_eq!(app.toasts.history()[0].kind, ToastKind::Error);
        std::fs::remove_dir(&file).unwrap();

        invoke(&mut frames, &mut app);
        let expected = "src/generated.log\n/outside.tmp\n";
        assert_eq!(std::fs::read_to_string(&file).unwrap(), expected);
        assert_eq!(app.toasts.history()[0].title, "Added to .gitignore");
        assert!(
            app.active()
                .unwrap()
                .status
                .unstaged
                .iter()
                .all(|change| !paths.contains(&change.path))
        );
        invoke(&mut frames, &mut app);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), expected);
        assert_eq!(app.toasts.history()[0].title, "Already in .gitignore");
        assert_eq!(app.toasts.history()[0].kind, ToastKind::Info);
    }

    #[test]
    fn single_file_and_folder_stashes_only_include_the_selected_paths() {
        let (_scratch, repo, mut app, base) = file_fixture();
        let paths = ["src/first.txt", "src/second.txt", "outside.txt"];
        for path in paths {
            std::fs::write(repo.workdir().join(path), "Modified\n").unwrap();
        }
        app.open_repository(&repo.workdir().to_string_lossy());
        let mut frames = TestFrames::new();
        choose(&mut frames, &mut app, "Stash file", |cx, app, state| {
            workdir_file_menu(
                cx,
                app,
                state,
                &change(paths[0], FileStatus::Modified),
                false,
            )
        });
        finish_file_command(&mut app, "Stash file");
        let read = |path| std::fs::read_to_string(repo.workdir().join(path)).unwrap();
        assert_eq!(read(paths[0]), "Original\n");
        assert_eq!(read(paths[1]), "Modified\n");
        assert_eq!(read(paths[2]), "Modified\n");

        choose(&mut frames, &mut app, "Stash folder", |cx, app, state| {
            folder_menu(
                cx,
                app,
                state,
                "src",
                paths[..2].iter().map(|path| (*path).to_owned()).collect(),
                false,
                true,
            )
        });
        finish_file_command(&mut app, "Stash folder");
        assert_eq!(read(paths[1]), "Original\n");
        assert_eq!(read(paths[2]), "Modified\n");
        let state = app.active().unwrap();
        let stashes: Vec<_> = state
            .history
            .refs
            .iter()
            .filter(|r| r.ref_type == imgit_git::RefType::Stash)
            .collect();
        assert_eq!(stashes.len(), 2);
        let first = stashes
            .iter()
            .find(|stash| stash.stash_index == Some(1))
            .unwrap()
            .target
            .clone();
        let second = stashes
            .iter()
            .find(|stash| stash.stash_index == Some(0))
            .unwrap()
            .target
            .clone();

        choose(
            &mut frames,
            &mut app,
            "Apply stashed file",
            |cx, app, state| {
                commit_file_menu(
                    cx,
                    app,
                    state,
                    &first,
                    &change(paths[0], FileStatus::Modified),
                )
            },
        );
        finish_file_command(&mut app, "Apply stashed file");
        assert_eq!(read(paths[0]), "Modified\n");
        assert_eq!(read(paths[1]), "Original\n");
        assert_eq!(read(paths[2]), "Modified\n");
        let state = app.active().unwrap();
        assert_eq!(state.history.head_sha.as_deref(), Some(base.as_str()));
        assert_eq!(state.repo.stash_index_of(&first), Some(1));
        assert_eq!(state.repo.stash_index_of(&second), Some(0));
    }

    fn finish_file_command(app: &mut Application, title: &str) {
        let operation = app.active().unwrap().async_op.as_ref().unwrap();
        assert_eq!(operation.title, title);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while app.active().unwrap().busy() {
            assert!(
                std::time::Instant::now() < deadline,
                "{title} did not finish"
            );
            app.poll_async();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_ne!(
            app.toasts.history()[0].kind,
            ToastKind::Error,
            "{:?}",
            app.toasts.history()[0]
        );
    }

    fn change(path: &str, status: FileStatus) -> FileChange {
        FileChange {
            path: path.to_owned(),
            old_path: None,
            status,
            stats: None,
        }
    }

    #[test]
    fn staged_new_files_are_discarded_as_untracked_ones() {
        let converted = as_unstaged([
            change("new.txt", FileStatus::Added),
            change("old.txt", FileStatus::Modified),
        ]);
        assert_eq!(converted[0].status, FileStatus::Untracked);
        assert_eq!(converted[1].status, FileStatus::Modified);
    }

    #[test]
    fn discarding_a_staged_new_file_deletes_it() {
        let scratch = tempfile::tempdir().unwrap();
        let folder = scratch.path().join("repo");
        std::fs::create_dir_all(&folder).unwrap();
        let repo = imgit_git::Repository::init(&folder).unwrap();
        repo.set_local_config("user.name", Some("Test")).unwrap();
        repo.set_local_config("user.email", Some("test@example.com"))
            .unwrap();
        repo.set_local_config("core.autocrlf", Some("false"))
            .unwrap();
        std::fs::write(
            folder.join("old.txt"),
            "old
",
        )
        .unwrap();
        repo.stage(&["old.txt"]).unwrap();
        repo.commit("first", false).unwrap();
        std::fs::write(
            folder.join("new.txt"),
            "new
",
        )
        .unwrap();
        std::fs::write(
            folder.join("old.txt"),
            "changed
",
        )
        .unwrap();
        repo.stage(&["new.txt", "old.txt"]).unwrap();
        let mut app = Application::with_config_directory(scratch.path().join("config"));
        app.open_repository(&folder.to_string_lossy());
        app.with_active(|app, state| {
            state.refresh();
            discard_paths(
                app,
                state,
                &["new.txt".to_owned(), "old.txt".to_owned()],
                true,
            );
            assert!(!folder.join("new.txt").exists());
            assert_eq!(
                std::fs::read_to_string(folder.join("old.txt")).unwrap(),
                "old
"
            );
            assert!(state.status.is_empty());
        });
    }

    #[test]
    fn ignoring_appends_one_line_per_pattern() {
        let scratch = tempfile::tempdir().unwrap();
        append_to_gitignore(scratch.path(), &["/a.txt".to_owned()]).unwrap();
        append_to_gitignore(scratch.path(), &["*.log".to_owned(), "build/".to_owned()]).unwrap();
        let text = std::fs::read_to_string(scratch.path().join(".gitignore")).unwrap();
        assert_eq!(text, "/a.txt\n*.log\nbuild/\n");
    }

    #[test]
    fn a_pattern_goes_on_its_own_line_whatever_the_file_ends_with() {
        let lines = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        // The last line has no line break: one is added, not glued to the pattern.
        let (added, count) = gitignore_additions(b"target", &lines(&["/a.txt"]));
        assert_eq!((added.as_slice(), count), (&b"\n/a.txt\n"[..], 1));
        // The file's own CRLF is kept, a missing last one is made up in the same style.
        let (added, _) = gitignore_additions(b"one\r\ntwo", &lines(&["/a", "b/"]));
        assert_eq!(added, b"\r\n/a\r\nb/\r\n");
        let (added, _) = gitignore_additions(b"one\r\ntwo\r\n", &lines(&["/a"]));
        assert_eq!(added, b"/a\r\n");
        // An empty file, or none, needs no leading break.
        assert_eq!(gitignore_additions(b"", &lines(&["/a"])).0, b"/a\n");
    }

    #[test]
    fn a_pattern_that_is_there_already_is_not_added_again() {
        let lines = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        let existing = b"/a.txt\r\nlog.txt\n*.o";
        // Exact, as part of the request twice, or covered by the unanchored form of a root file.
        let (added, count) = gitignore_additions(
            existing,
            &lines(&["/a.txt", "*.o", "/log.txt", "new/", "new/"]),
        );
        assert_eq!((added.as_slice(), count), (&b"\r\nnew/\r\n"[..], 1));
        assert_eq!(
            gitignore_additions(existing, &lines(&["/a.txt"])),
            (Vec::new(), 0)
        );
        // A nested path is not covered by a root file of the same last name.
        assert_eq!(
            gitignore_additions(b"log.txt\n", &lines(&["/src/log.txt"])).1,
            1
        );
        // Reported by the file append: nothing changes on disk.
        let scratch = tempfile::tempdir().unwrap();
        std::fs::write(scratch.path().join(".gitignore"), "/a.txt\n").unwrap();
        assert_eq!(
            append_to_gitignore(scratch.path(), &lines(&["/a.txt"])).unwrap(),
            0
        );
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(".gitignore")).unwrap(),
            "/a.txt\n"
        );
    }

    #[test]
    fn ignore_lines_match_that_file_only() {
        // A root file is anchored, so a file of that name in a folder stays tracked.
        assert_eq!(ignore_line("a.txt", false).as_deref(), Some("/a.txt"));
        // A path with a folder in it is anchored by git already.
        assert_eq!(
            ignore_line("src/a.txt", false).as_deref(),
            Some("src/a.txt")
        );
        assert_eq!(
            ignore_line("src\\ui\\a.txt", false).as_deref(),
            Some("src/ui/a.txt")
        );
        assert_eq!(ignore_line("src", true).as_deref(), Some("/src/"));
        assert_eq!(ignore_line("a/b", true).as_deref(), Some("a/b/"));
        // Syntax in the name is escaped.
        assert_eq!(ignore_line("#notes", false).as_deref(), Some("/\\#notes"));
        assert_eq!(
            ignore_line("!keep.txt", false).as_deref(),
            Some("/\\!keep.txt")
        );
        assert_eq!(
            ignore_line("a[1]*?.txt", false).as_deref(),
            Some("/a\\[1]\\*\\?.txt")
        );
        assert_eq!(ignore_line("dir/a#b", false).as_deref(), Some("dir/a#b"));
        assert_eq!(
            ignore_line("trail  ", false).as_deref(),
            Some("/trail\\ \\ ")
        );
        assert_eq!(
            ignore_line("back\\slash", false).as_deref(),
            Some("back/slash")
        );
        // No line can name these.
        assert_eq!(ignore_line("", false), None);
        assert_eq!(ignore_line("/", true), None);
        assert_eq!(ignore_line("a\nb", false), None);
        // The extension keeps its glob and escapes the rest.
        assert_eq!(ignore_extension_line("rs").as_deref(), Some("*.rs"));
        assert_eq!(ignore_extension_line("a[b").as_deref(), Some("*.a\\[b"));
        assert_eq!(ignore_extension_line(""), None);
    }

    #[test]
    fn git_ignores_exactly_what_the_lines_name() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let git = |args: &[&str]| {
            let output = imgit_git::cli::run(dir, args, imgit_git::cli::RunOptions::default());
            (output.exit_code, output.text)
        };
        assert_eq!(git(&["init", "-q"]).0, 0);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        // (No file called "star*": Windows has none, and `check-ignore` does not need one.)
        for name in ["a.txt", "sub/a.txt", "#hash", "[x]"] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        let lines: Vec<String> = ["a.txt", "#hash", "[x]", "star*"]
            .iter()
            .filter_map(|name| ignore_line(name, false))
            .collect();
        append_to_gitignore(dir, &lines).unwrap();
        let ignored = |path: &str| git(&["check-ignore", "-q", "--", path]).0 == 0;
        assert!(ignored("a.txt"));
        // The copy in the folder is another file.
        assert!(!ignored("sub/a.txt"));
        assert!(ignored("#hash") && ignored("[x]") && ignored("star*"));
        // The brackets did not become a character class matching "x".
        std::fs::write(dir.join("x"), "x").unwrap();
        assert!(!ignored("x"));
    }

    #[test]
    fn the_ignore_submenu_offers_file_extension_and_folder() {
        let labels = |path: &str| -> Vec<String> {
            ignore_submenu(Path::new("."), path)
                .submenu
                .into_iter()
                .map(|item| item.label)
                .collect()
        };
        assert_eq!(
            labels("src/main.rs"),
            [
                "Ignore 'main.rs'",
                "All files with the extension '.rs'",
                "All files in 'src/'"
            ]
        );
        assert_eq!(labels("Makefile"), ["Ignore 'Makefile'"]);
        assert_eq!(labels(".gitignore"), ["Ignore '.gitignore'"]);
        assert_eq!(
            labels("a/b/c.txt"),
            [
                "Ignore 'c.txt'",
                "All files with the extension '.txt'",
                "All files in 'b/'"
            ]
        );
    }

    #[test]
    fn diff_and_stash_arguments_end_with_the_paths() {
        let paths = vec!["a".to_owned(), "b c".to_owned()];
        assert_eq!(diff_args(false, &paths), ["diff", "--", "a", "b c"]);
        assert_eq!(
            diff_args(true, &paths),
            ["diff", "--cached", "--", "a", "b c"]
        );
        assert_eq!(stash_args(&paths), ["stash", "push", "--", "a", "b c"]);
    }
}
