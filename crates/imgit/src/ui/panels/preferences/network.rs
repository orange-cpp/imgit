//! The SSH and Connections pages.

use imgit_app::connections::checks::forget_stored_credential;
use imgit_app::connections::{Check, Connection, parse_remote_url, service_of};
use imgit_app::pref;

use super::controls::{BTN, Tone};
use super::form::{Form, Row, browse_row, help, paragraph, row, stored_checkbox, title, value_of};
use super::page::{BoxStyle, Face, HELP, LABEL, Page, Style};
use super::store::generate_ssh_key;
use crate::app::dialog::DialogKind;
use crate::app::toasts::ToastKind;
use crate::theme::Color;

pub(super) fn ssh_page(page: &mut Page<'_, '_>) {
    let mut form = Form::new(page);
    let form = &mut form;
    title(page, form, "SSH");
    row(page, form, "Use local SSH agent", |page, r| {
        stored_checkbox(page, r, "##ssh_agent", pref::SSH_AGENT, "");
        help(
            page,
            r,
            "ssh takes its keys from the running SSH agent and your ssh config instead of the private key \
             below.",
        );
    });
    // The path after the button sits on an inline line box half a pixel taller than the button.
    row(page, form, "SSH Private Key", |page, r| {
        browse_row(
            page,
            r,
            pref::SSH_PRIVATE_KEY,
            "None: ssh's own configuration and keys",
            "",
        );
        r.column = 31.64;
        help(
            page,
            r,
            "Git's network operations use only this key (unless the SSH agent is used).",
        );
    });
    row(page, form, "SSH Public Key", |page, r| {
        let mut path = value_of(pref::SSH_PUBLIC_KEY);
        let right = browse_row(page, r, pref::SSH_PUBLIC_KEY, "None", "pub");
        if path.is_empty() && !value_of(pref::SSH_PRIVATE_KEY).is_empty() {
            path = format!("{}.pub", value_of(pref::SSH_PRIVATE_KEY));
        }
        if !path.is_empty() {
            // The copy button: btn-default with a clipboard (18px).
            let copy_x = right + 5.0;
            let ink = page.ink(0.75);
            page.boxed(
                copy_x,
                r.top - 0.5,
                40.0,
                32.140_625,
                BoxStyle::new(BoxStyle::CLEAR, ink, 2.0),
            );
            page.svg(
                "clipboard",
                384.0,
                512.0,
                copy_x + 11.0,
                r.top + 6.5,
                13.5,
                18.0,
                ink,
            );
            if page.hit(
                "##copy_public_key",
                copy_x,
                r.top - 0.5,
                40.0,
                32.140_625,
                true,
            ) {
                match std::fs::read_to_string(&path) {
                    Ok(key) if !key.is_empty() => page.app.copy_to_clipboard(page.cx, &key),
                    _ => page.app.toasts.push_with_detail(
                        ToastKind::Error,
                        "Could not read the public key",
                        path.clone(),
                    ),
                }
            }
        }
        r.column = 31.64;
    });
    row(page, form, "Generate new Private/Public key", |page, r| {
        if page.button(
            "##generate_ssh",
            r.x(),
            r.top,
            "Generate",
            Tone::Success,
            BTN,
            false,
        ) {
            generate_ssh_key(page.app);
        }
        r.column = 31.140_625;
        help(
            page,
            r,
            "This will create new keys and configure ImGit Desktop to use them",
        );
    });
    row(
        page,
        form,
        "Use default Git Credential Manager",
        |page, r| {
            stored_checkbox(
                page,
                r,
                "##credential_manager",
                pref::CREDENTIAL_MANAGER,
                "",
            );
            help(
                page,
                r,
                "Lets the credential manager in your Git config show its own sign-in window. Off, ImGit asks for \
             usernames and passwords itself (the credential manager still answers with the ones it has \
             stored).",
            );
        },
    );
}

/// A connection's header: an icon, the service's name and, for a known service, its host after it.
fn connection_heading(page: &mut Page<'_, '_>, form: &mut Form, connection: &Connection) {
    let top = form.place(25.0);
    let style = Style::ink(18.0, 0.75).with_face(Face::Semibold);
    let known = service_of(&connection.host).is_some();
    let dim = page.ink(0.6);
    page.icon(
        if known { "far:cloud" } else { "far:globe" },
        form.left,
        top + 1.0,
        18.0,
        18.0,
        18.0,
        dim,
    );
    let w = page.text(
        form.left + 28.0,
        top + Page::leading(style, 19.796_875),
        style,
        &connection.name,
    );
    if connection.name != connection.host {
        page.text(
            form.left + 28.0 + w + 10.0,
            top + 4.0,
            HELP,
            &connection.host,
        );
    }
    form.advance(page, top, 19.796_875, 12.0);
}

