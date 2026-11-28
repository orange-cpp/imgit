//! The toolbar: undo / redo, pull / push, branch, stash, pop, terminal, search...

use std::fs;
use std::path::{Path, PathBuf};

use dear_imgui_rs::{ChildFlags, MouseButton, WindowFlags};
use imgit_app::RepoState;
use imgit_git::{Ref, RepoState as OperationState};

use crate::app::application::Application;
use crate::app::dialog::DialogKind;
use crate::app::settings::PullMode;
use crate::theme::{Color, FONT_SIZE_SMALL, TOOLBAR_HEIGHT};
use crate::ui::geom::{Vec2, vec2};
use crate::ui::icons::Icon;
use crate::ui::panels::{checkout_ref, stash_changes};
use crate::ui::{Cx, draw::DrawList};

// The toolbar (48px, toolbar__bg1 with a 1px toolbar__bg0 bottom border). Buttons are 52px wide: an Open Sans
// 11px caption (baseline 15px down) over a 16px Font Awesome icon centred 32px down.
const BUTTON_WIDTH: f32 = 52.0;
const CARET_WIDTH: f32 = 15.0;
const SEPARATOR_WIDTH: f32 = 6.0;
const LABEL_FONT: f32 = 11.0;
const LABEL_BASELINE: f32 = 15.0;
const ICON_EM: f32 = 16.0;
const ICON_CENTER: f32 = 32.0;

/// The LFS DropdownButton (.mr1): "LFS" in 16px (--fs-4) 10px from its left edge and bootstrap's 8x4 caret, on
/// toolbar-bg0 while hovered or open; a spinning circle-notch replaces the text while an LFS command runs.
const LFS_WIDTH: f32 = 52.0;
const LFS_MARGIN: f32 = 5.0;

/// .bread-crumb .option: min-width 110px, padding-left 20px; a 10px caption ("repository") over the 15px value
/// (bold for the repository), with a caret 11.5px from the right edge.
const CRUMB_VALUE_FONT: f32 = 15.0;
const CRUMB_MIN_WIDTH: f32 = 110.0;
const CRUMB_MAX_VALUE: f32 = 320.0;

/// A regular face's ascent over its em (Open Sans: 2189 / 2789 of the line, the way the C++ placed baselines).
const ASCENT_RATIO: f32 = 2189.0 / 2789.0;

/// The dropdowns' width (logical px).
const DROPDOWN_WIDTH: f32 = 250.0;

/// What the toolbar remembers between frames (the C++ function-local statics).
#[derive(Default)]
pub struct State {
    /// The breadcrumb dropdowns' search box.
    crumb_search: String,
    /// Whether the repository uses Git LFS (read again when the working directory changes).
    lfs: bool,
    lfs_checked_for: Option<PathBuf>,
    /// A cross of a submodule crumb closes the levels down to this depth (the tab's own repository is 0), one
    /// per frame.
    close_to_depth: Option<usize>,
}

/// What a toolbar button shows.
#[derive(Clone, Copy)]
enum ToolbarIcon {
    Icon(Icon),
    /// A Font Awesome name instead of the icon's own glyph.
    Fa(&'static str),
}

/// One button of the bar: an icon under a small caption. `dropdown` adds the split caret that opens a menu;
/// `open` is whether that menu is showing (the caret flips up).
struct ToolbarItem<'a> {
    id: &'a str,
    icon: ToolbarIcon,
    label: &'a str,
    enabled: bool,
    dropdown: bool,
    tip: Option<&'a str>,
    open: bool,
}

impl<'a> ToolbarItem<'a> {
    fn new(id: &'a str, icon: Icon, label: &'a str) -> Self {
        Self {
            id,
            icon: ToolbarIcon::Icon(icon),
            label,
            enabled: true,
            dropdown: false,
            tip: None,
            open: false,
        }
    }

    fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    fn tip(mut self, tip: Option<&'a str>) -> Self {
        self.tip = tip;
        self
    }

    fn dropdown(mut self, open: bool) -> Self {
        self.dropdown = true;
        self.open = open;
        self
    }
}

#[derive(Default, Clone, Copy)]
struct ItemResult {
    clicked: bool,
    dropdown: bool,
}

fn toolbar_item(cx: &mut Cx<'_>, labels: bool, item: &ToolbarItem<'_>) -> ItemResult {
    let ui = cx.ui;
    let p = cx.palette;
    let height = cx.px(TOOLBAR_HEIGHT) - 1.0;
    let width = cx.px(BUTTON_WIDTH + if item.dropdown { CARET_WIDTH } else { 0.0 });
    let pos = cx.cursor();
    let mut result = ItemResult::default();

    {
        let _disabled = (!item.enabled).then(|| ui.begin_disabled());
        ui.invisible_button(item.id, vec2(width, height));
    }
    let hovered = ui.is_item_hovered();
    let clicked = ui.is_item_clicked_with_button(MouseButton::Left);
    if item.enabled
        && let Some(tip) = item.tip
    {
        cx.tooltip(tip);
    }

    let draw = cx.window_draw_list();
    // Hovered, the button darkens to toolbar-bg0 (the whole item, or just its caret half on a split button).
    if hovered && item.enabled {
        let split_x = pos.x + cx.px(BUTTON_WIDTH);
        let on_caret = item.dropdown && cx.mouse_pos().x >= split_x;
        let left = if on_caret { split_x } else { pos.x };
        let right = if item.dropdown && !on_caret {
            split_x
        } else {
            pos.x + width
        };
        draw.add_rect_filled(
            vec2(left, pos.y + 1.0),
            vec2(right, pos.y + height + 1.0),
            p.toolbar_bg0,
            0.0,
        );
    }
    // The glyph brightens from 90% white to text-selected on hover; disabled buttons fade to 20% / 30%.
    let hover_t = cx.smooth_over(item.id, hovered && item.enabled, 0.12);
    let icon_color = if item.enabled {
        p.ink(0.9 + 0.1 * hover_t)
    } else {
        p.ink(0.2)
    };
    let label_color = if item.enabled {
        p.text_secondary
    } else {
        p.ink(0.3)
    };
    let center_x = pos.x + cx.px(BUTTON_WIDTH) / 2.0;
    let icon_y = if labels {
        pos.y + cx.px(ICON_CENTER)
    } else {
        pos.y + height / 2.0
    };
    let center = vec2(center_x, icon_y);
    match item.icon {
        ToolbarIcon::Fa(name) => cx.draw_fa_icon(draw, name, center, cx.px(ICON_EM), icon_color),
        ToolbarIcon::Icon(icon) => cx.draw_icon(draw, icon, center, cx.px(ICON_EM), icon_color),
    }
    if labels {
        let regular = cx.fonts.regular;
        let label_size = cx.text_size(regular, LABEL_FONT, item.label);
        let ascent = cx.font_px(regular, LABEL_FONT) * ASCENT_RATIO;
        cx.draw_text(
            draw,
            regular,
            LABEL_FONT,
            vec2(
                center_x - label_size.x / 2.0,
                pos.y + cx.px(LABEL_BASELINE) - ascent,
            ),
            label_color,
            item.label,
        );
    }

    if item.dropdown {
        let split_x = pos.x + cx.px(BUTTON_WIDTH);
        let on_dropdown = hovered && cx.mouse_pos().x >= split_x;
        let caret = if item.open {
            "fas:caret-up"
        } else {
            "fas:caret-down"
        };
        cx.draw_fa_icon(
            draw,
            caret,
            vec2(split_x + cx.px(5.5), pos.y + cx.px(24.0)),
            cx.px(14.0),
            if item.enabled {
                p.text_secondary
            } else {
                p.ink(0.3)
            },
        );
        if clicked {
            if on_dropdown {
                result.dropdown = true;
            } else {
                result.clicked = true;
            }
        }
    } else {
        result.clicked = clicked;
    }
    ui.same_line_with_spacing(0.0, 0.0);
    result
}

