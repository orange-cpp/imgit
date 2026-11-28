//! `AppFrame`: the application as a [`Frame`] of the shell: it owns the application, the themes and the UI state,
//! builds each frame's [`Cx`], and carries out the dev commands (`--select-row`, `--open-file`).

use dear_imgui_rs::Ui;
use imgit_app::repo_state::{DiffSource, DiffTarget};
use imgit_git::{FileStatus, NodeType};

use crate::app::application::Application;
use crate::dev::DevCommand;
use crate::shell::{Frame, ShellContext};
use crate::theme::Themes;
use crate::ui::panels::menus::text_field_menu;
use crate::ui::{Cx, UiState};

/// The application on the shell's window.
pub struct AppFrame {
    app: Application,
    themes: Themes,
    ui_state: UiState,
}

impl AppFrame {
    /// Starts the application on `shell`'s window: applies the saved zoom and theme, and opens the repository
    /// named on the command line, else the remembered tabs, else the most recent repository (as `main()` of the
    /// C++ build did).
    #[must_use]
    pub fn new(mut app: Application, shell: &mut ShellContext<'_>) -> Self {
        let mut ui_state = UiState::new();
        ui_state.text_field_menu = Some(text_field_menu);
        ui_state
            .avatars
            .set_cache_directory(Some(app.app_path("gravatars")));
        // The saved zoom and theme (custom themes first) apply with the first frame.
        app.request_custom_themes(&mut ui_state, false);
        app.apply_settings_to(&mut ui_state);

        let repositories_timing = imgit_app::startup::span("repositories.restore");
        let options = shell.options;
        match &options.repo {
            Some(repo) => {
                // Only this repository is open: the remembered tabs must survive this session.
                app.started_with_repository = true;
                app.open_repository(&repo.to_string_lossy());
            }
            None if app.settings.remember_tabs => app.restore_tabs(),
            None => {}
        }
        if options.terminal {
            app.show_terminal_panel = true;
        }
        if app.tabs.is_empty()
            && options.repo.is_none()
            && let Some(recent) = app.recent_repos.first().cloned()
        {
            app.open_repository(&recent.to_string_lossy());
        }
        // A missing or changed saved path may leave an unused preload. Never reuse it for a
        // repository opened later in the session (or after a profile switch).
        app.startup_repositories.clear();
        drop(repositories_timing);
        if app.active().is_some() {
            imgit_app::startup::mark("repository.active_ready");
        }
        Self {
            app,
            themes: Themes::new(),
            ui_state,
        }
    }
}

impl Frame for AppFrame {
    fn draw(&mut self, ui: &Ui, shell: &mut ShellContext<'_>) {
        self.ui_state.begin_frame(ui, &mut self.themes, shell);
        let mut cx = Cx::new(ui, shell, self.themes.palette(), &mut self.ui_state);
        self.app.draw_frame(&mut cx);
    }

    fn dev_command(&mut self, command: DevCommand, _shell: &mut ShellContext<'_>) {
        let Some(state) = self.app.active_mut() else {
            return;
        };
        match command {
            DevCommand::SelectRow(row) => {
                if let Some(sha) = state
                    .history
                    .commits
                    .get(row)
                    .map(|commit| commit.sha.clone())
                {
                    state.select_only(&sha);
                    state.scroll_to_row = Some(row);
                }
            }
            DevCommand::OpenFile {
                path,
                file_history,
                blame,
            } => {
                let sha = state
                    .single_selected()
                    .filter(|commit| commit.node_type != NodeType::WorkDir)
                    .map(|commit| commit.sha.clone());
                if file_history {
                    state.open_file_history(path, sha.clone(), sha, blame);
                } else if let Some(sha) = sha {
                    let _ = state.open_diff(DiffTarget {
                        source: DiffSource::Commit(sha),
                        path,
                        status: FileStatus::Modified,
                    });
                } else {
                    let _ = state.open_diff(DiffTarget {
                        source: DiffSource::Unstaged,
                        path,
                        status: FileStatus::Modified,
                    });
                }
            }
        }
    }