/// A check's status in its row: a dot, what it found, and a line about it under them (`first_line` down: lower
/// when a button sits beside the status). Returns the status text's right edge (CSS px).
fn connection_status(page: &mut Page<'_, '_>, r: &mut Row, check: &Check, first_line: f32) -> f32 {
    let (green, blue) = (page.cx.palette.green, page.cx.palette.accent);
    let (dot, status): (Color, String) = match check {
        Check::Connected { user, .. } => (
            green,
            user.as_deref().filter(|user| !user.is_empty()).map_or_else(
                || "Connected".to_owned(),
                |user| format!("Connected as {user}"),
            ),
        ),
        Check::NotConnected { .. } => (page.ink(0.35), "Not connected".to_owned()),
        Check::Checking => (blue, "Checking...".to_owned()),
        Check::Unchecked => (page.ink(0.35), "Not checked".to_owned()),
    };
    let centre = page.at(r.x() + 5.0, r.top + 17.0);
    page.draw.add_circle_filled(centre, page.px(4.0), dot, 0);
    let style = if check.is_connected() {
        Style::ink(14.0, 0.85).with_face(Face::Semibold)
    } else {
        LABEL
    };
    let w = page.text(
        r.x() + 16.0,
        r.top + 7.0 + Page::leading(style, 20.0),
        style,
        &status,
    );
    r.column = first_line;
    if let Some(detail) = check.detail().filter(|detail| !detail.is_empty()) {
        // Plain text (ssh's own words may hold markup characters), one line.
        page.text_truncated(
            r.x() + 16.0,
            r.top + r.column + Page::leading(HELP, 17.140_625),
            HELP,
            detail,
            (r.control_width() - 46.0).max(100.0),
        );
        r.column += 17.140_625;
    }
    r.x() + 16.0 + w
}

pub(super) fn connections_page(page: &mut Page<'_, '_>) {
    {
        let app = &mut *page.app;
        let mut remotes: Vec<(String, String)> = Vec::new();
        for tab in &app.tabs {
            if let Some(state) = tab.state.as_deref() {
                for remote in &state.history.remotes {
                    remotes.push((state.repo.name().to_owned(), remote.url.clone()));
                }
            }
        }
        app.connections.update_hosts(&remotes);
        app.connections.poll();
        app.connections.check(&app.credentials, false);
    }

    let mut form = Form::new(page);
    let form = &mut form;
    title(page, form, "Connections");
    paragraph(
        page,
        form,
        "The hosting services git can reach from this computer and the accounts it uses there: HTTPS \
         usernames and tokens (entered in ImGit, or kept by your Git credential helper) and SSH keys the \
         service accepts. Other servers appear here when an open repository has a remote on them.",
        10.0,
    );
    {
        let top = form.place(0.0);
        let busy = page.app.connections.checking();
        if page.button(
            "##refresh_connections",
            form.left,
            top,
            if busy { "Checking..." } else { "Check Again" },
            Tone::Primary,
            BTN,
            busy,
        ) {
            let app = &mut *page.app;
            app.connections.check(&app.credentials, true);
        }
        form.advance(page, top, 31.140_625, 10.0);
    }

    let connections: Vec<Connection> = page.app.connections.list().to_vec();
    for connection in &connections {
        draw_connection(page, form, connection);
    }
}

fn draw_connection(page: &mut Page<'_, '_>, form: &mut Form, connection: &Connection) {
    let _id = page.cx.ui.push_id(connection.host.as_str());
    connection_heading(page, form, connection);
    if connection.https {
        row(page, form, "HTTPS", |page, r| {
            let check = &connection.https_check;
            let connected = check.is_connected();
            let right = connection_status(page, r, check, if connected { 34.0 } else { 27.0 });
            if !connected {
                return;
            }
            // Forget: this session's credential at once; a stored one after a confirmation.
            if page.button(
                "##forget",
                right + 15.0,
                r.top + 1.5,
                "Forget",
                Tone::Default,
                BTN,
                false,
            ) {
                forget(page, connection, check.user().unwrap_or_default());
            }
        });
    }
    if let Some(ssh) = &connection.ssh {
        row(page, form, "SSH", |page, r| {
            connection_status(page, r, &connection.ssh_check, 27.0);
            let target = match &ssh.port {
                Some(port) => format!("{}:{port}", ssh.target),
                None => ssh.target.clone(),
            };
            page.text(
                r.x() + 16.0,
                r.top + r.column + Page::leading(HELP, 17.140_625),
                HELP,
                &format!("Checked with ssh -T {target}"),
            );
            r.column += 17.140_625;
        });
    }
    if !connection.repos.is_empty() {
        row(page, form, "Open repositories", |page, r| {
            let names = connection.repos.join(", ");
            page.text_truncated(
                r.x(),
                r.top + 7.0 + Page::leading(LABEL, 20.0),
                LABEL,
                &names,
                (r.control_width() - 30.0).max(100.0),
            );
            r.column = 27.0;
        });
    }
}