/// The LFS dropdown button: `true` when clicked (not while an LFS command runs).
fn lfs_button(cx: &mut Cx<'_>, busy: bool, open: bool) -> bool {
    let ui = cx.ui;
    let p = cx.palette;
    let height = cx.px(TOOLBAR_HEIGHT) - 1.0;
    // Whole pixels, like the dropdown under it.
    let pos = vec2(cx.cursor().x.round(), cx.cursor().y);
    cx.set_cursor(pos);
    let clicked = {
        let _disabled = busy.then(|| ui.begin_disabled());
        ui.invisible_button("##lfs", vec2(cx.px(LFS_WIDTH), height))
    };
    let hovered = ui.is_item_hovered();
    if !busy {
        cx.tooltip("LFS Commands");
    }
    let draw = cx.window_draw_list();
    if (hovered && !busy) || open {
        draw.add_rect_filled(
            vec2(pos.x, pos.y + 1.0),
            vec2(pos.x + cx.px(LFS_WIDTH), pos.y + height + 1.0),
            p.toolbar_bg0,
            0.0,
        );
    }
    let hover_t = cx.smooth_over("##lfs", hovered && !busy, 0.12);
    let color = if busy {
        p.ink(0.2)
    } else {
        p.ink(0.9 + 0.1 * hover_t)
    };
    let baseline = pos.y + cx.px(29.0);
    if busy {
        cx.draw_icon(
            draw,
            Icon::Spinner,
            vec2(pos.x + cx.px(23.5), baseline - cx.px(5.5)),
            cx.px(16.0),
            color,
        );
    } else {
        let regular = cx.fonts.regular;
        let ascent = cx.font_px(regular, 16.0) * ASCENT_RATIO;
        cx.draw_text(
            draw,
            regular,
            16.0,
            vec2(pos.x + cx.px(10.0), baseline - ascent),
            color,
            "LFS",
        );
    }
    let caret_x = pos.x + cx.px(40.0);
    let caret_y = pos.y + cx.px(22.0);
    draw.add_triangle_filled(
        vec2(caret_x, caret_y),
        vec2(caret_x + cx.px(8.0), caret_y),
        vec2(caret_x + cx.px(4.0), caret_y + cx.px(4.0)),
        color,
    );
    ui.same_line_with_spacing(0.0, cx.px(LFS_MARGIN));
    clicked && !busy
}

fn toolbar_separator(cx: &Cx<'_>, draw: DrawList<'_>, height: f32) {
    let pos = cx.cursor();
    draw.add_line(
        vec2(pos.x + 0.5, pos.y + 1.0),
        vec2(pos.x + 0.5, pos.y + height - 1.0),
        cx.palette.ink(0.2),
        1.0,
    );
    cx.ui.dummy(vec2(cx.px(SEPARATOR_WIDTH), height));
    cx.ui.same_line_with_spacing(0.0, 0.0);
}

/// The chevron between two crumbs.
fn chevron(cx: &mut Cx<'_>, draw: DrawList<'_>, item_height: f32) {
    let pos = cx.cursor();
    cx.draw_icon(
        draw,
        Icon::ChevronRight,
        vec2(pos.x + cx.px(8.5), pos.y + cx.px(28.5)),
        cx.px(16.0),
        cx.palette.text_disabled,
    );
    cx.ui.dummy(vec2(cx.px(13.0), item_height));
    cx.ui.same_line_with_spacing(0.0, 0.0);
}

/// A crumb's width: at least 110px, else 20px of padding, the value (at most 320px) and 28px for the caret.
fn crumb_width(px: impl Fn(f32) -> f32, value_width: f32) -> f32 {
    px(CRUMB_MIN_WIDTH).max(px(20.0) + value_width.min(px(CRUMB_MAX_VALUE)) + px(28.0))
}

/// A submodule crumb's width: the cross (14px), a 6px gap, the name (at most 320px) and 20px.
fn submodule_crumb_width(px: impl Fn(f32) -> f32, name_width: f32) -> f32 {
    px(CRUMB_MIN_WIDTH)
        .max(px(20.0) + px(14.0) + px(6.0) + name_width.min(px(CRUMB_MAX_VALUE)) + px(20.0))
}

/// `open`: its dropdown is showing (ImGit then flips the caret up). `true` when clicked.
#[allow(clippy::too_many_arguments)] // the crumb's own parts
fn breadcrumb(
    cx: &mut Cx<'_>,
    draw: DrawList<'_>,
    id: &str,
    caption: &str,
    value: &str,
    height: f32,
    bold: bool,
    open: bool,
) -> bool {
    let ui = cx.ui;
    let p = cx.palette;
    let pos = cx.cursor();
    let value_font = if bold {
        cx.fonts.bold
    } else {
        cx.fonts.regular
    };
    let value_w = cx
        .text_size(value_font, CRUMB_VALUE_FONT, value)
        .x
        .min(cx.px(CRUMB_MAX_VALUE));
    let width = crumb_width(|logical| cx.px(logical), value_w);
    ui.invisible_button(id, vec2(width, height));
    let hovered = ui.is_item_hovered();
    let clicked = ui.is_item_clicked_with_button(MouseButton::Left);
    if hovered {
        draw.add_rect_filled(pos, vec2(pos.x + width, pos.y + height), p.toolbar_bg0, 0.0);
    }
    let regular = cx.fonts.regular;
    let caption_ascent = cx.font_px(regular, 10.0) * ASCENT_RATIO;
    cx.draw_text(
        draw,
        regular,
        10.0,
        vec2(pos.x + cx.px(20.0), pos.y + cx.px(12.7) - caption_ascent),
        p.text_secondary,
        caption,
    );
    let value_ascent = cx.font_px(value_font, CRUMB_VALUE_FONT) * ASCENT_RATIO;
    cx.draw_text_ellipsis(
        draw,
        value_font,
        CRUMB_VALUE_FONT,
        vec2(pos.x + cx.px(20.0), pos.y + cx.px(34.4) - value_ascent),
        value_w + 1.0,
        p.text_selected,
        value,
    );
    cx.draw_fa_icon(
        draw,
        if open {
            "fas:caret-up"
        } else {
            "fas:caret-down"
        },
        vec2(pos.x + width - cx.px(11.5), pos.y + cx.px(30.0)),
        cx.px(12.0),
        if hovered || open {
            p.text_selected
        } else {
            p.text_secondary
        },
    );
    ui.same_line_with_spacing(0.0, 0.0);
    clicked
}

/// An open submodule's crumb: the "submodule" caption over a cross and its name. The cross closes it, unless
/// `can_close` is false (dimmed: a command still runs in it).
fn submodule_crumb(
    cx: &mut Cx<'_>,
    draw: DrawList<'_>,
    id: &str,
    name: &str,
    height: f32,
    can_close: bool,
) -> bool {
    let ui = cx.ui;
    let p = cx.palette;
    let pos = cx.cursor();
    let cross_w = cx.px(14.0);
    let gap = cx.px(6.0);
    let regular = cx.fonts.regular;
    let value_w = cx
        .text_size(regular, CRUMB_VALUE_FONT, name)
        .x
        .min(cx.px(CRUMB_MAX_VALUE));
    let width = submodule_crumb_width(|logical| cx.px(logical), value_w);
    ui.invisible_button(id, vec2(width, height));
    let row_hovered = ui.is_item_hovered();
    let clicked = ui.is_item_clicked_with_button(MouseButton::Left);
    let cross_min = vec2(pos.x + cx.px(20.0), pos.y + cx.px(30.0) - cx.px(8.0));
    let cross_max = vec2(cross_min.x + cross_w, cross_min.y + cx.px(16.0));
    let mouse = cx.mouse_pos();
    let cross_hovered = row_hovered
        && mouse.x >= cross_min.x
        && mouse.x < cross_max.x
        && mouse.y >= cross_min.y
        && mouse.y < cross_max.y;
    if row_hovered {
        draw.add_rect_filled(pos, vec2(pos.x + width, pos.y + height), p.toolbar_bg0, 0.0);
    }
    if cross_hovered {
        cx.styled_tooltip(
            cross_min,
            cross_max,
            if can_close {
                "Close submodule"
            } else {
                "Wait for the running command to finish"
            },
            false,
        );
    }
    let caption_ascent = cx.font_px(regular, 10.0) * ASCENT_RATIO;
    cx.draw_text(
        draw,
        regular,
        10.0,
        vec2(pos.x + cx.px(20.0), pos.y + cx.px(12.7) - caption_ascent),
        p.text_secondary,
        "submodule",
    );
    let cross_color = if !can_close {
        p.text_disabled
    } else if cross_hovered {
        p.text_selected
    } else {
        p.text_secondary
    };
    cx.draw_fa_icon(
        draw,
        "fas:times",
        vec2(cross_min.x + cross_w / 2.0, pos.y + cx.px(30.0)),
        cx.px(12.0),
        cross_color,
    );
    let value_ascent = cx.font_px(regular, CRUMB_VALUE_FONT) * ASCENT_RATIO;
    cx.draw_text_ellipsis(
        draw,
        regular,
        CRUMB_VALUE_FONT,
        vec2(cross_max.x + gap, pos.y + cx.px(34.4) - value_ascent),
        value_w + 1.0,
        p.text_selected,
        name,
    );
    ui.same_line_with_spacing(0.0, 0.0);
    can_close && cross_hovered && clicked
}

