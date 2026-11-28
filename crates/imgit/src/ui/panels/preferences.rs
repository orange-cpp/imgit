//! The Preferences view (`app.preferences_open`), in place of the tab's content.
//!
//! A 300px sidebar with the global and repo-specific pages, and the selected page in a scrolling panel.
//! Positions are CSS pixels (fractions are Chromium's LayoutUnits, 1/64 px, so heights such as 19.796875 add up
//! to its pixel rounding): most pages are Bootstrap form-horizontal rows (a 285px label column, controls 15px
//! into the next one), Sparse Checkout uses a 248px label column. Values ImGit implements are bound to its
//! settings; the others are kept in preferences.txt.
//!
//! One module per group of pages: [`general`], [`profiles`], [`network`] (SSH, Connections), [`tools`] (External
//! Tools, Editor, In-App Terminal), [`appearance`] (UI Customization), [`signing`] and [`repo_pages`]; the
//! shared layout and controls are [`page`], [`controls`] and [`form`], the git configuration and key handling
//! [`store`].

mod appearance;
mod controls;
mod form;
mod general;
mod network;
mod page;
mod profiles;
mod repo_pages;
mod signing;
mod store;
mod tools;

use dear_imgui_rs::{StyleVar, WindowFlags};

use self::page::{BoxStyle, Page, Style};
pub use self::store::State;
use self::store::context;
use crate::app::application::Application;
use crate::ui::profile_widgets::profile_select_menu;
use crate::ui::profile_widgets::{SIDEBAR_PROFILE_MENU, draw_profile_icon, draw_select_focus};
use crate::ui::{Cx, Vec2, vec2};

/// A page of the sidebar.
struct PageEntry {
    name: &'static str,
    icon: &'static str,
    draw: fn(&mut Page<'_, '_>),
}

const GLOBAL_PAGES: [PageEntry; 10] = [
    PageEntry {
        name: "General",
        icon: "fas:cog",
        draw: general::general_page,
    },
    PageEntry {
        name: "Profiles",
        icon: "fas:user-friends",
        draw: profiles::profiles_page,
    },
    PageEntry {
        name: "SSH",
        icon: "fas:shield-check",
        draw: network::ssh_page,
    },
    PageEntry {
        name: "Connections",
        icon: "fas:plug",
        draw: network::connections_page,
    },
    PageEntry {
        name: "External Tools",
        icon: "fas:tools",
        draw: tools::external_tools_page,
    },
    PageEntry {
        name: "Notifications",
        icon: "fas:bell",
        draw: general::notifications_page,
    },
    PageEntry {
        name: "UI Customization",
        icon: "fas:paint-brush",
        draw: appearance::ui_customization_page,
    },
    PageEntry {
        name: "Commit Signing",
        icon: "fas:file-signature",
        draw: signing::commit_signing_page,
    },
    PageEntry {
        name: "Editor",
        icon: "fas:code",
        draw: tools::editor_page,
    },
    PageEntry {
        name: "In-App Terminal",
        icon: "far:terminal",
        draw: tools::terminal_page,
    },
];

const REPO_PAGES: [PageEntry; 5] = [
    PageEntry {
        name: "Gitflow",
        icon: "fas:exchange",
        draw: repo_pages::gitflow_page,
    },
    PageEntry {
        name: "Git Hooks",
        icon: "fas:brackets-curly",
        draw: repo_pages::git_hooks_page,
    },
    PageEntry {
        name: "Commit",
        icon: "fas:code-commit",
        draw: repo_pages::commit_page,
    },
    PageEntry {
        name: "LFS",
        icon: "fas:file",
        draw: repo_pages::lfs_page,
    },
    PageEntry {
        name: "Sparse Checkout",
        icon: "fas:folder-tree",
        draw: repo_pages::sparse_checkout_page,
    },
];

const PROFILE_SELECT: &str = "##sidebar_profile_select";

/// A page row: 45.7px (py2 around an 18px label on a 25.7px line), the fa-fw icon 30px in.
fn sidebar_row(page: &mut Page<'_, '_>, top: f32, entry: &PageEntry) {
    let selected = page.app.preferences_page == entry.name;
    let clicked = page.hit(entry.name, 0.0, top, page.width, 45.7, true);
    let hovered = page.cx.ui.is_item_hovered();
    if clicked {
        page.app.preferences_page = entry.name.to_owned();
    }
    let width = page.width;
    if selected || hovered {
        let fill = if selected {
            page.cx.palette.selected_row
        } else {
            page.cx.palette.hover_row
        };
        page.boxed(0.0, top, width, 45.7, BoxStyle::fill(fill));
    }
    let color = page.ink(if selected || hovered { 1.0 } else { 0.6 });
    page.icon(entry.icon, 30.0, top + 13.25, 22.5, 18.0, 18.0, color);
    page.text(72.5, top + 10.0, Style::fixed(18.0, color), entry.name);
}

