//! The commit graph: the column header (drag to reorder / resize, the header menu), the rows with their ref
//! pills, lanes and nodes, the commit search bar, selection, hover highlights, timeline markers and the
//! sideways scroll of the graph zone.

mod changes;
mod lanes;
mod refs;
mod rows;
mod search_bar;
mod zones;

use dear_imgui_rs::{ChildFlags, MouseButton, StyleColor, WindowFlags, sys};
use imgit_app::RepoState;
use imgit_git::Ref;

use self::changes::ChangeCounts;
use self::refs::{RefGroupKey, RefInteraction};
use self::zones::{Metrics, ZoneLayout, ZoneType, Zones};
use crate::app::application::Application;
use crate::theme::{GRAPH_HEADER_HEIGHT, GRAPH_HEADER_MARGIN_BOTTOM, GRAPH_LEFT_MARGIN};
use crate::ui::draw::DrawList;
use crate::ui::geom::{Rect, Vec2, vec2};
use crate::ui::menu::popup_menu;
use crate::ui::panels::menus::graph_header_menu;
use crate::ui::widgets::ResizeAxis;
use crate::ui::{Cx, panels};
use crate::window::CursorShape;

/// The zone whose right edge is being dragged (re-resizable: its width when pressed plus the mouse's travel).
#[derive(Debug, Clone, Copy, PartialEq)]
struct ZoneDrag {
    index: usize,
    start_width: f32,
    start_mouse: f32,
}

/// A header cell held and moved 4px becomes a column drag: the cell follows the mouse as a floating copy and the
/// zone swaps places with a neighbour once the cell's leading edge passes that neighbour's centre (its centre
/// would have to travel half its own width, far for the message column); the order is saved on release.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ZoneMove {
    zone: ZoneType,
    active: bool,
    press_x: f32,
    /// The press, from the cell's left edge.
    grab: f32,
    /// The cell.
    width: f32,
}

/// The graph's state between frames (what the C++ kept in file-level and function-local statics).
#[derive(Default)]
pub struct State {
    zones: ZoneLayout,
    zone_drag: Option<ZoneDrag>,
    zone_move: Option<ZoneMove>,
    /// The graph zone's scrollLeft, CSS px.
    graph_scroll: f32,
    refs: RefInteraction,
    context_sha: String,
    context_ref: Option<Ref>,
    /// For dimming the rows it does not reach.
    hovered_group: Option<RefGroupKey>,
    /// The row whose ref zone is hovered lists all its ref groups in a popover over the rows below.
    hovered_zone_sha: Option<String>,
    zone_popover_ghost: bool,
    zone_popover_row: Option<usize>,
    dimming_for: Option<RefGroupKey>,
    dim_since: f64,
    /// The search input takes the focus back after Enter.
    search_refocus: bool,
    inline_ref_input_id: usize,
    changes: ChangeCounts,
}

/// What the graph asks for once its rows are drawn: they read the history, and these change it.
#[derive(Default)]
struct Deferred {
    checkout: Option<Ref>,
    show_more: bool,
    inline_done: Option<imgit_app::repo_state::InlineRef>,
}

/// The measures of a frame of the header: where everything is, in device px.
struct Geometry {
    m: Metrics,
    origin: Vec2,
    size: Vec2,
    scrollbar: f32,
    gear_w: f32,
    header_y: f32,
    header_h: f32,
    zones_x: f32,
    z: Zones,
}

/// The graph, `size` device px.
pub fn draw_graph_view(cx: &mut Cx<'_>, app: &mut Application, state: &mut RepoState, size: Vec2) {
    app.with_panel_state(
        |panels| &mut panels.graph_view,
        |app, graph| graph.draw(cx, app, state, size),
    );
}

/// `ImGui::GetStyle().ScrollbarSize` and `ItemSpacing.x`.
fn style_sizes(cx: &Cx<'_>) -> (f32, f32) {
    let style = cx.ui.clone_style();
    (style.scrollbar_size(), style.item_spacing()[0])
}

impl State {
    /// Whether background work a screenshot should wait for is running (the CHANGES column's counts).
    #[must_use]
    pub fn busy(&self) -> bool {
        self.changes.busy()
    }

    fn draw(&mut self, cx: &mut Cx<'_>, app: &mut Application, state: &mut RepoState, size: Vec2) {
        let ui = cx.ui;
        let origin = cx.cursor();
        let deferred = ui
            .child_window("##graph_view")
            .size(size.into())
            .child_flags(ChildFlags::NONE)
            .flags(WindowFlags::NO_SCROLLBAR)
            .build(ui, || self.draw_content(cx, app, state, origin, size))
            .flatten();
        // Without a load error: the search bar floats over the graph.
        let Some(deferred) = deferred else {
            return;
        };
        search_bar::commit_search_bar(cx, state, &mut self.search_refocus, origin, size);

        if let Some(r) = deferred.checkout {
            let _ = panels::checkout_ref(cx, app, state, &r);
        }
        if deferred.show_more {
            state.commit_limit += 1000;
            state.refresh();
        }
        if let Some(inline) = deferred.inline_done {
            if inline.tag {
                panels::create_tag_at(cx, app, state, &inline.name, &inline.sha, "");
            } else {
                panels::create_branch_at(
                    cx,
                    app,
                    state,
                    &inline.name,
                    &inline.sha,
                    inline.checkout,
                );
            }
        }
    }