    /// Screenshots wait for the avatar downloads and the File History view's background jobs.
    fn busy(&self) -> bool {
        self.ui_state.busy()
            || self.app.panels.graph_view.busy() // the CHANGES column's counts
            || self.app.active().is_some_and(|state| {
                state
                    .file_history
                    .as_ref()
                    .is_some_and(|view| view.history_loading || view.blame_loading)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imgit_git::Repository;

    use crate::dev::DevOptions;
    use crate::shell::test_frame::TestFrames;

    #[test]
    fn startup_and_dev_commands_open_the_requested_repository_revision_and_path() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = Repository::init(scratch.path().join("repo")).unwrap();
        repo.set_local_config("user.name", Some("Startup test"))
            .unwrap();
        repo.set_local_config("user.email", Some("startup@example.invalid"))
            .unwrap();
        let path = "tracked.txt";
        std::fs::write(repo.workdir().join(path), "Before\n").unwrap();
        repo.stage(&[path]).unwrap();
        let sha = repo.commit("Add tracked file", false).unwrap();
        std::fs::write(repo.workdir().join(path), "After\n").unwrap();

        let app = Application::with_config_directory(scratch.path().join("config"));
        let options = DevOptions {
            repo: Some(repo.workdir().to_owned()),
            ..DevOptions::default()
        };
        let mut frames = TestFrames::with_options(options);
        let mut application_frame = None;
        frames.draw(|cx| {
            let mut created = AppFrame::new(app, &mut cx.shell);
            created.ui_state.avatars.set_enabled(false);
            created.draw(cx.ui, &mut cx.shell);
            application_frame = Some(created);
        });
        let mut app_frame = application_frame.unwrap();
        assert_eq!(app_frame.app.active_workdir(), Some(repo.workdir()));
        assert!(app_frame.app.started_with_repository);
        let state = app_frame.app.active().unwrap();
        let row = state.history.row_by_sha[&sha];
        let workdir = state.history.row_by_sha[imgit_app::WIP_SHA];

        frames.draw(|cx| {
            app_frame.dev_command(DevCommand::SelectRow(row), &mut cx.shell);
            app_frame.dev_command(
                DevCommand::OpenFile {
                    path: path.to_owned(),
                    file_history: false,
                    blame: false,
                },
                &mut cx.shell,
            );
        });
        let committed = app_frame.app.active().unwrap();
        assert_eq!(committed.selected, [sha.as_str()]);
        assert_eq!(
            committed.diff_target.as_ref().unwrap().source,
            DiffSource::Commit(sha.clone())
        );

        frames.draw(|cx| {
            app_frame.dev_command(DevCommand::SelectRow(workdir), &mut cx.shell);
            app_frame.dev_command(
                DevCommand::OpenFile {
                    path: path.to_owned(),
                    file_history: false,
                    blame: false,
                },
                &mut cx.shell,
            );
        });
        let working_copy = app_frame.app.active().unwrap();
        assert_eq!(
            working_copy.diff_target.as_ref().unwrap().source,
            DiffSource::Unstaged
        );

        frames.draw(|cx| {
            app_frame.dev_command(DevCommand::SelectRow(row), &mut cx.shell);
            app_frame.dev_command(
                DevCommand::OpenFile {
                    path: path.to_owned(),
                    file_history: true,
                    blame: true,
                },
                &mut cx.shell,
            );
        });
        let history = app_frame
            .app
            .active()
            .unwrap()
            .file_history
            .as_ref()
            .unwrap();
        assert_eq!(history.path, path);
        assert_eq!(history.from.as_deref(), Some(sha.as_str()));
        assert!(history.blame);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app_frame.busy() {
            assert!(
                std::time::Instant::now() < deadline,
                "file history did not settle"
            );
            frames.draw(|cx| app_frame.draw(cx.ui, &mut cx.shell));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let history = app_frame
            .app
            .active()
            .unwrap()
            .file_history
            .as_ref()
            .unwrap();
        assert_eq!(history.history.entries[0].commit.sha, sha);
        assert_eq!(history.blame_data.as_ref().unwrap().hunks[0].sha, sha);
    }
}