/// The pull dropdown's text for the mode.
const fn pull_mode_label(mode: PullMode) -> &'static str {
    match mode {
        PullMode::FetchAll => "Fetch All",
        PullMode::FastForwardIfPossible => "Pull (fast-forward if possible)",
        PullMode::FastForwardOnly => "Pull (fast-forward only)",
        PullMode::Rebase => "Pull (rebase)",
    }
}

const PULL_MODES: [PullMode; 4] = [
    PullMode::FetchAll,
    PullMode::FastForwardIfPossible,
    PullMode::FastForwardOnly,
    PullMode::Rebase,
];

/// The git command of the pull button for `mode`: its title and arguments.
fn pull_command(mode: PullMode) -> (&'static str, &'static [&'static str]) {
    match mode {
        PullMode::FetchAll => ("Fetch", &["fetch", "--all", "--prune"]),
        PullMode::FastForwardOnly => ("Pull", &["pull", "--ff-only"]),
        PullMode::Rebase => ("Pull", &["pull", "--rebase"]),
        PullMode::FastForwardIfPossible => ("Pull", &["pull", "--ff"]),
    }
}

/// The pull dropdown: the explanation (12px secondary, 10px padding) and four radio rows, the default one in
/// green with a dot.
fn pull_mode_list(cx: &mut Cx<'_>, app: &mut Application) {
    cx.popover_note("Select a default pull/fetch operation to execute when clicking this button");
    for mode in PULL_MODES {
        if cx.popover_radio_row(pull_mode_label(mode), app.settings.pull_mode == mode) {
            app.settings.pull_mode = mode;
            app.settings.save(&app.config_path("settings.txt"));
            cx.close_popover();
        }
    }
}

/// ImGit shows its LFS button only for repositories that use Git LFS.
fn uses_lfs(root: &Path) -> bool {
    root.join(".git").join("lfs").exists()
        || fs::read_to_string(root.join(".gitattributes"))
            .is_ok_and(|attributes| attributes.lines().any(|line| line.contains("filter=lfs")))
}

/// The remote `git lfs push --all` sends to: the checked-out branch's upstream remote, else origin.
fn lfs_remote(refs: &[Ref]) -> String {
    refs.iter()
        .find(|r| r.is_head)
        .and_then(|head| head.upstream.as_deref())
        .filter(|upstream| !upstream.is_empty())
        .map_or("origin", |upstream| {
            upstream.split('/').next().unwrap_or(upstream)
        })
        .to_owned()
}

const fn state_label(state: OperationState) -> &'static str {
    match state {
        OperationState::Merge => "Merge in progress",
        OperationState::Revert => "Revert in progress",
        OperationState::CherryPick => "Cherry pick in progress",
        OperationState::Rebase => "Rebase in progress",
        OperationState::Other => "Operation in progress",
        OperationState::None => "",
    }
}

/// The text of the crumb of the branch.
fn branch_crumb_text(state: &RepoState) -> String {
    if state.history.unborn {
        "no commits yet".to_owned()
    } else if state.history.detached {
        "detached HEAD".to_owned()
    } else {
        state.history.head_branch.clone().unwrap_or_default()
    }
}

/// The tab's repository first, the shown one last.
fn levels_of(state: &RepoState) -> Vec<&RepoState> {
    let mut levels: Vec<&RepoState> =
        std::iter::successors(Some(state), |level| level.parent.as_deref()).collect();
    levels.reverse();
    levels
}

/// The toolbar of the repository tab, at the cursor (`TOOLBAR_HEIGHT` tall).
pub fn draw_toolbar(cx: &mut Cx<'_>, app: &mut Application, state: &mut RepoState) {
    app.with_panel_state(
        |panels| &mut panels.toolbar,
        |app, toolbar| draw(cx, app, state, toolbar),
    );
}

/// Closes the next submodule level of a cross clicked deeper in the chain (the application closes one per
/// frame).
fn continue_closing(app: &mut Application, state: &RepoState, toolbar: &mut State) {
    let Some(target) = toolbar.close_to_depth else {
        return;
    };
    let depth = levels_of(state).len() - 1;
    if depth <= target || state.busy() {
        toolbar.close_to_depth = None;
    } else {
        app.close_submodule(state);
    }
}

fn draw(cx: &mut Cx<'_>, app: &mut Application, state: &mut RepoState, toolbar: &mut State) {
    let ui = cx.ui;
    let height = cx.px(TOOLBAR_HEIGHT);
    let origin = cx.cursor();
    let width = cx.avail().x;
    continue_closing(app, state, toolbar);

    ui.child_window("##toolbar")
        .size([width, height])
        .child_flags(ChildFlags::NONE)
        .flags(WindowFlags::NO_SCROLLBAR | WindowFlags::NO_SCROLL_WITH_MOUSE)
        .build(ui, || {
            draw_bar(cx, app, state, toolbar, origin, width, height);
        });
}

