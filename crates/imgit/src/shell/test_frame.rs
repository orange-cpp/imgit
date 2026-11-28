//! Exercises real widget draw lists without a desktop window or an OpenGL context.

use std::sync::{MutexGuard, PoisonError};

use dear_imgui_rs::{Key, MouseButton, TextureId};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};

use super::{
    Context, DevOptions, FrameRequests, FrameWindow, ShellContext, Textures, UserAttentionType,
};
use crate::fonts::Fonts;
use crate::gfx;
use crate::theme::Themes;
use crate::ui::{Cx, Rect, UiState, Vec2};

/// Reads an ImGui window's geometry after it has been drawn in this frame.
pub(crate) fn window_rect(_cx: &Cx<'_>, name: &std::ffi::CStr) -> Rect {
    // SAFETY: the caller holds the active frame. Copy the geometry without
    // retaining pointers to ImGui's window.
    let (position, size) = unsafe {
        let window = dear_imgui_rs::sys::igFindWindowByName(name.as_ptr());
        assert!(!window.is_null(), "window {name:?} was not drawn");
        ((*window).Pos, (*window).Size)
    };
    Rect::from_pos_size(position.into(), size.into())
}

struct TestWindow;

impl HasWindowHandle for TestWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Err(HandleError::Unavailable)
    }
}

impl HasDisplayHandle for TestWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Err(HandleError::Unavailable)
    }
}

impl FrameWindow for TestWindow {
    fn has_focus(&self) -> bool {
        true
    }

    fn request_user_attention(&self, _: Option<UserAttentionType>) {}
}

#[derive(Default)]
struct TestTextures {
    uploads: Vec<Vec<u8>>,
}

impl Textures for TestTextures {
    fn create_texture(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<TextureId, gfx::Error> {
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        self.uploads.push(rgba.to_vec());
        Ok(TextureId::new(self.uploads.len() as u64))
    }

    fn destroy_texture(&mut self, _: TextureId) {}
}

pub(crate) struct TestFrames {
    context: Context,
    fonts: Fonts,
    options: DevOptions,
    state: UiState,
    themes: Themes,
    textures: TestTextures,
    index: u32,
    // Keep the context serialized until it has been dropped.
    _guard: MutexGuard<'static, ()>,
}

impl TestFrames {
    pub(crate) fn new() -> Self {
        Self::with_options(DevOptions::default())
    }

    pub(crate) fn with_options(options: DevOptions) -> Self {
        let guard = crate::IMGUI_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut context = Context::create();
        context.set_ini_filename(None::<String>).unwrap();
        context.io_mut().set_display_size([1400.0, 800.0]);
        context.io_mut().set_delta_time(0.1);
        let fonts = Fonts::for_test(&context);
        context
            .font_atlas()
            .try_claim_legacy_renderer()
            .unwrap()
            .build();

        Self {
            context,
            fonts,
            options,
            state: UiState::new(),
            themes: Themes::new(),
            textures: TestTextures::default(),
            index: 0,
            _guard: guard,
        }
    }

    pub(crate) fn draw(&mut self, draw: impl FnOnce(&mut Cx<'_>)) -> bool {
        let mut requests = FrameRequests::default();
        let mut shell = ShellContext {
            fonts: &self.fonts,
            textures: &mut self.textures,
            window: &TestWindow,
            options: &self.options,
            frame_index: self.index,
            title_bar_inset: 0.0,
            requests: &mut requests,
        };
        let ui = self.context.frame();
        self.state.begin_frame(ui, &mut self.themes, &mut shell);
        ui.window("Test")
            .position([0.0, 0.0], dear_imgui_rs::Condition::Always)
            .size([1400.0, 800.0], dear_imgui_rs::Condition::Always)
            .build(|| {
                let mut cx = Cx::new(ui, &mut shell, self.themes.palette(), &mut self.state);
                draw(&mut cx);
            });
        drop(self.context.render_legacy());
        self.index += 1;
        requests.animating
    }

    pub(crate) fn move_mouse(&mut self, position: [f32; 2]) {
        self.context.io_mut().add_mouse_pos_event(position);
    }

    pub(crate) fn set_left_mouse(&mut self, down: bool) {
        self.context
            .io_mut()
            .add_mouse_button_event(MouseButton::Left, down);
    }

    pub(crate) fn set_right_mouse(&mut self, down: bool) {
        self.context
            .io_mut()
            .add_mouse_button_event(MouseButton::Right, down);
    }

    pub(crate) fn set_key(&mut self, key: Key, down: bool) {
        self.context.io_mut().add_key_event(key, down);
    }

    pub(crate) fn scroll_vertical(&mut self, notches: f32) {
        self.context.io_mut().add_mouse_wheel_event([0.0, notches]);
    }

    pub(crate) fn type_text(&mut self, text: &str) {
        self.context.io_mut().add_input_characters_utf8(text);
    }

    pub(crate) fn click_left(&mut self, position: Vec2, mut draw: impl FnMut(&mut Cx<'_>)) {
        self.move_mouse(position.into());
        self.draw(&mut draw);
        self.set_left_mouse(true);
        self.draw(&mut draw);
        self.set_left_mouse(false);
        self.draw(draw);
    }

    pub(crate) fn uploads(&self) -> &[Vec<u8>] {
        &self.textures.uploads
    }
}