fn draw_sidebar(page: &mut Page<'_, '_>, height: f32) {
    let ui = page.cx.ui;
    let w = page.width;
    let bg0 = page.cx.palette.panel_bg0;
    let bg1 = page.cx.palette.panel_bg1;
    let field = page.cx.palette.input_bg;
    let accent = page.cx.palette.accent;
    let hover_row = page.cx.palette.hover_row;
    page.boxed(0.0, 0.0, 299.0, height.max(1317.0), BoxStyle::fill(bg1));
    let secondary = Style::ink(14.0, 0.6);

    // Exit Preferences.
    if page.hit("##exit", 0.0, -0.855, w, 45.7, true) {
        page.app.preferences_open = false;
    }
    if ui.is_item_hovered() {
        page.boxed(0.0, -0.855, w, 45.7, BoxStyle::fill(hover_row));
    }
    let ink = page.ink(0.75);
    page.icon("fas:sign-out", 20.0, 13.25, 18.0, 18.0, 18.0, ink);
    page.text(43.0, 10.0, Style::ink(18.0, 0.75), "Exit Preferences");

    // The profile picker: a react-select showing the current profile as its placeholder; its menu switches.
    let profile = page.app.profiles.current().clone();
    page.text(10.0, 45.7, secondary, "Current profile");
    page.boxed(
        10.0,
        75.7,
        w - 10.0,
        42.0,
        BoxStyle::new(bg0, BoxStyle::CLEAR, 2.0),
    );
    let border = page.ink(0.08);
    page.boxed(
        10.0,
        75.7,
        w - 10.0,
        42.0,
        BoxStyle::new(field, border, 4.0),
    );
    if page.hit("##profile_picker", 10.0, 75.7, w - 10.0, 42.0, true) {
        ui.open_popup(PROFILE_SELECT);
    }
    if ui.is_popup_open(PROFILE_SELECT) {
        page.boxed(
            10.0,
            75.7,
            w - 10.0,
            42.0,
            BoxStyle::new(BoxStyle::CLEAR, accent, 4.0),
        );
        let (min, max, rounding) = (page.at(10.0, 75.7), page.at(w, 117.7), page.px(4.0));
        draw_select_focus(page.cx, page.draw, min, max, rounding);
    }
    let icon_min = page.at(21.0, 82.7);
    let icon_size = page.px(28.0);
    draw_profile_icon(page.cx, page.draw, icon_min, icon_size, &profile);
    page.text_truncated(59.0, 86.7, Style::ink(14.0, 0.4), &profile.name, 200.0);
    page.boxed(w - 30.0, 82.7, 1.0, 28.0, BoxStyle::fill(border));
    page.chevron(w - 25.0, 86.7, 20.0, ink);
    let (control_min, control_max) = (page.at(10.0, 75.7), page.at(w, 117.7));
    if let Some(picked) = profile_select_menu(
        page.cx,
        page.app,
        PROFILE_SELECT,
        control_min,
        control_max,
        SIDEBAR_PROFILE_MENU,
    ) {
        page.app.switch_profile(&picked);
    }

    page.text(10.0, 137.7, secondary, "Preferences");
    let mut top = 162.7;
    for entry in &GLOBAL_PAGES {
        sidebar_row(page, top, entry);
        top += 45.703_125;
    }
    page.text(10.0, top + 30.0, secondary, "Repo-Specific Preferences");
    let dim = page.ink(0.6);
    page.icon("far:folder", 10.0, top + 57.75, 14.0, 14.0, 14.0, dim);
    let repo_name = context(page.app).repo_name;
    page.text(29.0, top + 55.0, secondary, &format!("Repo: {repo_name}"));
    top += 80.0;
    for entry in &REPO_PAGES {
        sidebar_row(page, top, entry);
        top += 45.703_125;
    }
    page.cx.set_cursor(page.origin);
    ui.dummy(vec2(page.px(w), page.px(top)));
}