fn draw_bar(
    cx: &mut Cx<'_>,
    app: &mut Application,
    state: &mut RepoState,
    toolbar: &mut State,
    origin: Vec2,
    width: f32,
    height: f32,
) {
    let ui = cx.ui;
    let p = cx.palette;
    let draw = cx.window_draw_list();
    draw.add_rect_filled(
        origin,
        vec2(origin.x + width, origin.y + height),
        p.toolbar_bg1,
        0.0,
    );
    draw.add_rect_filled(
        vec2(origin.x, origin.y + height - 1.0),
        vec2(origin.x + width, origin.y + height),
        p.toolbar_bg0,
        0.0,
    );
    let item_h = height - 1.0;

    let head = state
        .head_ref()
        .map(|head| (head.name.clone(), head.upstream.clone()));
    let has_head = state.history.head_sha.is_some();
    let idle = !state.busy();
    let has_stash = state
        .history
        .refs
        .iter()
        .any(|r| r.ref_type == imgit_git::RefType::Stash);
    let labels = app.settings.show_toolbar_labels;

    // Breadcrumb: repository > branch. Their dropdowns open under the bar (1px over its border), 250px wide: a
    // search box, then the recent repositories / local branches. An open submodule shows the repository the
    // tab was opened on first, then a "submodule" crumb per level with a cross that goes back to the level
    // above it.
    let dropdown_y = origin.y + height - 1.0;
    cx.set_cursor(origin);
    let repo_x = cx.cursor().x;
    // What the crumbs need of each level, so that the state stays free for the clicks.
    struct Level {
        workdir: PathBuf,
        name: String,
        /// This level or one open in it runs a command.
        busy_inside: bool,
    }
    let levels: Vec<Level> = {
        let chain = levels_of(state);
        (0..chain.len())
            .map(|i| Level {
                workdir: chain[i].repo.workdir().to_path_buf(),
                name: chain[i].repo.name().to_owned(),
                busy_inside: chain[i..].iter().any(|level| level.busy()),
            })
            .collect()
    };
    let root_name = app.repo_display_name(&levels[0].workdir);
    if breadcrumb(
        cx,
        draw,
        "##repo_crumb",
        "repository",
        &root_name,
        item_h,
        true,
        ui.is_popup_open("##repo_crumb_menu"),
    ) {
        toolbar.crumb_search.clear();
        cx.open_popover("##repo_crumb_menu");
    }
    let mut to_open: Option<PathBuf> = None;
    cx.popover(
        "##repo_crumb_menu",
        vec2(repo_x, dropdown_y),
        DROPDOWN_WIDTH,
        |cx| {
            cx.popover_search(&mut toolbar.crumb_search, "Search");
            cx.popover_header("Recently opened", Some("far:clock"));
            for path in &app.recent_repos {
                // Neither this repository nor, in a submodule, the repositories it is open in (this very tab).
                if levels.iter().any(|level| level.workdir == *path) {
                    continue;
                }
                let name = app.repo_display_name(path);
                if !toolbar.crumb_search.is_empty()
                    && !crate::ui::widgets::contains_ci(&name, &toolbar.crumb_search)
                {
                    continue;
                }
                if cx.popover_row(&name, None, false) {
                    to_open = Some(path.clone());
                    cx.close_popover();
                    break;
                }
            }
            if cx.popover_link_row("View all repositories") {
                cx.close_popover();
                app.repo_management_open = true;
            }
        },
    );
    if let Some(path) = to_open {
        app.open_repository(&path.to_string_lossy());
    }
    chevron(cx, draw, item_h);
    for (i, level) in levels.iter().enumerate().skip(1) {
        // Closing a level closes the ones open in it too: not while one of them runs a command, whose result
        // would have nowhere to go.
        let _id = ui.push_id(i);
        if submodule_crumb(
            cx,
            draw,
            "##submodule_crumb",
            &level.name,
            item_h,
            !level.busy_inside,
        ) {
            // Innermost first; the application closes one level per frame, so the rest follows.
            toolbar.close_to_depth = Some(i - 1);
            continue_closing(app, state, toolbar);
        }
        drop(_id);
        chevron(cx, draw, item_h);
    }
    let branch_x = cx.cursor().x;
    if breadcrumb(
        cx,
        draw,
        "##branch_crumb",
        "branch",
        &branch_crumb_text(state),
        item_h,
        false,
        ui.is_popup_open("##branch_crumb_menu"),
    ) {
        toolbar.crumb_search.clear();
        cx.open_popover("##branch_crumb_menu");
    }
    let mut checkout: Option<Ref> = None;
    cx.popover(
        "##branch_crumb_menu",
        vec2(branch_x, dropdown_y),
        DROPDOWN_WIDTH,
        |cx| {
            cx.popover_search(&mut toolbar.crumb_search, "Search");
            for r in &state.history.refs {
                if r.ref_type != imgit_git::RefType::LocalBranch
                    || (!toolbar.crumb_search.is_empty()
                        && !crate::ui::widgets::contains_ci(&r.name, &toolbar.crumb_search))
                {
                    continue;
                }
                if cx.popover_row(&r.name, Some("far:code-branch"), r.is_head) && !r.is_head {
                    checkout = Some(r.clone());
                }
            }
            if checkout.is_some() {
                cx.close_popover();
            }
        },
    );
    if let Some(target) = checkout {
        checkout_ref(cx, app, state, &target);
    }

    // Centre: Undo Redo Pull▾ Push Branch Stash Pop | Terminal [LFS▾], centred in the space left of the
    // Actions / Search group.
    let workdir = state.repo.workdir();
    if toolbar.lfs_checked_for.as_deref() != Some(workdir) {
        toolbar.lfs_checked_for = Some(workdir.to_path_buf());
        toolbar.lfs = uses_lfs(workdir);
    }
    let lfs = toolbar.lfs;
    let right_group = cx.px(2.0 * BUTTON_WIDTH + 1.0);
    let group_width = cx.px(8.0 * BUTTON_WIDTH + CARET_WIDTH + SEPARATOR_WIDTH)
        + if lfs {
            cx.px(LFS_WIDTH + LFS_MARGIN)
        } else {
            0.0
        };
    cx.set_cursor(vec2(
        origin.x
            + (cx.cursor().x - origin.x + cx.px(20.0))
                .max((width - right_group - group_width) / 2.0),
        origin.y,
    ));

    let undo_tip = state
        .undo_stack
        .last()
        .map(|entry| format!("Undo {}", entry.label));
    if toolbar_item(
        cx,
        labels,
        &ToolbarItem::new("##undo", Icon::Undo, "Undo")
            .enabled(undo_tip.is_some())
            .tip(undo_tip.as_deref()),
    )
    .clicked
    {
        app.undo(state);
    }
    let redo_tip = state
        .redo_stack
        .last()
        .map(|entry| format!("Redo {}", entry.label));
    if toolbar_item(
        cx,
        labels,
        &ToolbarItem::new("##redo", Icon::Redo, "Redo")
            .enabled(redo_tip.is_some())
            .tip(redo_tip.as_deref()),
    )
    .clicked
    {
        app.redo(state);
    }

    // The pull button runs the default operation chosen in its dropdown.
    let pull_mode = app.settings.pull_mode;
    let pull_x = cx.cursor().x;
    let pull = toolbar_item(
        cx,
        labels,
        &ToolbarItem::new("##pull", Icon::Pull, "Pull")
            .enabled(idle && head.is_some())
            .tip(head.is_some().then(|| pull_mode_label(pull_mode)))
            .dropdown(ui.is_popup_open("##pull_menu")),
    );
    if pull.clicked {
        let (title, args) = pull_command(pull_mode);
        app.run_git_cli(state, title, args.iter().copied());
    }
    if pull.dropdown {
        cx.open_popover("##pull_menu");
    }
    cx.popover(
        "##pull_menu",
        vec2(pull_x, dropdown_y),
        DROPDOWN_WIDTH,
        |cx| pull_mode_list(cx, app),
    );

    if toolbar_item(
        cx,
        labels,
        &ToolbarItem::new("##push", Icon::Push, "Push")
            .enabled(idle && head.is_some())
            .tip(head.is_some().then_some("Push the current branch")),
    )
    .clicked
        && let Some((name, upstream)) = &head
    {
        if upstream.is_none() {
            app.dialog.open(DialogKind::PushNewBranch);
            app.dialog.ref_name = name.clone();
            app.dialog.text_a = state
                .history
                .remotes
                .first()
                .map_or_else(|| "origin".to_owned(), |remote| remote.name.clone());
        } else {
            app.run_git_cli(state, "Push", ["push"]);
        }
    }
    if toolbar_item(
        cx,
        labels,
        &ToolbarItem::new("##branch", Icon::Branch, "Branch")
            .enabled(has_head)
            .tip(Some("Create a branch at HEAD (Ctrl+B)")),
    )
    .clicked
    {
        app.dialog.open(DialogKind::CreateBranch);
        app.dialog.flag = true;
        app.dialog.sha = state.history.head_sha.clone().unwrap_or_default();
    }
    if toolbar_item(
        cx,
        labels,
        &ToolbarItem::new("##stash", Icon::Stash, "Stash")
            .enabled(has_head && !state.status.is_empty())
            .tip(Some("Stash all changes")),
    )
    .clicked
    {
        stash_changes(cx, app, state, "");
    }
    if toolbar_item(
        cx,
        labels,
        &ToolbarItem::new("##pop", Icon::Pop, "Pop")
            .enabled(has_stash)
            .tip(Some("Pop the most recent stash")),
    )
    .clicked
        && !app.refuse_while_busy(state)
    {
        let result = state.repo.stash_pop(0);
        app.run(state, "Pop stash", result, Some("Stash popped"));
    }
    toolbar_separator(cx, draw, item_h);
    if toolbar_item(
        cx,
        labels,
        &ToolbarItem::new("##terminal", Icon::Terminal, "Terminal")
            .tip(Some("Toggle the terminal panel (Ctrl+`)")),
    )
    .clicked
    {
        app.show_terminal_panel = !app.show_terminal_panel;
        app.terminal.focus_requested = app.show_terminal_panel;
    }
    if lfs {
        draw_lfs(cx, app, state, dropdown_y);
    }

    // Right side: Actions, Search, then the in-progress operation and the busy indicator.
    let mut right = origin.x + width - 1.0;
    right -= cx.px(BUTTON_WIDTH);
    cx.set_cursor(vec2(right, origin.y));
    if toolbar_item(
        cx,
        labels,
        &ToolbarItem::new("##search", Icon::Search, "Search")
            .tip(Some("Search Commits (Ctrl + F)")),
    )
    .clicked
    {
        state.search_open = !state.search_open;
        state.search_focus_requested = state.search_open;
        state.run_search();
    }
    right -= cx.px(BUTTON_WIDTH);
    cx.set_cursor(vec2(right, origin.y));
    // Actions opens the command palette.
    let mut actions =
        ToolbarItem::new("##actions", Icon::Magic, "Actions").tip(Some("Actions (Ctrl+P)"));
    actions.icon = ToolbarIcon::Fa("fal:magic");
    if toolbar_item(cx, labels, &actions).clicked {
        app.fuzzy.show();
    }
    right -= cx.px(8.0);
    if let Some(operation) = &state.async_op {
        let text = format!("{}...", operation.title);
        let regular = cx.fonts.regular;
        let dims = cx.text_size(regular, FONT_SIZE_SMALL, &text);
        cx.draw_icon(
            draw,
            Icon::Spinner,
            vec2(right - dims.x - cx.px(14.0), origin.y + height / 2.0),
            cx.px(12.0),
            p.text_accent,
        );
        cx.draw_text(
            draw,
            regular,
            FONT_SIZE_SMALL,
            vec2(right - dims.x, origin.y + (height - dims.y) / 2.0),
            p.text_accent,
            &text,
        );
        right -= dims.x + cx.px(30.0);
    }
    if state.operation_state != OperationState::None {
        draw_operation_badge(cx, app, state, draw, origin, height, right);
    }
}