    fn draw_content(
        &mut self,
        cx: &mut Cx<'_>,
        app: &mut Application,
        state: &mut RepoState,
        origin: Vec2,
        size: Vec2,
    ) -> Option<Deferred> {
        let ui = cx.ui;
        let p = cx.palette;
        if let Some(error) = &state.load_error {
            cx.set_cursor(origin + vec2(cx.px(20.0), cx.px(20.0)));
            let _color = ui.push_style_color(StyleColor::Text, p.red);
            ui.text_wrapped(error);
            return None;
        }

        let m = Metrics::new(app.settings.compact_graph, cx.scale);
        let header_h = cx.px(GRAPH_HEADER_HEIGHT);
        let (scrollbar, _) = style_sizes(cx);
        let gear_w = cx.px(26.0);
        // The zones fill the graph from the rows' left edge, 4px left of the first header cell, to the panel's
        // right edge; the last one ends at the column settings button for the headers and rows drawn here.
        let zones_x = origin.x + cx.px(GRAPH_LEFT_MARGIN);
        let width = ((size.x - cx.px(GRAPH_LEFT_MARGIN - 4.0)) / cx.scale).floor();
        if self
            .zones
            .update(&app.settings, width, state.layout.max_columns, &m)
        {
            self.zone_drag = None;
        }
        self.apply_zone_drag(cx, app);
        let mut z = Zones::place(&self.zones, zones_x, cx.scale);
        z.extend_last(
            &self.zones,
            origin.x + size.x - scrollbar - gear_w,
            cx.scale,
        );
        let mut g = Geometry {
            m,
            origin,
            size,
            scrollbar,
            gear_w,
            header_y: origin.y,
            header_h,
            zones_x,
            z,
        };
        self.draw_header(cx, app, &mut g);

        let rows_top = g.header_y + header_h + cx.px(GRAPH_HEADER_MARGIN_BOTTOM);
        cx.set_cursor(vec2(origin.x, rows_top));
        ui.child_window("##graph_rows")
            .size([size.x, origin.y + size.y - rows_top])
            .build(ui, || self.draw_rows(cx, app, state, &mut g, rows_top))
    }

    /// Follows the mouse while a zone's edge is held; on release the zone keeps the width as its preferred one
    /// and the column resize saves it.
    fn apply_zone_drag(&mut self, cx: &Cx<'_>, app: &mut Application) {
        let Some(drag) = self.zone_drag else {
            return;
        };
        if drag.index + 1 >= self.zones.zones.len() {
            self.zone_drag = None;
            return;
        }
        let zone = self.zones.zones[drag.index];
        if cx.ui.is_mouse_down(MouseButton::Left) {
            let travel = (cx.mouse_pos().x - drag.start_mouse) / cx.scale;
            let width = zone
                .minimum
                .max((drag.start_width + travel).min(self.zones.max_zone_width(drag.index)));
            if width != zone.current {
                self.zones.resize_zone(drag.index, width, false);
            }
            return;
        }
        if zone.current != drag.start_width {
            self.zones.resize_zone(drag.index, zone.current, true);
            app.settings
                .graph_column_widths
                .insert(zone.zone.key().to_owned(), zone.current);
            app.settings.save(&app.config_path("settings.txt"));
        }
        self.zone_drag = None;
    }

    /// The handles on the right edge of every zone but the last: `header` ones fill the header cell's last 10px
    /// and reach 4px over the border into the next cell, so hovering the border shows the resize cursor rather
    /// than the next cell's grab hand; the others, over the rows, are 10px centred 4px left of it (the rows sit
    /// 4px left of the header).
    fn zone_handles(&mut self, cx: &mut Cx<'_>, x: f32, top: f32, bottom: f32, header: bool) {
        for i in 0..self.zones.zones.len().saturating_sub(1) {
            let right =
                zone_right(&self.zones, x, i, cx.scale) - if header { 0.0 } else { cx.px(4.0) };
            let left_reach = if header { cx.px(10.0) } else { cx.px(5.0) };
            let right_reach = if header { cx.px(4.0) } else { cx.px(5.0) };
            let key = self.zones.zones[i].zone.key();
            let id = format!(
                "{}{key}",
                if header {
                    "##zone_header_resize_"
                } else {
                    "##zone_resize_"
                }
            );
            let handle = cx.resize_handle(
                &id,
                vec2(right - left_reach, top),
                vec2(right + right_reach, bottom),
                ResizeAxis::Columns,
            );
            if handle.activated {
                self.zone_drag = Some(ZoneDrag {
                    index: i,
                    start_width: self.zones.zones[i].current,
                    start_mouse: cx.mouse_pos().x,
                });
            }
        }
    }

    fn draw_header_cells(&self, cx: &Cx<'_>, draw: DrawList<'_>, g: &Geometry) {
        draw.add_rect_filled(
            vec2(g.origin.x, g.header_y),
            vec2(g.origin.x + g.size.x, g.header_y + g.header_h),
            cx.palette.panel_bg0,
            0.0,
        );
        for (i, zone) in self.zones.zones.iter().enumerate() {
            let ty = zone.zone;
            header_cell(
                cx,
                draw,
                (g.z.x(ty), g.z.w(ty)),
                (g.header_y, g.header_h),
                header_label(&g.z, &g.m, ty),
                i > 0,
            );
        }
    }