/// The Preferences view, `size` device px (everything under the tabs bar).
pub fn draw_preferences(cx: &mut Cx<'_>, app: &mut Application, size: Vec2) {
    let ui = cx.ui;
    app.panels.preferences.now = cx.time();
    let origin = cx.cursor();
    let draw = cx.window_draw_list();
    // panel-bg0 under everything.
    draw.add_rect_filled(origin, origin + size, cx.palette.panel_bg0, 0.0);
    draw.add_rect_filled(
        vec2(origin.x + cx.px(299.0), origin.y),
        vec2(origin.x + cx.px(300.0), origin.y + size.y),
        cx.palette.ink(0.08),
        0.0,
    );
    {
        let _scrollbar = ui.push_style_var(StyleVar::ScrollbarSize(cx.px(8.0)));
        let height = size.y / cx.scale;
        ui.child_window("##prefs_sidebar")
            .size([cx.px(299.0), size.y])
            .flags(WindowFlags::NO_BACKGROUND)
            .build(ui, || {
                // A short window scrolls the sidebar; its 8px scrollbar narrows the rows (the border-right stays
                // at 299px, drawn above).
                let w = 299.0 - if ui.scroll_max_y() > 0.0 { 8.0 } else { 0.0 };
                let draw = cx.window_draw_list();
                let origin = cx.cursor();
                let mut page = Page::new(&mut *cx, &mut *app, draw, origin, w);
                draw_sidebar(&mut page, height);
            });
    }

    cx.set_cursor(vec2(origin.x + cx.px(300.0), origin.y));
    let page_size = vec2(size.x - cx.px(300.0), size.y);
    ui.child_window("##prefs_page")
        .size(page_size.into())
        .flags(WindowFlags::NO_BACKGROUND)
        .build(ui, || {
            let draw = cx.window_draw_list();
            let origin = cx.cursor();
            let width = page_size.x / cx.scale;
            let mut page = Page::new(&mut *cx, &mut *app, draw, origin, width);
            // The page scrollbar is 8px wide; the grid's breakpoint follows the window (a media query).
            page.scrollbar = if ui.scroll_max_y() > 0.0 { 8.0 } else { 0.0 };
            page.stacked = ui.io().display_size()[0] / page.cx.scale < 992.0;
            let name = page.app.preferences_page.clone();
            let _id = ui.push_id(name.as_str());
            if let Some(entry) = GLOBAL_PAGES
                .iter()
                .chain(&REPO_PAGES)
                .find(|entry| entry.name == name)
            {
                (entry.draw)(&mut page);
            }
            // Reserve the content for scrolling.
            page.cx.set_cursor(page.origin);
            ui.dummy(vec2(page.px(page.width), page.px(page.extent)));
        });
    profiles::draw_profile_dialogs(cx, app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::test_frame::TestFrames;
    use dear_imgui_rs::Key;

    #[test]
    fn the_auto_fetch_interval_is_clamped_and_saved_when_the_field_loses_focus() {
        let scratch = tempfile::tempdir().unwrap();
        let mut app = Application::with_config_directory(scratch.path().join("config"));
        app.preferences_page = "General".to_owned();
        app.settings.auto_fetch_minutes = 9;
        let mut frames = TestFrames::new();
        let size = vec2(1260.0, 730.0);
        let mut field = Vec2::ZERO;
        let mut outside = Vec2::ZERO;
        for _ in 0..2 {
            frames.draw(|cx| {
                let page_left = 300.0;
                let control = 55.0 + (size.x - page_left - 110.0) / 4.0 + 15.0;
                field = cx.cursor() + vec2(page_left + control + 30.0, 73.0);
                outside = cx.cursor() + vec2(page_left + 70.0, 28.0);
                draw_preferences(cx, &mut app, size);
            });
        }

        frames.click_left(field, |cx| draw_preferences(cx, &mut app, size));
        frames.set_key(Key::ModCtrl, true);
        frames.set_key(Key::A, true);
        frames.draw(|cx| draw_preferences(cx, &mut app, size));
        frames.set_key(Key::A, false);
        frames.set_key(Key::ModCtrl, false);
        frames.draw(|cx| draw_preferences(cx, &mut app, size));
        frames.type_text("999");
        frames.draw(|cx| draw_preferences(cx, &mut app, size));
        assert_eq!(app.settings.auto_fetch_minutes, 9);

        frames.click_left(outside, |cx| draw_preferences(cx, &mut app, size));
        assert_eq!(app.settings.auto_fetch_minutes, 60);
        let mut stored = crate::app::settings::Settings::default();
        stored.load(&app.config_path("settings.txt"));
        assert_eq!(stored.auto_fetch_minutes, 60);
    }

    #[test]
    fn choosing_a_custom_theme_from_the_select_applies_and_saves_it() {
        let scratch = tempfile::tempdir().unwrap();
        let mut app = Application::with_config_directory(scratch.path().join("config"));
        app.preferences_page = "UI Customization".to_owned();
        let themes = app.config_path("themes");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(
            themes.join("test.json"),
            r##"{"name":"Coverage Theme","base":"ImGit Light","accent":"#2F7D6D"}"##,
        )
        .unwrap();
        let size = vec2(1260.0, 730.0);
        let mut frames = TestFrames::new();
        frames.draw(|cx| app.reload_custom_themes(cx, false));

        let draw = |cx: &mut Cx<'_>, app: &mut Application| {
            draw_preferences(cx, app, size);
            crate::ui::menu::draw_menus(cx, app);
        };
        let mut theme_select = Vec2::ZERO;
        let mut custom_index = None;
        for _ in 0..2 {
            frames.draw(|cx| {
                let control = 55.0 + (size.x - 300.0 - 110.0) / 4.0 + 15.0;
                theme_select = cx.cursor() + vec2(300.0 + control + 45.0, 73.0);
                custom_index = cx
                    .state
                    .theme_names
                    .iter()
                    .position(|name| name == "Coverage Theme");
                draw(cx, &mut app);
            });
        }
        frames.click_left(theme_select, |cx| draw(cx, &mut app));
        for _ in 0..=custom_index.unwrap() {
            frames.set_key(Key::DownArrow, true);
            frames.draw(|cx| draw(cx, &mut app));
            frames.set_key(Key::DownArrow, false);
            frames.draw(|cx| draw(cx, &mut app));
        }
        frames.set_key(Key::Enter, true);
        frames.draw(|cx| draw(cx, &mut app));
        assert_eq!(app.settings.theme, "Coverage Theme");

        frames.draw(|cx| {
            assert_eq!(cx.state.theme_name, "Coverage Theme");
            assert_eq!(cx.palette.accent, crate::theme::Color::rgb(0x2F7D6D));
        });
        let mut stored = crate::app::settings::Settings::default();
        stored.load(&app.config_path("settings.txt"));
        assert_eq!(stored.theme, "Coverage Theme");
    }

    #[test]
    fn the_preferences_sidebar_opens_other_pages_and_returns_to_the_repository() {
        let scratch = tempfile::tempdir().unwrap();
        let repo = imgit_git::Repository::init(scratch.path().join("repo")).unwrap();
        let mut app = Application::with_config_directory(scratch.path().join("config"));
        app.open_repository(&repo.workdir().to_string_lossy());
        app.preferences_open = true;
        let size = vec2(1260.0, 730.0);
        let mut frames = TestFrames::new();
        let mut origin = Vec2::ZERO;
        for _ in 0..2 {
            frames.draw(|cx| {
                origin = cx.cursor();
                draw_preferences(cx, &mut app, size);
            });
        }
        for page in [
            "SSH",
            "External Tools",
            "Notifications",
            "Editor",
            "In-App Terminal",
        ] {
            let index = GLOBAL_PAGES
                .iter()
                .position(|entry| entry.name == page)
                .unwrap();
            let row = origin + vec2(100.0, 162.7 + 45.703_125 * index as f32 + 22.0);
            frames.click_left(row, |cx| draw_preferences(cx, &mut app, size));
            assert_eq!(app.preferences_page, page);
            assert!(app.preferences_open);
        }
        frames.click_left(origin + vec2(65.0, 20.0), |cx| {
            draw_preferences(cx, &mut app, size)
        });
        assert!(!app.preferences_open);
        assert_eq!(app.active_workdir(), Some(repo.workdir()));
    }

    #[test]
    fn page_names_are_unique() {
        let mut names: Vec<&str> = GLOBAL_PAGES
            .iter()
            .chain(&REPO_PAGES)
            .map(|entry| entry.name)
            .collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), GLOBAL_PAGES.len() + REPO_PAGES.len());
    }

    #[test]
    fn the_default_page_exists() {
        let app =
            Application::with_config_directory(tempfile::tempdir().unwrap().path().join("config"));
        assert!(
            GLOBAL_PAGES
                .iter()
                .any(|entry| entry.name == app.preferences_page)
        );
    }
}