/// The LFS button and its dropdown, in the centre group after the Terminal button.
fn draw_lfs(cx: &mut Cx<'_>, app: &mut Application, state: &mut RepoState, dropdown_y: f32) {
    // Lfs-Fetch / Checkout / Pull / Push / Prune, the generateLfsMenuItems order.
    let lfs_x = cx.cursor().x.round();
    let busy = state
        .async_op
        .as_ref()
        .is_some_and(|operation| operation.title.starts_with("LFS "));
    if lfs_button(cx, busy, cx.ui.is_popup_open("##lfs_menu")) {
        cx.open_popover("##lfs_menu");
    }
    let remote = lfs_remote(&state.history.refs);
    cx.popover(
        "##lfs_menu",
        vec2(lfs_x, dropdown_y),
        DROPDOWN_WIDTH,
        |cx| {
            let commands: [(&str, &str, Vec<&str>); 5] = [
                ("Fetch all LFS files", "LFS Fetch", vec!["lfs", "fetch"]),
                (
                    "Checkout all LFS files",
                    "LFS Checkout",
                    vec!["lfs", "checkout"],
                ),
                ("Pull all LFS files", "LFS Pull", vec!["lfs", "pull"]),
                (
                    "Push all LFS files",
                    "LFS Push",
                    vec!["lfs", "push", "--all", "--", &remote],
                ),
                (
                    "Prune local LFS repository",
                    "LFS Prune",
                    vec!["lfs", "prune"],
                ),
            ];
            cx.popover_padding(5.0);
            for (label, title, args) in commands {
                if cx.popover_menu_item(label) {
                    cx.close_popover();
                    app.run_git_cli(state, title, args);
                }
            }
            cx.popover_padding(5.0);
        },
    );
}