/// Forget: this session's credential at once; a stored one after a confirmation.
fn forget(page: &mut Page<'_, '_>, connection: &Connection, user: &str) {
    let host = connection.host.clone();
    let user = user.to_owned();
    let app = &mut *page.app;
    if connection.session_credential {
        app.credentials.retain(|credential| {
            !parse_remote_url(&format!("{}/", credential.url))
                .is_some_and(|parsed| parsed.host == host)
        });
        app.toasts.push_with_detail(
            ToastKind::Info,
            "Connections",
            format!("Forgot the credentials entered for {host} this session."),
        );
        return;
    }
    let d = &mut app.dialog;
    d.open(DialogKind::Confirm);
    d.title = "Forget stored credentials".to_owned();
    d.message = format!(
        "Remove {user}'s credentials for {host} from your Git credential helper? Git will ask for them the \
         next time it connects over HTTPS."
    );
    d.confirm_label = "Forget".to_owned();
    d.danger = true;
    d.on_confirm = Some(Box::new(move |_cx, app| {
        forget_stored_credential(&host, &user);
        if let Some(connection) = app.connections.find_mut(&host) {
            connection.https_check = Check::default();
        }
        app.toasts.push_with_detail(
            ToastKind::Info,
            "Connections",
            format!("Removed the stored credentials for {host}."),
        );
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::application::Application;
    use crate::shell::test_frame::TestFrames;
    use crate::ui::Vec2;
    use imgit_app::connections::SshTarget;
    use imgit_git::{Repository, cli::Credential};

    #[test]
    fn connections_page_draws_known_and_custom_endpoints_with_their_status() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = Repository::init(scratch.path().join("worktree")).unwrap();
        let https = "https://git.example.invalid/project/repo.git";
        let ssh = "ssh://git@git.example.invalid:2222/project/repo.git";
        repo.add_remote("origin", https).unwrap();
        repo.add_remote("deploy", ssh).unwrap();
        let mut app = Application::with_config_directory(scratch.path().join("config"));
        app.open_repository(&repo.workdir().to_string_lossy());
        app.connections
            .update_hosts(&[("worktree", https), ("worktree", ssh)]);
        let hosts: Vec<_> = app
            .connections
            .list()
            .iter()
            .map(|connection| connection.host.clone())
            .collect();
        for host in hosts {
            let connection = app.connections.find_mut(&host).unwrap();
            connection.https_check = Check::NotConnected {
                detail: "No credentials stored".to_owned(),
            };
            connection.ssh_check = Check::Connected {
                user: None,
                detail: "The server accepted this key".to_owned(),
            };
        }
        app.credentials.push(Credential {
            url: "https://github.com".to_owned(),
            username: "contributor".to_owned(),
            ..Credential::default()
        });

        let mut frames = TestFrames::new();
        frames.draw(|cx| {
            let origin = cx.cursor();
            let draw = cx.window_draw_list();
            let vertices = draw.vertex_count();
            let mut page = Page::new(cx, &mut app, draw, origin, 960.0);
            connections_page(&mut page);
            assert!(page.extent > 500.0, "each host should have its own rows");
            assert!(draw.vertex_count() > vertices);
        });
        assert!(!app.connections.checking());
        let github = app.connections.find("github.com").unwrap();
        assert_eq!(github.https_check.user(), Some("contributor"));
        assert!(github.session_credential);
        let custom = app.connections.find("git.example.invalid").unwrap();
        assert_eq!(custom.repos, ["worktree"]);
        assert_eq!(custom.ssh.as_ref().unwrap().port.as_deref(), Some("2222"));
        assert!(matches!(custom.https_check, Check::NotConnected { .. }));
        assert!(custom.ssh_check.is_connected());
    }

    #[test]
    fn forgetting_a_session_credential_keeps_other_hosts_intact() {
        let scratch = tempfile::tempdir().unwrap();
        let mut app = Application::with_config_directory(scratch.path().join("config"));
        let mut frames = TestFrames::new();
        app.credentials = [
            "https://git.example.invalid",
            "https://other.example.invalid",
        ]
        .map(|url| Credential {
            url: url.to_owned(),
            ..Credential::default()
        })
        .to_vec();
        let connection = connected_host(true);
        click_forget(&mut frames, &mut app, &connection);
        assert_eq!(app.dialog.kind, None);
        assert_eq!(app.credentials.len(), 1);
        assert_eq!(app.credentials[0].url, "https://other.example.invalid");
        let notice = &app.toasts.history()[0];
        assert_eq!(notice.kind, ToastKind::Info);
        assert_eq!(
            notice.detail,
            "Forgot the credentials entered for git.example.invalid this session."
        );
    }

    #[test]
    fn forgetting_a_stored_credential_requires_confirmation() {
        let scratch = tempfile::tempdir().unwrap();
        let mut app = Application::with_config_directory(scratch.path().join("config"));
        let mut frames = TestFrames::new();
        click_forget(&mut frames, &mut app, &connected_host(false));
        assert_eq!(app.dialog.kind, Some(DialogKind::Confirm));
        assert_eq!(app.dialog.title, "Forget stored credentials");
        assert!(
            app.dialog
                .message
                .contains("alice's credentials for git.example.invalid")
        );
        assert_eq!(app.dialog.confirm_label, "Forget");
        assert!(app.dialog.danger);
        assert!(app.dialog.on_confirm.is_some());
        assert!(app.toasts.history().is_empty());
    }

    #[test]
    fn status_rows_reserve_space_for_details_and_use_distinct_status_labels() {
        let scratch = tempfile::tempdir().unwrap();
        let mut app = Application::with_config_directory(scratch.path().join("config"));
        let mut frames = TestFrames::new();
        frames.draw(|cx| {
            let origin = cx.cursor();
            let draw = cx.window_draw_list();
            let mut page = Page::new(cx, &mut app, draw, origin, 960.0);
            let mut form = Form::new(&page);
            for (check, label, details) in [
                (Check::Unchecked, "Not checked", false),
                (Check::Checking, "Checking...", false),
                (
                    Check::NotConnected {
                        detail: "No local account".to_owned(),
                    },
                    "Not connected",
                    true,
                ),
                (
                    Check::Connected {
                        user: Some(String::new()),
                        detail: String::new(),
                    },
                    "Connected",
                    false,
                ),
            ] {
                row(&mut page, &mut form, "HTTPS", |page, r| {
                    let right = connection_status(page, r, &check, 27.0);
                    let style = if check.is_connected() {
                        Style::ink(14.0, 0.85).with_face(Face::Semibold)
                    } else {
                        LABEL
                    };
                    assert!((right - (r.x() + 16.0 + page.measure(style, label))).abs() < 0.01);
                    let expected_height = if details { 44.140_625 } else { 27.0 };
                    assert!((r.column - expected_height).abs() < 0.01);
                });
            }
        });
    }

    fn connected_host(session_credential: bool) -> Connection {
        Connection {
            name: "git.example.invalid".to_owned(),
            host: "git.example.invalid".to_owned(),
            https: true,
            ssh: Some(SshTarget {
                target: "git@git.example.invalid".to_owned(),
                port: None,
            }),
            session_credential,
            https_check: Check::Connected {
                user: Some("alice".to_owned()),
                detail: "Account connected".to_owned(),
            },
            ssh_check: Check::Unchecked,
            repos: vec!["worktree".to_owned()],
        }
    }

    fn click_forget(frames: &mut TestFrames, app: &mut Application, connection: &Connection) {
        let draw = |cx: &mut crate::ui::Cx<'_>, app: &mut Application| {
            let origin = cx.cursor();
            let draw = cx.window_draw_list();
            let mut page = Page::new(cx, app, draw, origin, 960.0);
            let mut form = Form::new(&page);
            let status_width = page.measure(
                Style::ink(14.0, 0.85).with_face(Face::Semibold),
                "Connected as alice",
            );
            let button = page.at(
                form.control_x() + 16.0 + status_width + 15.0 + 12.0,
                form.place(25.0) + 19.796_875 + 12.0 + 1.5 + 10.0,
            );
            draw_connection(&mut page, &mut form, connection);
            button
        };
        let mut button = Vec2::ZERO;
        for _ in 0..2 {
            frames.draw(|cx| button = draw(cx, app));
        }
        frames.click_left(button, |cx| {
            draw(cx, app);
        });
    }
}