    fn draw_header(&mut self, cx: &mut Cx<'_>, app: &mut Application, g: &mut Geometry) {
        let ui = cx.ui;
        let p = cx.palette;
        let draw = cx.window_draw_list();
        let (origin, size, header_y, header_h) = (g.origin, g.size, g.header_y, g.header_h);
        // Header (.panel-bg0 .graph-header)
        self.draw_header_cells(cx, draw, g);
        self.zone_handles(cx, g.zones_x, header_y, header_y + header_h, true);
        {
            // .columns-settings: far:cog (12px, text-disabled) 9px from the right edge; it opens the same native
            // menu as a right-click on the header.
            let gear_min = vec2(origin.x + size.x - cx.px(16.0), header_y);
            cx.set_cursor(gear_min);
            let gear = ui.invisible_button("##column_settings", vec2(cx.px(16.0), header_h));
            let gear_hovered = ui.is_item_hovered();
            cx.draw_fa_icon(
                draw,
                "far:cog",
                vec2(origin.x + size.x - cx.px(9.0), header_y + cx.px(12.17)),
                cx.px(12.0),
                if gear_hovered {
                    p.text_selected
                } else {
                    p.text_disabled
                },
            );
            if gear_hovered {
                cx.styled_tooltip(
                    gear_min,
                    vec2(gear_min.x + cx.px(16.0), header_y + header_h),
                    "Column settings",
                    true,
                );
            }
            if gear {
                let items = graph_header_menu(cx, app);
                popup_menu(cx, items, None);
            }
        }
        // The header cells, after the resize handles and the gear so those win the hover: pressing one and moving
        // 4px drags the column.
        {
            let mouse = cx.mouse_pos();
            for i in 0..self.zones.zones.len() {
                let ty = self.zones.zones[i].zone;
                let (x, w) = (g.z.x(ty), g.z.w(ty));
                if w <= 0.0 {
                    continue;
                }
                cx.set_cursor(vec2(x, header_y));
                ui.invisible_button(format!("##zone_header_{}", ty.key()), vec2(w, header_h));
                // cursor: grab over the header cells. `is_item_hovered()` goes by the rectangle alone, so it would
                // also say yes over the resize handle at the cell's edge, which was submitted first and owns the
                // hover: only the cell that owns it shows the hand.
                if ui.is_item_hovered() && cx.hovered_id() == ui.item_id().raw() {
                    cx.set_drag_cursor(CursorShape::Grab);
                }
                if ui.is_item_activated() && self.zone_move.is_none() {
                    self.zone_move = Some(ZoneMove {
                        zone: ty,
                        active: false,
                        press_x: mouse.x,
                        grab: mouse.x - x,
                        width: w,
                    });
                }
            }
            if self.zone_move.is_some() && !ui.is_mouse_down(MouseButton::Left) {
                if self.zone_move.is_some_and(|zone_move| zone_move.active) {
                    app.settings.save(&app.config_path("settings.txt"));
                }
                self.zone_move = None;
            }
            if let Some(zone_move) = &mut self.zone_move {
                if !zone_move.active && (mouse.x - zone_move.press_x).abs() >= cx.px(4.0) {
                    zone_move.active = true;
                }
                cx.set_drag_cursor(CursorShape::Grab);
            }
            if let Some(zone_move) = self.zone_move.filter(|zone_move| zone_move.active) {
                // The floating cell's edges against the neighbours' centres: past one, the zones swap and the
                // layout is placed again for the rows below.
                let left_edge = mouse.x - zone_move.grab;
                let right_edge = left_edge + zone_move.width;
                let mut moved = false;
                for _ in 0..self.zones.zones.len() {
                    let Some(index) = self.zones.index_of(zone_move.zone) else {
                        break;
                    };
                    let placed = Zones::place(&self.zones, g.zones_x, cx.scale);
                    let centre = |ty: ZoneType| placed.x(ty) + placed.w(ty) / 2.0;
                    let mut to = index;
                    if index > 0 && left_edge < centre(self.zones.zones[index - 1].zone) {
                        to = index - 1;
                    }
                    if to == index
                        && index + 1 < self.zones.zones.len()
                        && right_edge > centre(self.zones.zones[index + 1].zone)
                    {
                        to = index + 1;
                    }
                    if to == index {
                        break;
                    }
                    if let Some(order) = self.zones.move_zone(&app.settings, index, to) {
                        app.settings.graph_column_order = order;
                        moved = true;
                    }
                }
                if moved {
                    // Placed again, with the last zone's trailing room as above.
                    g.z = Zones::place(&self.zones, g.zones_x, cx.scale);
                    g.z.extend_last(
                        &self.zones,
                        origin.x + size.x - g.scrollbar - g.gear_w,
                        cx.scale,
                    );
                    self.draw_header_cells(cx, draw, g);
                    cx.draw_fa_icon(
                        draw,
                        "far:cog",
                        vec2(origin.x + size.x - cx.px(9.0), header_y + cx.px(12.17)),
                        cx.px(12.0),
                        p.text_disabled,
                    );
                }
                // The dragged cell, over everything: the header's background and label following the mouse.
                let front = cx.foreground_draw_list();
                let fx = (mouse.x - zone_move.grab).round();
                front.add_rect_filled(
                    vec2(fx, header_y),
                    vec2(fx + zone_move.width, header_y + header_h),
                    p.panel_bg2.with_alpha(0.95),
                    cx.px(2.0),
                );
                front.add_rect(
                    vec2(fx, header_y),
                    vec2(fx + zone_move.width, header_y + header_h),
                    p.section_border,
                    cx.px(2.0),
                    1.0,
                );
                header_cell(
                    cx,
                    front,
                    (fx, zone_move.width),
                    (header_y, header_h),
                    header_label(&g.z, &g.m, zone_move.zone),
                    false,
                );
            }
        }

        if ui.is_window_hovered_with_flags(dear_imgui_rs::WindowHoveredFlags::CHILD_WINDOWS)
            && ui.is_mouse_released(MouseButton::Right)
            && Rect::new(
                vec2(origin.x, header_y),
                vec2(origin.x + size.x, header_y + header_h),
            )
            .contains(cx.mouse_pos())
        {
            let items = graph_header_menu(cx, app);
            popup_menu(cx, items, None);
        }
    }
}

/// The right edge of the shown zone `index`, in device px from `x`.
fn zone_right(layout: &ZoneLayout, x: f32, index: usize, scale: f32) -> f32 {
    let edge: f32 = layout.zones[..=index].iter().map(|zone| zone.current).sum();
    x + (scale * edge).round()
}

/// The label of a header cell: the AUTHOR one goes when the zone is too narrow for more than the avatar.
fn header_label(z: &Zones, m: &Metrics, zone: ZoneType) -> &'static str {
    if zone == ZoneType::Author && z.w(zone) <= m.px(60.0) {
        ""
    } else {
        zone.label()
    }
}

/// A graph header cell: ".fs-1 .ml1 .font-monospace" in text-disabled, a 1px section border on its left.
fn header_cell(
    cx: &Cx<'_>,
    draw: DrawList<'_>,
    (x, w): (f32, f32),
    (y, h): (f32, f32),
    label: &str,
    border: bool,
) {
    let p = cx.palette;
    if w <= 0.0 {
        return;
    }
    // Baseline 15.5px below the header's top edge, for the 10px Consolas line.
    let ascent = (cx.px(10.0) * 1884.0 / 2048.0).ceil();
    cx.draw_text_ellipsis(
        draw,
        cx.fonts.mono,
        10.0,
        vec2(x + cx.px(6.0), y + cx.px(15.5) - ascent),
        w - cx.px(10.0),
        p.text_disabled,
        label,
    );
    if border {
        // The previous zone's resize handle has the 1px border-right, just left of this cell.
        draw.add_rect_filled(
            vec2(x - cx.px(1.0), y),
            vec2(x, y + h),
            p.section_border,
            0.0,
        );
    }
}