/// The in-progress operation's badge (orange, with an Abort button) ending at `right`.
fn draw_operation_badge(
    cx: &mut Cx<'_>,
    app: &mut Application,
    state: &mut RepoState,
    draw: DrawList<'_>,
    origin: Vec2,
    height: f32,
    right: f32,
) {
    let p = cx.palette;
    let label = state_label(state.operation_state);
    let semibold = cx.fonts.semibold;
    let label_size = cx.text_size(semibold, FONT_SIZE_SMALL, label);
    let badge_width = label_size.x + cx.px(90.0);
    let badge_min = vec2(right - badge_width, origin.y + cx.px(10.0));
    let badge_max = vec2(right, origin.y + height - cx.px(10.0));
    let orange: Color = p.orange;
    draw.add_rect_filled(badge_min, badge_max, orange.with_alpha(0.25), cx.px(3.0));
    cx.draw_icon(
        draw,
        Icon::Warning,
        vec2(badge_min.x + cx.px(14.0), (badge_min.y + badge_max.y) / 2.0),
        cx.px(14.0),
        orange,
    );
    cx.draw_text(
        draw,
        semibold,
        FONT_SIZE_SMALL,
        vec2(
            badge_min.x + cx.px(26.0),
            (badge_min.y + badge_max.y) / 2.0 - label_size.y / 2.0,
        ),
        p.text_selected,
        label,
    );
    cx.set_cursor(vec2(badge_max.x - cx.px(56.0), badge_min.y + cx.px(2.0)));
    if cx.danger_button(
        "Abort",
        vec2(cx.px(52.0), badge_max.y - badge_min.y - cx.px(4.0)),
        true,
    ) && !app.refuse_while_busy(state)
    {
        let result = state.repo.abort_operation();
        app.run(state, "Abort", result, Some("Operation aborted"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dear_imgui_rs::Key;
    use imgit_git::Repository;

    use crate::shell::test_frame::TestFrames;

    fn toolbar_fixture() -> (tempfile::TempDir, Repository, Application, String) {
        let scratch = tempfile::tempdir().unwrap();
        let repo = Repository::init(scratch.path().join("repo")).unwrap();
        repo.set_local_config("user.name", Some("Toolbar test"))
            .unwrap();
        repo.set_local_config("user.email", Some("toolbar@example.invalid"))
            .unwrap();
        repo.set_local_config("core.autocrlf", Some("false"))
            .unwrap();
        fs::write(repo.workdir().join("file.txt"), "Before\n").unwrap();
        repo.stage(&["file.txt"]).unwrap();
        let base = repo.commit("Base", false).unwrap();
        let app = Application::with_config_directory(scratch.path().join("config"));
        (scratch, repo, app, base)
    }

    fn toolbar_key(
        frames: &mut TestFrames,
        app: &mut Application,
        state: &mut RepoState,
        key: Key,
    ) {
        frames.set_key(key, true);
        frames.draw(|cx| draw_toolbar(cx, app, state));
        frames.set_key(key, false);
        frames.draw(|cx| draw_toolbar(cx, app, state));
    }

    fn center_group(cx: &Cx<'_>) -> Vec2 {
        let toolbar_width = cx.avail().x;
        let group_width = cx.px(8.0 * BUTTON_WIDTH + CARET_WIDTH + SEPARATOR_WIDTH);
        let tools_width = cx.px(2.0 * BUTTON_WIDTH + 1.0);
        cx.cursor()
            + vec2(
                (toolbar_width - tools_width - group_width) / 2.0,
                cx.px(TOOLBAR_HEIGHT / 2.0),
            )
    }

    #[test]
    fn toolbar_actions_target_head_and_open_the_expected_panels() {
        let (_scratch, repo, mut app, head) = toolbar_fixture();
        repo.add_remote("backup", ".").unwrap();
        app.open_repository(&repo.workdir().to_string_lossy());
        app.with_active(|app, state| {
            let mut frames = TestFrames::new();
            let mut group = Vec2::ZERO;
            let mut search = Vec2::ZERO;
            for _ in 0..2 {
                frames.draw(|cx| {
                    group = center_group(cx);
                    search = cx.cursor()
                        + vec2(
                            cx.avail().x - BUTTON_WIDTH / 2.0 - 1.0,
                            TOOLBAR_HEIGHT / 2.0,
                        );
                    draw_toolbar(cx, app, state);
                });
            }
            let button = |index: f32| {
                group
                    + vec2(
                        BUTTON_WIDTH * (index + 0.5)
                            + CARET_WIDTH
                            + if index == 7.0 { SEPARATOR_WIDTH } else { 0.0 },
                        0.0,
                    )
            };
            frames.click_left(button(3.0), |cx| draw_toolbar(cx, app, state));
            assert!(app.dialog.is(DialogKind::PushNewBranch));
            assert_eq!(
                Some(app.dialog.ref_name.as_str()),
                state.history.head_branch.as_deref()
            );
            assert_eq!(app.dialog.text_a, "backup");
            assert!(!state.busy());
            app.dialog.close();

            frames.click_left(button(4.0), |cx| draw_toolbar(cx, app, state));
            assert!(app.dialog.is(DialogKind::CreateBranch));
            assert_eq!(app.dialog.sha, head);
            assert!(app.dialog.flag);
            app.dialog.close();

            app.show_terminal_panel = false;
            frames.click_left(button(7.0), |cx| draw_toolbar(cx, app, state));
            assert!(app.show_terminal_panel && app.terminal.focus_requested);
            frames.click_left(button(7.0), |cx| draw_toolbar(cx, app, state));
            assert!(!app.show_terminal_panel && !app.terminal.focus_requested);

            state.search_text = "Base".to_owned();
            frames.click_left(search, |cx| draw_toolbar(cx, app, state));
            assert!(state.search_open && state.search_focus_requested);
            assert_eq!(state.search_match_sha.as_deref(), Some(head.as_str()));
            frames.click_left(search, |cx| draw_toolbar(cx, app, state));
            assert!(!state.search_open && !state.search_focus_requested);
            assert!(state.search_matches.is_empty());

            app.fuzzy.text = "previous query".to_owned();
            frames.click_left(search - vec2(BUTTON_WIDTH, 0.0), |cx| {
                draw_toolbar(cx, app, state)
            });
            assert!(app.fuzzy.open && app.fuzzy.focus_requested);
            assert!(app.fuzzy.text.is_empty());
            assert_eq!(state.history.head_sha.as_deref(), Some(head.as_str()));
        })
        .unwrap();
    }

    #[test]
    fn the_push_button_updates_the_configured_upstream() {
        let (scratch, repo, mut app, _) = toolbar_fixture();
        let remote = scratch.path().join("upstream.git");
        let git = |dir: &Path, args: &[&str]| {
            let output = imgit_git::cli::run(dir, args, imgit_git::cli::RunOptions::default());
            assert!(output.success(), "git {args:?}: {}", output.text);
            output.text.trim().to_owned()
        };
        git(
            scratch.path(),
            &["init", "--bare", &remote.to_string_lossy()],
        );
        let branch = git(repo.workdir(), &["symbolic-ref", "--short", "HEAD"]);
        repo.add_remote("origin", &remote.to_string_lossy())
            .unwrap();
        repo.set_local_config("push.default", Some("simple"))
            .unwrap();
        git(
            repo.workdir(),
            &["push", "--set-upstream", "--", "origin", &branch],
        );
        let old_tip = git(&remote, &["rev-parse", &format!("refs/heads/{branch}")]);
        fs::write(repo.workdir().join("file.txt"), "After\n").unwrap();
        repo.stage(&["file.txt"]).unwrap();
        let head = repo.commit("Ready to push", false).unwrap();
        assert_ne!(old_tip, head);
        app.open_repository(&repo.workdir().to_string_lossy());

        let mut frames = TestFrames::new();
        app.with_active(|app, state| {
            let mut push = Vec2::ZERO;
            for _ in 0..2 {
                frames.draw(|cx| {
                    push = center_group(cx) + vec2(3.5 * BUTTON_WIDTH + CARET_WIDTH, 0.0);
                    draw_toolbar(cx, app, state);
                });
            }
            frames.click_left(push, |cx| draw_toolbar(cx, app, state));
            assert!(!app.dialog.is_open());
            let operation = state.async_op.as_ref().unwrap();
            assert_eq!(operation.title, "Push");
            assert_eq!(operation.args, ["push"]);
        })
        .unwrap();

        finish_toolbar_command(&mut app, "Push");
        assert_ne!(
            app.toasts.history()[0].kind,
            crate::app::toasts::ToastKind::Error
        );
        assert_eq!(
            git(&remote, &["rev-parse", &format!("refs/heads/{branch}")]),
            head
        );
        assert_eq!(app.active().unwrap().head_ref().unwrap().ahead, 0);
    }

    fn finish_toolbar_command(app: &mut Application, title: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while app.active().unwrap().busy() {
            assert!(
                std::time::Instant::now() < deadline,
                "{title} did not finish"
            );
            app.poll_async();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn the_lfs_dropdown_runs_checkout_and_stays_closed_while_it_is_busy() {
        let (_scratch, repo, mut app, head) = toolbar_fixture();
        fs::write(
            repo.workdir().join(".gitattributes"),
            "*.asset filter=lfs\n",
        )
        .unwrap();
        let available = imgit_git::cli::run(
            repo.workdir(),
            ["lfs", "version"],
            imgit_git::cli::RunOptions::default(),
        )
        .success();
        app.open_repository(&repo.workdir().to_string_lossy());
        let mut frames = TestFrames::new();
        app.with_active(|app, state| {
            let mut button = Vec2::ZERO;
            for _ in 0..2 {
                frames.draw(|cx| {
                    let normal = 8.0 * BUTTON_WIDTH + CARET_WIDTH + SEPARATOR_WIDTH;
                    button = center_group(cx)
                        + vec2(
                            normal - (LFS_WIDTH + LFS_MARGIN) / 2.0 + LFS_WIDTH / 2.0,
                            0.0,
                        );
                    draw_toolbar(cx, app, state);
                });
            }
            frames.click_left(button, |cx| draw_toolbar(cx, app, state));
            toolbar_key(&mut frames, app, state, Key::DownArrow);
            toolbar_key(&mut frames, app, state, Key::DownArrow);
            toolbar_key(&mut frames, app, state, Key::Enter);
            let operation = state.async_op.as_ref().unwrap();
            assert_eq!(operation.title, "LFS Checkout");
            assert_eq!(operation.args, ["lfs", "checkout"]);
            let toasts = app.toasts.pushed();
            frames.click_left(button, |cx| draw_toolbar(cx, app, state));
            toolbar_key(&mut frames, app, state, Key::DownArrow);
            toolbar_key(&mut frames, app, state, Key::Enter);
            assert_eq!(app.toasts.pushed(), toasts);
        })
        .unwrap();

        finish_toolbar_command(&mut app, "LFS Checkout");
        assert_eq!(
            app.toasts.history()[0].kind == crate::app::toasts::ToastKind::Error,
            !available,
            "{:?}",
            app.toasts.history()[0],
        );
        assert_eq!(
            app.active().unwrap().history.head_sha.as_deref(),
            Some(head.as_str())
        );
        assert_eq!(
            fs::read_to_string(repo.workdir().join("file.txt")).unwrap(),
            "Before\n"
        );
    }

    #[test]
    fn an_outer_submodule_crumb_waits_for_the_inner_command_then_closes_both_levels() {
        use imgit_app::{job::Job, repo_state::AsyncOperation};

        let (_scratch, root, mut app, _) = toolbar_fixture();
        let child = Repository::init(root.workdir().join("child")).unwrap();
        let leaf = Repository::init(child.workdir().join("leaf")).unwrap();
        app.open_repository(&root.workdir().to_string_lossy());
        app.with_active(|app, state| app.open_submodule(state, "child"))
            .unwrap();
        app.with_active(|app, state| app.open_submodule(state, "leaf"))
            .unwrap();
        app.active_mut().unwrap().async_op = Some(AsyncOperation {
            title: "Read worktree status".to_owned(),
            job: Job::idle(),
            silent: true,
            args: Vec::new(),
            attempt: None,
            queued: Vec::new(),
        });

        let mut frames = TestFrames::new();
        let mut close = Vec2::ZERO;
        let draw = |cx: &mut Cx<'_>, app: &mut Application| {
            app.with_active(|app, state| draw_toolbar(cx, app, state))
                .unwrap();
        };
        for _ in 0..2 {
            frames.draw(|cx| {
                let root_width = cx.text_size(cx.fonts.bold, CRUMB_VALUE_FONT, root.name()).x;
                close =
                    cx.cursor() + vec2(crumb_width(|px| cx.px(px), root_width) + 13.0 + 27.0, 30.0);
                draw(cx, &mut app);
            });
        }
        frames.click_left(close, |cx| draw(cx, &mut app));
        assert_eq!(app.active_workdir(), Some(leaf.workdir()));
        assert!(app.panels.toolbar.close_to_depth.is_none());
        app.active_mut().unwrap().async_op = None;

        frames.move_mouse(close.into());
        frames.draw(|cx| draw(cx, &mut app));
        frames.set_left_mouse(true);
        frames.draw(|cx| draw(cx, &mut app));
        assert_eq!(app.active_workdir(), Some(child.workdir()));
        assert_eq!(app.panels.toolbar.close_to_depth, Some(0));
        frames.set_left_mouse(false);
        frames.draw(|cx| draw(cx, &mut app));
        assert_eq!(app.active_workdir(), Some(root.workdir()));
        frames.draw(|cx| draw(cx, &mut app));
        assert!(app.panels.toolbar.close_to_depth.is_none());
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.closed_submodules.len(), 2);
    }

    #[test]
    fn the_pull_dropdown_saves_fetch_as_the_default_and_runs_it() {
        let (_scratch, repo, mut app, _) = toolbar_fixture();
        app.open_repository(&repo.workdir().to_string_lossy());
        app.settings.pull_mode = PullMode::FastForwardIfPossible;
        let mut frames = TestFrames::new();
        app.with_active(|app, state| {
            let mut pull = Vec2::ZERO;
            for _ in 0..2 {
                frames.draw(|cx| {
                    pull = center_group(cx) + vec2(2.0 * BUTTON_WIDTH, 0.0);
                    draw_toolbar(cx, app, state);
                });
            }
            frames.click_left(pull + vec2(BUTTON_WIDTH + CARET_WIDTH / 2.0, 0.0), |cx| {
                draw_toolbar(cx, app, state)
            });
            toolbar_key(&mut frames, app, state, Key::DownArrow);
            toolbar_key(&mut frames, app, state, Key::Enter);
            assert_eq!(app.settings.pull_mode, PullMode::FetchAll);
            assert!(!state.busy());
            let mut saved = crate::app::settings::Settings::default();
            saved.load(&app.config_path("settings.txt"));
            assert_eq!(saved.pull_mode, PullMode::FetchAll);

            frames.click_left(pull + vec2(BUTTON_WIDTH / 2.0, 0.0), |cx| {
                draw_toolbar(cx, app, state)
            });
            let operation = state.async_op.as_ref().unwrap();
            assert_eq!(operation.title, "Fetch");
            assert_eq!(operation.args.first().map(String::as_str), Some("fetch"));
            assert!(operation.args.iter().any(|arg| arg == "--all"));
            assert!(operation.args.iter().any(|arg| arg == "--prune"));
        })
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.active().unwrap().busy() {
            assert!(std::time::Instant::now() < deadline, "fetch did not finish");
            app.poll_async();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            app.toasts
                .history()
                .iter()
                .all(|entry| entry.kind != crate::app::toasts::ToastKind::Error)
        );
    }

    #[test]
    fn searching_the_branch_breadcrumb_checks_out_only_the_matching_branch() {
        let (_scratch, repo, mut app, base) = toolbar_fixture();
        repo.create_branch("topic/match", &base, false).unwrap();
        repo.create_branch("topic/other", &base, false).unwrap();
        let head = repo.commit("New HEAD", false).unwrap();
        app.open_repository(&repo.workdir().to_string_lossy());
        let mut frames = TestFrames::new();
        app.with_active(|app, state| {
            let mut crumb = Vec2::ZERO;
            for _ in 0..2 {
                frames.draw(|cx| {
                    let name = app.repo_display_name(repo.workdir());
                    let value_width = cx.text_size(cx.fonts.bold, CRUMB_VALUE_FONT, &name).x;
                    let repo_width = crumb_width(|value| cx.px(value), value_width);
                    crumb = cx.cursor() + vec2(repo_width + 13.0 + 55.0, TOOLBAR_HEIGHT / 2.0);
                    draw_toolbar(cx, app, state);
                });
            }
            frames.click_left(crumb, |cx| draw_toolbar(cx, app, state));
            for _ in 0..2 {
                frames.draw(|cx| draw_toolbar(cx, app, state));
            }
            frames.type_text("match");
            frames.draw(|cx| draw_toolbar(cx, app, state));
            assert_eq!(app.panels.toolbar.crumb_search, "match");
            toolbar_key(&mut frames, app, state, Key::DownArrow);
            toolbar_key(&mut frames, app, state, Key::Enter);
            assert_eq!(state.history.head_branch.as_deref(), Some("topic/match"));
            assert_eq!(state.history.head_sha.as_deref(), Some(base.as_str()));

            app.undo(state);
            assert_eq!(state.history.head_sha.as_deref(), Some(head.as_str()));
        })
        .unwrap();
    }

    #[test]
    fn the_repository_breadcrumb_opens_a_filtered_recent_repository_or_its_management_page() {
        let (scratch, repo, mut app, _) = toolbar_fixture();
        let other = Repository::init(scratch.path().join("review-target")).unwrap();
        app.open_repository(&other.workdir().to_string_lossy());
        app.open_repository(&repo.workdir().to_string_lossy());
        let mut frames = TestFrames::new();
        let select = |frames: &mut TestFrames, app: &mut Application, query: &str| {
            app.with_active(|app, state| {
                let mut crumb = Vec2::ZERO;
                for _ in 0..2 {
                    frames.draw(|cx| {
                        crumb = cx.cursor() + vec2(55.0, TOOLBAR_HEIGHT / 2.0);
                        draw_toolbar(cx, app, state);
                    });
                }
                frames.click_left(crumb, |cx| draw_toolbar(cx, app, state));
                for _ in 0..2 {
                    frames.draw(|cx| draw_toolbar(cx, app, state));
                }
                frames.type_text(query);
                frames.draw(|cx| draw_toolbar(cx, app, state));
                assert_eq!(app.panels.toolbar.crumb_search, query);
                toolbar_key(frames, app, state, Key::Enter);
            })
            .unwrap();
        };

        select(&mut frames, &mut app, "review-target");
        assert_eq!(app.active_workdir(), Some(other.workdir()));
        assert_eq!(app.tabs.len(), 2);
        assert!(!app.repo_management_open);
        select(&mut frames, &mut app, "no-such-repository");
        assert!(app.repo_management_open);
        assert_eq!(app.active_workdir(), Some(other.workdir()));
    }

    #[test]
    fn toolbar_stash_can_be_undone_redone_and_popped() {
        let (_scratch, repo, mut app, base) = toolbar_fixture();
        let file = repo.workdir().join("file.txt");
        fs::write(&file, "Working change\n").unwrap();
        app.open_repository(&repo.workdir().to_string_lossy());
        let mut frames = TestFrames::new();
        app.with_active(|app, state| {
            let mut group = Vec2::ZERO;
            for _ in 0..2 {
                frames.draw(|cx| {
                    group = center_group(cx);
                    draw_toolbar(cx, app, state);
                });
            }
            let button = |index: usize| {
                let split = if index > 2 { CARET_WIDTH } else { 0.0 };
                group + vec2(BUTTON_WIDTH * (index as f32 + 0.5) + split, 0.0)
            };
            let read = || fs::read_to_string(&file).unwrap();
            frames.click_left(button(5), |cx| draw_toolbar(cx, app, state));
            assert_eq!(read(), "Before\n");
            assert!(state.status.is_empty());

            frames.click_left(button(0), |cx| draw_toolbar(cx, app, state));
            assert_eq!(read(), "Working change\n");
            assert_eq!(state.status.unstaged.len(), 1);
            frames.click_left(button(1), |cx| draw_toolbar(cx, app, state));
            assert_eq!(read(), "Before\n");
            assert!(state.status.is_empty());

            frames.click_left(button(6), |cx| draw_toolbar(cx, app, state));
            assert_eq!(read(), "Working change\n");
            assert_eq!(state.status.unstaged.len(), 1);
            assert!(
                state
                    .history
                    .refs
                    .iter()
                    .all(|reference| reference.ref_type != imgit_git::RefType::Stash)
            );
            assert_eq!(state.history.head_sha.as_deref(), Some(base.as_str()));
        })
        .unwrap();
    }

    #[test]
    fn the_toolbar_abort_button_restores_head_after_a_merge_conflict() {
        let (_scratch, repo, mut app, base) = toolbar_fixture();
        repo.create_branch("topic", &base, true).unwrap();
        let file = repo.workdir().join("file.txt");
        fs::write(&file, "Their change\n").unwrap();
        repo.stage(&["file.txt"]).unwrap();
        repo.commit("Their change", false).unwrap();
        repo.create_branch("current", &base, true).unwrap();
        fs::write(&file, "Our change\n").unwrap();
        repo.stage(&["file.txt"]).unwrap();
        let head = repo.commit("Our change", false).unwrap();
        assert_eq!(
            repo.merge("topic", false).unwrap(),
            imgit_git::repository::Outcome::Conflicts
        );
        app.open_repository(&repo.workdir().to_string_lossy());

        let mut frames = TestFrames::new();
        app.with_active(|app, state| {
            let mut abort = Vec2::ZERO;
            for _ in 0..2 {
                frames.draw(|cx| {
                    let right = cx.avail().x - 1.0 - 2.0 * BUTTON_WIDTH - 8.0;
                    abort = cx.cursor() + vec2(right - 30.0, TOOLBAR_HEIGHT / 2.0);
                    draw_toolbar(cx, app, state);
                });
            }
            assert_eq!(state.operation_state, OperationState::Merge);
            frames.click_left(abort, |cx| draw_toolbar(cx, app, state));
            assert_eq!(state.operation_state, OperationState::None);
            assert_eq!(state.history.head_sha.as_deref(), Some(head.as_str()));
            assert_eq!(fs::read_to_string(&file).unwrap(), "Our change\n");
            assert!(state.status.is_empty());
            assert_eq!(app.toasts.history()[0].title, "Operation aborted");
        })
        .unwrap();
    }

    fn at_100(logical: f32) -> f32 {
        logical.round()
    }

    #[test]
    fn a_background_git_operation_rotates_the_toolbar_spinner_until_it_finishes() {
        use imgit_app::{job::Job, repo_state::AsyncOperation};
        use imgit_git::Repository;

        use crate::shell::test_frame::TestFrames;

        let scratch = tempfile::tempdir().unwrap();
        let mut app = Application::with_config_directory(scratch.path().join("config"));
        let repo = scratch.path().join("repo");
        Repository::init(&repo).unwrap();
        app.open_repository(&repo.to_string_lossy());

        let mut frames = TestFrames::new();
        app.with_active(|app, state| {
            let draw = |frames: &mut TestFrames, app: &mut Application, state: &mut RepoState| {
                frames.draw(|cx| draw_toolbar(cx, app, state))
            };
            // Let the toolbar's initial hover transitions settle.
            for _ in 0..4 {
                draw(&mut frames, app, state);
            }
            assert!(!draw(&mut frames, app, state));
            let before = frames.uploads().len();

            state.async_op = Some(AsyncOperation {
                title: "Auto-fetch".to_owned(),
                job: Job::idle(),
                silent: true,
                args: Vec::new(),
                attempt: None,
                queued: Vec::new(),
            });
            assert!(draw(&mut frames, app, state));
            assert_eq!(frames.uploads().len(), before + 1);
            assert!(draw(&mut frames, app, state));
            assert_eq!(frames.uploads().len(), before + 2);
            assert_ne!(frames.uploads()[before], frames.uploads()[before + 1]);

            state.async_op = None;
            assert!(!draw(&mut frames, app, state));
            assert_eq!(frames.uploads().len(), before + 2);
        })
        .unwrap();
    }

    #[test]
    fn a_crumb_is_at_least_110_wide_and_at_most_a_320_wide_value_plus_padding() {
        assert_eq!(crumb_width(at_100, 20.0), 110.0);
        assert_eq!(crumb_width(at_100, 100.0), 148.0);
        assert_eq!(crumb_width(at_100, 900.0), 20.0 + 320.0 + 28.0);
    }

    #[test]
    fn a_submodule_crumb_has_room_for_its_cross() {
        assert_eq!(submodule_crumb_width(at_100, 10.0), 110.0);
        assert_eq!(submodule_crumb_width(at_100, 100.0), 160.0);
    }

    #[test]
    fn the_pull_button_runs_what_the_mode_says() {
        assert_eq!(
            pull_command(PullMode::FetchAll),
            ("Fetch", &["fetch", "--all", "--prune"][..])
        );
        assert_eq!(
            pull_command(PullMode::FastForwardIfPossible),
            ("Pull", &["pull", "--ff"][..])
        );
        assert_eq!(
            pull_command(PullMode::FastForwardOnly).1,
            ["pull", "--ff-only"]
        );
        assert_eq!(pull_command(PullMode::Rebase).1, ["pull", "--rebase"]);
        assert_eq!(
            PULL_MODES.map(pull_mode_label),
            [
                "Fetch All",
                "Pull (fast-forward if possible)",
                "Pull (fast-forward only)",
                "Pull (rebase)"
            ]
        );
    }

    #[test]
    fn lfs_pushes_to_the_upstream_remote_of_the_checked_out_branch() {
        let branch = |name: &str, head: bool, upstream: Option<&str>| Ref {
            name: name.to_owned(),
            is_head: head,
            upstream: upstream.map(str::to_owned),
            ..Ref::default()
        };
        assert_eq!(lfs_remote(&[]), "origin");
        assert_eq!(lfs_remote(&[branch("main", true, None)]), "origin");
        assert_eq!(
            lfs_remote(&[
                branch("old", false, Some("backup/old")),
                branch("main", true, Some("upstream/main")),
            ]),
            "upstream"
        );
    }

    #[test]
    fn lfs_is_detected_from_the_lfs_folder_or_the_attributes() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!uses_lfs(dir.path()));
        fs::write(dir.path().join(".gitattributes"), "*.txt text\n").unwrap();
        assert!(!uses_lfs(dir.path()));
        fs::write(
            dir.path().join(".gitattributes"),
            "*.txt text\n*.psd filter=lfs diff=lfs merge=lfs -text\n",
        )
        .unwrap();
        assert!(uses_lfs(dir.path()));
        fs::remove_file(dir.path().join(".gitattributes")).unwrap();
        fs::create_dir_all(dir.path().join(".git").join("lfs")).unwrap();
        assert!(uses_lfs(dir.path()));
    }

    #[test]
    fn operation_labels() {
        assert_eq!(state_label(OperationState::Merge), "Merge in progress");
        assert_eq!(
            state_label(OperationState::CherryPick),
            "Cherry pick in progress"
        );
        assert_eq!(state_label(OperationState::None), "");
    }
}