/// The timeline bucket of a commit `age` seconds old, on format_relative's steps: 0 within the hour, then rising
/// with each hour, "yesterday", day, week, month and year it reads.
fn timeline_bucket(age: i64) -> i64 {
    const HOUR: i64 = 3600;
    const DAY: i64 = 86400;
    if age < HOUR {
        0
    } else if age < DAY {
        age / HOUR // 1..23
    } else if age < DAY * 2 {
        24 // yesterday
    } else if age < DAY * 7 {
        23 + age / DAY // 25..29
    } else if age < DAY * 30 {
        29 + age / (DAY * 7) // 30..33
    } else if age < DAY * 365 {
        33 + age / (DAY * 30) // 34..45
    } else {
        45 + (age / (DAY * 365)).min(1000)
    }
}

/// The marks along the rows' vertical scrollbar where the branches sit, at their rows' share of the list: a short
/// tick in the lane colour per local branch (half as strong for remote ones), the checked-out commit a full-width
/// bar in the accent and the selected commits a thin bar in the text colour. Call inside the rows' window, after
/// its rows.
fn draw_scroll_markers(cx: &Cx<'_>, state: &RepoState, row_h: f32, list_rows: usize) {
    // SAFETY: a window is current while its contents are built, which is where this is called; the window is only
    // read (its scrollbar flag), and the rectangle is returned by value.
    let track = unsafe {
        let window = sys::igGetCurrentWindow();
        if window.is_null() || !(*window).ScrollbarY || list_rows == 0 {
            return;
        }
        sys::igGetWindowScrollbarRect(window, sys::ImGuiAxis_Y as sys::ImGuiAxis)
    };
    let (track_min, track_max) = (
        vec2(track.Min.x, track.Min.y),
        vec2(track.Max.x, track.Max.y),
    );
    let p = cx.palette;
    let height = track_max.y - track_min.y;
    let content = (list_rows as f32 * row_h).max(height);
    let scale = height / content;
    let draw = cx.window_draw_list();
    draw.push_clip_rect(track_min, track_max, false);
    let mark_at = |row: usize, inset: f32, mark_height: f32, color| {
        let centre = track_min.y + (row as f32 + 0.5) * row_h * scale;
        let top = (centre - mark_height / 2.0)
            .clamp(track_min.y, (track_max.y - mark_height).max(track_min.y))
            .round();
        draw.add_rect_filled(
            vec2(track_min.x + inset, top),
            vec2(track_max.x - inset, top + mark_height),
            color,
            0.0,
        );
    };
    let row_of = |sha: &str| state.history.row_by_sha.get(sha).copied();
    for r in &state.history.refs {
        use imgit_git::RefType;
        if r.ref_type != RefType::LocalBranch && r.ref_type != RefType::RemoteBranch {
            continue;
        }
        let Some(row) = row_of(&r.target).filter(|&row| row < state.layout.rows.len()) else {
            continue;
        };
        let lane = p.graph_color(state.layout.rows[row].column as i32);
        let local = r.ref_type == RefType::LocalBranch;
        mark_at(
            row,
            cx.px(2.0),
            cx.px(2.0),
            lane.with_alpha(if local { 1.0 } else { 0.5 }),
        );
    }
    for sha in &state.selected {
        if let Some(row) = row_of(sha) {
            mark_at(row, 0.0, cx.px(1.0), p.text_normal);
        }
    }
    if let Some(head) = state.history.head_sha.as_deref().and_then(row_of) {
        mark_at(head, 0.0, cx.px(3.0), p.accent);
    }
    draw.pop_clip_rect();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resizing_a_graph_column_commits_the_width_on_mouse_release() {
        use crate::app::settings::Settings;
        use crate::shell::test_frame::TestFrames;

        let (_scratch, _repo, mut app, _, _) = graph_fixture();
        let mut frames = TestFrames::new();
        let mut origin = Vec2::ZERO;
        let size = vec2(1180.0, 450.0);
        let draw = |cx: &mut Cx<'_>, app: &mut Application| {
            app.with_active(|app, state| draw_graph_view(cx, app, state, size))
                .unwrap();
        };
        for _ in 0..2 {
            frames.draw(|cx| {
                origin = cx.cursor();
                draw(cx, &mut app);
            });
        }
        let layout = &app.panels.graph_view.zones;
        let index = layout.index_of(ZoneType::Ref).unwrap();
        let initial = layout.zones[index].current;
        let x = zone_right(layout, origin.x + GRAPH_LEFT_MARGIN, index, 1.0);
        let handle = vec2(x - 3.0, origin.y + GRAPH_HEADER_HEIGHT / 2.0);

        frames.move_mouse(handle.into());
        frames.draw(|cx| draw(cx, &mut app));
        frames.set_left_mouse(true);
        frames.draw(|cx| draw(cx, &mut app));
        assert_eq!(app.panels.graph_view.zone_drag.unwrap().index, index);
        assert!(app.panels.graph_view.zone_move.is_none());
        frames.move_mouse((handle + vec2(45.0, 0.0)).into());
        frames.draw(|cx| draw(cx, &mut app));
        assert!((app.panels.graph_view.zones.zones[index].current - initial - 45.0).abs() < 0.01);
        assert!(!app.settings.graph_column_widths.contains_key("ref"));

        frames.set_left_mouse(false);
        frames.draw(|cx| draw(cx, &mut app));
        assert!(app.panels.graph_view.zone_drag.is_none());
        let mut stored = Settings::default();
        stored.load(&app.config_path("settings.txt"));
        let saved = stored.graph_column_widths[ZoneType::Ref.key()];
        assert!((saved - initial - 45.0).abs() < 0.01);
        assert!((app.panels.graph_view.zones.zones[index].preferred - saved).abs() < 0.01);
    }

    #[test]
    fn dragging_the_graph_header_reorders_columns_and_saves_the_order() {
        use crate::app::settings::Settings;
        use crate::shell::test_frame::TestFrames;

        let (_scratch, _repo, mut app, _, _) = graph_fixture();
        app.settings.show_author = true;
        app.settings.show_sha = true;
        let mut frames = TestFrames::new();
        let mut origin = Vec2::ZERO;
        let size = vec2(1180.0, 470.0);
        let draw = |cx: &mut Cx<'_>, app: &mut Application| {
            app.with_active(|app, state| draw_graph_view(cx, app, state, size))
                .unwrap();
        };
        for _ in 0..2 {
            frames.draw(|cx| {
                origin = cx.cursor();
                draw(cx, &mut app);
            });
        }
        let layout = &app.panels.graph_view.zones;
        assert!(
            layout.index_of(ZoneType::Author).unwrap()
                > layout.index_of(ZoneType::Message).unwrap()
        );
        let placed = Zones::place(layout, origin.x + GRAPH_LEFT_MARGIN, 1.0);
        let y = origin.y + GRAPH_HEADER_HEIGHT / 2.0;
        let start = vec2(placed.x(ZoneType::Author) + 24.0, y);
        let target = vec2(placed.x(ZoneType::Message) + 35.0, y);
        frames.move_mouse(start.into());
        frames.draw(|cx| draw(cx, &mut app));
        frames.set_left_mouse(true);
        frames.draw(|cx| draw(cx, &mut app));
        let pending = app.panels.graph_view.zone_move.unwrap();
        assert_eq!(pending.zone, ZoneType::Author);
        assert!(!pending.active);
        frames.move_mouse(target.into());
        frames.draw(|cx| draw(cx, &mut app));
        assert!(app.panels.graph_view.zone_move.unwrap().active);
        let layout = &app.panels.graph_view.zones;
        assert!(
            layout.index_of(ZoneType::Author).unwrap()
                < layout.index_of(ZoneType::Message).unwrap()
        );

        frames.set_left_mouse(false);
        frames.draw(|cx| draw(cx, &mut app));
        assert!(app.panels.graph_view.zone_move.is_none());
        let mut stored = Settings::default();
        stored.load(&app.config_path("settings.txt"));
        assert_eq!(stored.graph_column_order, app.settings.graph_column_order);
        let author = stored
            .graph_column_order
            .iter()
            .position(|key| key == ZoneType::Author.key())
            .unwrap();
        let message = stored
            .graph_column_order
            .iter()
            .position(|key| key == ZoneType::Message.key())
            .unwrap();
        assert!(author < message);
    }

    #[test]
    fn navigating_a_long_graph_scrolls_between_its_workdir_and_oldest_commit() {
        use crate::shell::test_frame::TestFrames;
        use dear_imgui_rs::Key;

        let (_scratch, repo, mut app, first, latest) = graph_fixture();
        repo.create_branch("review", &latest, true).unwrap();
        let review = repo.commit("Review side", false).unwrap();
        let main = app.active().unwrap().history.head_branch.clone().unwrap();
        repo.checkout_branch(&main).unwrap();
        for i in 0..26 {
            repo.commit(&format!("Work item {i}"), false).unwrap();
        }
        let main_tip = repo
            .load_history(&imgit_git::HistoryOptions::default())
            .unwrap()
            .head_sha
            .unwrap();
        assert!(repo.merge("review", true).unwrap().is_done());
        std::fs::write(repo.workdir().join("draft.txt"), "Saved work").unwrap();
        let stash = repo.stash_push("Graph stash").unwrap();
        std::fs::write(repo.workdir().join("draft.txt"), "New work").unwrap();
        repo.create_tag("v1", &first, None).unwrap();
        // A remote tracking branch also gets a scroll marker.
        repo.add_remote("mirror", ".").unwrap();
        let output = imgit_git::cli::run(
            repo.workdir(),
            ["update-ref", "refs/remotes/mirror/review", &review],
            imgit_git::cli::RunOptions::default(),
        );
        assert!(output.success(), "{}", output.text);

        let state = app.active_mut().unwrap();
        state.refresh();
        assert!(state.history.commits.len() > 28);
        assert!(
            state
                .history
                .commits
                .iter()
                .any(|commit| commit.parents == [main_tip.as_str(), review.as_str()])
        );
        assert!(state.commit_for(&stash).is_some());
        assert!(state.history.row_by_sha.contains_key(imgit_app::WIP_SHA));
        let mut frames = TestFrames::new();
        let mut origin = Vec2::ZERO;
        let size = vec2(1160.0, 280.0);
        let draw = |cx: &mut Cx<'_>, app: &mut Application| {
            app.with_active(|app, state| draw_graph_view(cx, app, state, size))
                .unwrap();
        };
        for _ in 0..3 {
            frames.draw(|cx| {
                origin = cx.cursor();
                draw(cx, &mut app);
            });
        }

        frames.set_key(Key::End, true);
        frames.draw(|cx| draw(cx, &mut app));
        frames.set_key(Key::End, false);
        frames.draw(|cx| draw(cx, &mut app));
        assert_eq!(app.active().unwrap().selected, [first.as_str()]);
        assert!(app.active().unwrap().scroll_to_row.is_none());
        let bottom = origin + vec2(340.0, size.y - 20.0);
        frames.move_mouse(bottom.into());
        frames.draw(|cx| draw(cx, &mut app));
        let hovered = app.active().unwrap().hovered_sha.as_deref().unwrap();
        assert!(app.active().unwrap().history.row_by_sha[hovered] > 20);

        frames.set_key(Key::Home, true);
        frames.draw(|cx| draw(cx, &mut app));
        frames.set_key(Key::Home, false);
        frames.draw(|cx| draw(cx, &mut app));
        assert_eq!(app.active().unwrap().selected, [imgit_app::WIP_SHA]);
        let top = origin
            + vec2(
                340.0,
                GRAPH_HEADER_HEIGHT + GRAPH_HEADER_MARGIN_BOTTOM + 7.0,
            );
        frames.move_mouse(top.into());
        frames.draw(|cx| draw(cx, &mut app));
        assert_eq!(
            app.active().unwrap().hovered_sha.as_deref(),
            Some(imgit_app::WIP_SHA)
        );
    }

    #[test]
    fn shift_wheel_scrolls_a_graph_with_more_lanes_than_its_column_fits() {
        use crate::shell::test_frame::TestFrames;
        use dear_imgui_rs::Key;

        let (_scratch, repo, mut app, _, latest) = graph_fixture();
        let main = app.active().unwrap().history.head_branch.clone().unwrap();
        for i in 0..12 {
            let name = format!("topic-{i:02}");
            repo.create_branch(&name, &latest, true).unwrap();
            repo.commit(&format!("Work in {name}"), false).unwrap();
        }
        repo.checkout_branch(&main).unwrap();
        app.active_mut().unwrap().refresh();
        assert!(app.active().unwrap().layout.max_columns >= 10);
        app.settings
            .graph_column_widths
            .insert("graph".to_owned(), 95.0);

        let mut frames = TestFrames::new();
        let size = vec2(1050.0, 420.0);
        let mut origin = Vec2::ZERO;
        let draw = |cx: &mut Cx<'_>, app: &mut Application| {
            app.with_active(|app, state| draw_graph_view(cx, app, state, size))
                .unwrap();
        };
        for _ in 0..2 {
            frames.draw(|cx| {
                origin = cx.cursor();
                draw(cx, &mut app);
            });
        }
        let layout = &app.panels.graph_view.zones;
        let placed = Zones::place(layout, origin.x + GRAPH_LEFT_MARGIN, 1.0);
        let lane = vec2(
            placed.x(ZoneType::Graph) + 34.0,
            origin.y + GRAPH_HEADER_HEIGHT + GRAPH_HEADER_MARGIN_BOTTOM + 52.0,
        );
        frames.move_mouse(lane.into());
        frames.draw(|cx| draw(cx, &mut app));
        assert!((app.panels.graph_view.graph_scroll).abs() < 0.001);

        frames.set_key(Key::ModShift, true);
        frames.scroll_vertical(-1.0);
        frames.draw(|cx| draw(cx, &mut app));
        assert!(app.panels.graph_view.graph_scroll > 20.0);
        frames.scroll_vertical(10.0);
        frames.draw(|cx| draw(cx, &mut app));
        assert!((app.panels.graph_view.graph_scroll).abs() < 0.001);
    }

    #[test]
    fn a_commit_row_opens_its_context_menu_and_double_click_shows_details() {
        use crate::shell::test_frame::TestFrames;
        use crate::ui::menu::{draw_menus, menu_open};
        use dear_imgui_rs::Key;

        let (_scratch, repo, mut app, first, latest) = graph_fixture();
        let initial_branch = app.active().unwrap().history.head_branch.clone();
        app.settings.show_author = true;
        app.settings.show_date = true;
        app.settings.show_sha = true;
        app.settings.description_on_hover = true;
        app.show_detail_panel = false;
        let mut frames = TestFrames::new();
        let mut origin = Vec2::ZERO;
        let draw = |cx: &mut Cx<'_>, app: &mut Application| {
            app.with_active(|app, state| draw_graph_view(cx, app, state, vec2(1180.0, 420.0)))
                .unwrap();
            draw_menus(cx, app);
        };
        for _ in 0..2 {
            frames.draw(|cx| {
                origin = cx.cursor();
                draw(cx, &mut app);
            });
        }
        let row = app.active().unwrap().history.row_by_sha[&first];
        let m = Metrics::new(app.settings.compact_graph, 1.0);
        let zones = Zones::place(
            &app.panels.graph_view.zones,
            origin.x + GRAPH_LEFT_MARGIN,
            1.0,
        );
        let point = vec2(
            zones.x(ZoneType::Message) + 38.0,
            origin.y
                + GRAPH_HEADER_HEIGHT
                + GRAPH_HEADER_MARGIN_BOTTOM
                + (row as f32 + 0.5) * m.px(m.row_height),
        );
        frames.move_mouse(point.into());
        frames.draw(|cx| draw(cx, &mut app));
        assert_eq!(
            app.active().unwrap().hovered_sha.as_deref(),
            Some(first.as_str())
        );
        frames.set_right_mouse(true);
        frames.draw(|cx| draw(cx, &mut app));
        assert_eq!(app.active().unwrap().selected, [first.as_str()]);
        frames.set_right_mouse(false);
        frames.draw(|cx| {
            draw(cx, &mut app);
            assert!(menu_open(cx));
        });
        assert_eq!(app.panels.graph_view.context_sha, first);
        frames.set_key(Key::Escape, true);
        frames.draw(|cx| {
            draw(cx, &mut app);
            assert!(!menu_open(cx));
        });
        frames.set_key(Key::Escape, false);
        frames.draw(|cx| draw(cx, &mut app));

        for _ in 0..2 {
            frames.set_left_mouse(true);
            frames.draw(|cx| draw(cx, &mut app));
            frames.set_left_mouse(false);
            frames.draw(|cx| draw(cx, &mut app));
        }
        assert!(app.show_detail_panel);
        let state = app.active().unwrap();
        assert_eq!(state.history.head_branch, initial_branch);
        assert_eq!(state.history.head_sha.as_deref(), Some(latest.as_str()));
        assert_eq!(repo.head_commit_message().unwrap(), "Latest\n\nMore detail");
    }

    #[test]
    fn inline_graph_refs_validate_names_and_attach_to_the_selected_commit() {
        use crate::app::dialog::DialogKind;
        use crate::shell::test_frame::TestFrames;
        use dear_imgui_rs::Key;

        let (_scratch, _repo, mut app, first, latest) = graph_fixture();
        let mut frames = TestFrames::new();
        let draw = |cx: &mut Cx<'_>, app: &mut Application| {
            panels::draw_dialogs(cx, app);
            app.with_active(|app, state| draw_graph_view(cx, app, state, vec2(1180.0, 420.0)))
                .unwrap();
        };
        for _ in 0..2 {
            frames.draw(|cx| draw(cx, &mut app));
        }
        app.dialog.open(DialogKind::CreateBranch);
        app.dialog.sha.clone_from(&first);
        for _ in 0..3 {
            frames.draw(|cx| draw(cx, &mut app));
        }
        assert_eq!(app.dialog.kind, None);
        assert_eq!(app.active().unwrap().selected, [first.as_str()]);
        frames.type_text("-invalid");
        frames.draw(|cx| draw(cx, &mut app));
        frames.set_key(Key::Enter, true);
        frames.draw(|cx| draw(cx, &mut app));
        assert_eq!(
            app.active().unwrap().inline_ref.as_ref().unwrap().name,
            "-invalid"
        );
        assert!(
            app.active()
                .unwrap()
                .find_ref("refs/heads/-invalid")
                .is_none()
        );
        frames.set_key(Key::Enter, false);
        frames.draw(|cx| draw(cx, &mut app));
        frames.set_key(Key::Escape, true);
        frames.draw(|cx| draw(cx, &mut app));
        assert!(app.active().unwrap().inline_ref.is_none());
        frames.set_key(Key::Escape, false);

        for (kind, name, full) in [
            (
                DialogKind::CreateBranch,
                "topic/inline",
                "refs/heads/topic/inline",
            ),
            (DialogKind::CreateTag, "v-inline", "refs/tags/v-inline"),
        ] {
            app.dialog.open(kind);
            app.dialog.sha.clone_from(&first);
            for _ in 0..3 {
                frames.draw(|cx| draw(cx, &mut app));
            }
            frames.type_text(name);
            frames.draw(|cx| draw(cx, &mut app));
            assert_eq!(
                app.active().unwrap().inline_ref.as_ref().unwrap().name,
                name,
                "{kind:?}"
            );
            frames.set_key(Key::Enter, true);
            frames.draw(|cx| draw(cx, &mut app));
            assert!(app.active().unwrap().inline_ref.is_none(), "{kind:?}");
            let reference = app.active().unwrap().find_ref(full).unwrap();
            assert_eq!(reference.target, first);
            frames.set_key(Key::Enter, false);
            frames.draw(|cx| draw(cx, &mut app));
        }
        assert_eq!(
            app.active().unwrap().history.head_sha.as_deref(),
            Some(latest.as_str())
        );
    }

    fn graph_fixture() -> (
        tempfile::TempDir,
        imgit_git::Repository,
        Application,
        String,
        String,
    ) {
        let scratch = tempfile::tempdir().unwrap();
        let repo = imgit_git::Repository::init(scratch.path().join("graph")).unwrap();
        repo.set_local_config("user.name", Some("Graph test"))
            .unwrap();
        repo.set_local_config("user.email", Some("graph@example.invalid"))
            .unwrap();
        repo.set_local_config("tag.gpgSign", Some("false")).unwrap();
        let first = repo.commit("First\n\nInitial details", false).unwrap();
        let latest = repo.commit("Latest\n\nMore detail", false).unwrap();
        let mut app = Application::with_config_directory(scratch.path().join("config"));
        app.open_repository(&repo.workdir().to_string_lossy());
        (scratch, repo, app, first, latest)
    }

    #[test]
    fn keyboard_navigation_extends_a_range_from_the_focused_commit() {
        use crate::shell::test_frame::TestFrames;
        use dear_imgui_rs::Key;
        use imgit_git::Repository;

        let scratch = tempfile::tempdir().unwrap();
        let repo = Repository::init(scratch.path().join("repo")).unwrap();
        repo.set_local_config("user.name", Some("Graph test"))
            .unwrap();
        repo.set_local_config("user.email", Some("graph@example.invalid"))
            .unwrap();
        let oldest = repo.commit("First", false).unwrap();
        let middle = repo.commit("Second", false).unwrap();
        let newest = repo.commit("Third", false).unwrap();
        repo.create_branch("topic/example", &middle, false).unwrap();
        repo.create_tag("v1.0", &oldest, None).unwrap();
        let mut app = Application::with_config_directory(scratch.path().join("config"));
        app.open_repository(&repo.workdir().to_string_lossy());

        let mut frames = TestFrames::new();
        app.with_active(|app, state| {
            let draw = |frames: &mut TestFrames, app: &mut Application, state: &mut RepoState| {
                frames.draw(|cx| draw_graph_view(cx, app, state, vec2(1180.0, 520.0)));
            };
            for _ in 0..3 {
                draw(&mut frames, app, state);
            }

            frames.set_key(Key::End, true);
            draw(&mut frames, app, state);
            assert_eq!(state.selected, [oldest]);
            frames.set_key(Key::End, false);

            frames.set_key(Key::UpArrow, true);
            draw(&mut frames, app, state);
            assert_eq!(state.selected, [middle.as_str()]);
            frames.set_key(Key::UpArrow, false);
            draw(&mut frames, app, state);

            frames.set_key(Key::ModShift, true);
            frames.set_key(Key::UpArrow, true);
            draw(&mut frames, app, state);
            assert_eq!(state.selected, [newest.as_str(), middle.as_str()]);
            assert_eq!(state.focused.as_deref(), Some(middle.as_str()));
            assert_eq!(state.range_cursor.as_deref(), Some(newest.as_str()));
            frames.set_key(Key::UpArrow, false);
            draw(&mut frames, app, state);

            frames.set_key(Key::DownArrow, true);
            draw(&mut frames, app, state);
            assert_eq!(state.selected, [middle]);
        })
        .unwrap();
    }

    #[test]
    fn the_compact_graph_loads_more_commits_and_counts_changes_for_their_rows() {
        use imgit_git::Repository;

        use crate::shell::test_frame::TestFrames;

        let scratch = tempfile::tempdir().unwrap();
        let repo = Repository::init(scratch.path().join("repo")).unwrap();
        repo.set_local_config("user.name", Some("Graph counts test"))
            .unwrap();
        repo.set_local_config("user.email", Some("graph-counts@example.invalid"))
            .unwrap();
        std::fs::write(repo.workdir().join("edited.txt"), "Before\n").unwrap();
        std::fs::write(repo.workdir().join("deleted.txt"), "Delete me\n").unwrap();
        repo.stage_all().unwrap();
        let first = repo.commit("Original files", false).unwrap();
        std::fs::write(repo.workdir().join("edited.txt"), "After\n").unwrap();
        std::fs::remove_file(repo.workdir().join("deleted.txt")).unwrap();
        std::fs::write(repo.workdir().join("created.txt"), "Created\n").unwrap();
        repo.stage_all().unwrap();
        let head = repo.commit("Change all three", false).unwrap();
        std::fs::write(repo.workdir().join("working.txt"), "Working copy\n").unwrap();

        let mut app = Application::with_config_directory(scratch.path().join("config"));
        app.settings.compact_graph = true;
        app.settings.show_changes = true;
        app.open_repository(&repo.workdir().to_string_lossy());
        let state = app.active_mut().unwrap();
        state.commit_limit = 1;
        state.refresh();
        assert!(state.history.has_more);
        assert!(state.commit_for(&first).is_none());
        let mut frames = TestFrames::new();
        let mut frames_drawn = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            frames.draw(|cx| {
                app.with_active(|app, state| draw_graph_view(cx, app, state, vec2(1180.0, 480.0)))
                    .unwrap();
            });
            frames_drawn += 1;
            if app
                .panels
                .graph_view
                .changes
                .get(repo.workdir(), &first)
                .is_some()
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "graph counts did not settle"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(frames_drawn > 1);
        let state = app.active().unwrap();
        assert!(!state.history.has_more);
        assert!(state.commit_for(&first).is_some());
        let counts = &app.panels.graph_view.changes;
        assert_eq!(
            counts.get(repo.workdir(), &first),
            Some(&changes::FileCounts {
                added: 2,
                ..changes::FileCounts::default()
            })
        );
        assert_eq!(
            counts.get(repo.workdir(), &head),
            Some(&changes::FileCounts {
                added: 1,
                modified: 1,
                deleted: 1
            })
        );
    }

    #[test]
    fn dragging_a_branch_pill_opens_the_drop_menu_on_another_branch() {
        use dear_imgui_rs::Key;
        use imgit_git::Repository;

        use crate::shell::test_frame::TestFrames;
        use crate::ui::menu::{draw_menus, menu_open};

        let scratch = tempfile::tempdir().unwrap();
        let repo = Repository::init(scratch.path().join("repo")).unwrap();
        repo.set_local_config("user.name", Some("Graph drag test"))
            .unwrap();
        repo.set_local_config("user.email", Some("graph-drag@example.invalid"))
            .unwrap();
        let parent = repo.commit("Parent", false).unwrap();
        repo.create_branch("topic", &parent, false).unwrap();
        let newer = repo.commit("Newer", false).unwrap();
        let mut app = Application::with_config_directory(scratch.path().join("config"));
        app.open_repository(&repo.workdir().to_string_lossy());
        let mut frames = TestFrames::new();
        let size = vec2(1180.0, 460.0);
        let mut origin = Vec2::ZERO;
        for _ in 0..2 {
            frames.draw(|cx| {
                origin = cx.cursor();
                app.with_active(|app, state| draw_graph_view(cx, app, state, size))
                    .unwrap();
            });
        }
        let graph = &app.panels.graph_view;
        let zones = Zones::place(&graph.zones, origin.x + GRAPH_LEFT_MARGIN, 1.0);
        let metrics = Metrics::new(app.settings.compact_graph, 1.0);
        let row_height = metrics.px(metrics.row_height);
        let pill = |sha: &str| {
            let index = app.active().unwrap().history.row_by_sha[sha];
            vec2(
                zones.x(ZoneType::Ref) + 16.0,
                origin.y
                    + GRAPH_HEADER_HEIGHT
                    + GRAPH_HEADER_MARGIN_BOTTOM
                    + row_height * (index as f32 + 0.5),
            )
        };
        let source = pill(&parent);
        let target = pill(&newer);
        let draw = |frames: &mut TestFrames, app: &mut Application| {
            let mut open = false;
            frames.draw(|cx| {
                app.with_active(|app, state| draw_graph_view(cx, app, state, size))
                    .unwrap();
                draw_menus(cx, app);
                open = menu_open(cx);
            });
            open
        };
        frames.move_mouse(source.into());
        draw(&mut frames, &mut app);
        draw(&mut frames, &mut app);
        frames.set_left_mouse(true);
        draw(&mut frames, &mut app);
        assert!(app.panels.graph_view.refs.drag.pressed);
        frames.move_mouse(target.into());
        assert!(!draw(&mut frames, &mut app));
        assert!(app.panels.graph_view.refs.drag.active);
        let drop_target = app.panels.graph_view.refs.drop_target.as_ref().unwrap();
        assert_eq!(drop_target.target, newer);

        frames.set_left_mouse(false);
        assert!(draw(&mut frames, &mut app));
        assert!(!app.panels.graph_view.refs.drag.active);
        frames.set_key(Key::Escape, true);
        assert!(!draw(&mut frames, &mut app));
        let state = app.active().unwrap();
        assert_eq!(state.history.head_sha.as_deref(), Some(newer.as_str()));
        assert_eq!(state.find_ref("refs/heads/topic").unwrap().target, parent);
    }

    #[test]
    fn timeline_buckets_step_with_the_age() {
        const HOUR: i64 = 3600;
        const DAY: i64 = 86400;
        assert_eq!(timeline_bucket(0), 0);
        assert_eq!(timeline_bucket(HOUR - 1), 0);
        assert_eq!(timeline_bucket(HOUR), 1);
        assert_eq!(timeline_bucket(DAY - 1), 23);
        assert_eq!(timeline_bucket(DAY), 24);
        assert_eq!(timeline_bucket(2 * DAY - 1), 24);
        assert_eq!(timeline_bucket(2 * DAY), 25);
        assert_eq!(timeline_bucket(6 * DAY), 29);
        assert_eq!(timeline_bucket(7 * DAY), 30);
        assert_eq!(timeline_bucket(29 * DAY), 33);
        assert_eq!(timeline_bucket(30 * DAY), 34);
        assert_eq!(timeline_bucket(364 * DAY), 45);
        assert_eq!(timeline_bucket(365 * DAY), 46);
        assert_eq!(timeline_bucket(i64::MAX / 2), 45 + 1000);
        // Buckets never fall as the age grows.
        let mut last = 0;
        for age in (0..400 * DAY).step_by(3600) {
            let bucket = timeline_bucket(age);
            assert!(bucket >= last);
            last = bucket;
        }
    }

    #[test]
    fn zone_edges_accumulate_the_widths() {
        let layout = ZoneLayout {
            zones: [(ZoneType::Ref, 100.0), (ZoneType::Graph, 50.5)]
                .iter()
                .map(|&(zone, current)| zones::GraphZone {
                    zone,
                    minimum: 0.0,
                    maximum: 0.0,
                    current,
                    preferred: current,
                })
                .collect(),
            width: 150.5,
            content: 0.0,
        };
        assert_eq!(zone_right(&layout, 10.0, 0, 1.5), 160.0);
        assert_eq!(
            zone_right(&layout, 10.0, 1, 1.5),
            10.0 + (1.5f32 * 150.5).round()
        );
    }

    #[test]
    fn narrow_author_zones_lose_their_label() {
        let m = Metrics::rich(1.0);
        let mut z = Zones::default();
        z.rect_mut(ZoneType::Author).w = 60.0;
        assert_eq!(header_label(&z, &m, ZoneType::Author), "");
        z.rect_mut(ZoneType::Author).w = 61.0;
        assert_eq!(header_label(&z, &m, ZoneType::Author), "AUTHOR");
        assert_eq!(header_label(&z, &m, ZoneType::Sha), "SHA");
    }
}
