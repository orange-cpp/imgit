//! Hidden-window tests for the desktop shell and its OpenGL renderer.

use std::cell::Cell;

use dear_imgui_rs::TextureId;
use winit::platform::windows::EventLoopBuilderExtWindows;

use super::*;
use crate::theme::{Color, Themes};
use crate::ui::{Cx, UiState, vec2};

struct Probe {
    themes: Themes,
    ui_state: UiState,
    texture: TextureId,
    draws: u32,
    commands: Vec<DevCommand>,
    capture_checks: Cell<u32>,
}

impl Probe {
    fn new(shell: &mut ShellContext<'_>) -> Self {
        shell.set_background([0.0, 0.0, 1.0, 1.0]);
        assert!(matches!(
            shell.textures.create_texture(1, 1, &[1, 2, 3]),
            Err(gfx::Error::InvalidTexture { .. })
        ));
        let texture = shell
            .textures
            .create_texture(1, 1, &[240, 32, 16, 255])
            .unwrap();
        let mut ui_state = UiState::new();
        ui_state.avatars.set_enabled(false);
        Self {
            themes: Themes::new(),
            ui_state,
            texture,
            draws: 0,
            commands: Vec::new(),
            capture_checks: Cell::new(0),
        }
    }
}

impl Frame for Probe {
    fn draw(&mut self, ui: &Ui, shell: &mut ShellContext<'_>) {
        self.draws += 1;
        self.ui_state.begin_frame(ui, &mut self.themes, shell);
        let mut cx = Cx::new(ui, shell, self.themes.palette(), &mut self.ui_state);
        cx.shell.set_background([0.0, 0.0, 1.0, 1.0]);
        if cx.shell.options.screenshot.is_none() && cx.shell.frame_index == 2 {
            cx.shell.quit();
        }
        if cx.shell.frame_index == 1 {
            let temporary = cx.shell.textures.create_texture(1, 1, &[0; 4]).unwrap();
            cx.shell.textures.destroy_texture(temporary);
            cx.shell.textures.destroy_texture(temporary);
            cx.shell.set_cursor(CursorShape::Move);
        }
        ui.window("Glyphs")
            .position([160.0, 100.0], dear_imgui_rs::Condition::Always)
            .size([220.0, 120.0], dear_imgui_rs::Condition::Always)
            .build(|| {
                ui.text("LCD text");
                cx.window_draw_list().set_grayscale_text(true);
                ui.text("Grayscale text");
            });
        let draw = cx.foreground_draw_list();
        draw.add_image(
            self.texture,
            vec2(40.0, 40.0),
            vec2(80.0, 80.0),
            vec2(0.0, 0.0),
            vec2(1.0, 1.0),
            Color::WHITE,
        );
        draw.push_clip_rect(vec2(92.0, 40.0), vec2(110.0, 80.0), true);
        draw.add_image(
            self.texture,
            vec2(90.0, 40.0),
            vec2(120.0, 80.0),
            vec2(0.0, 0.0),
            vec2(1.0, 1.0),
            Color::WHITE,
        );
        draw.pop_clip_rect();
        let size = if cx.shell.frame_index < 2 { 15.0 } else { 24.0 };
        cx.draw_text(
            draw,
            cx.fonts.regular,
            size,
            vec2(135.0, 44.0),
            Color::WHITE,
            "Rendered text",
        );
    }

    fn dev_command(&mut self, command: DevCommand, _shell: &mut ShellContext<'_>) {
        self.commands.push(command);
    }

    fn busy(&self) -> bool {
        let checks = self.capture_checks.get();
        self.capture_checks.set(checks + 1);
        checks == 0
    }
}

struct HiddenCapture {
    config: Config,
    result: Option<anyhow::Result<()>>,
}

impl HiddenCapture {
    fn render(&self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let mut state = State::new(event_loop, &self.config, Probe::new)?;
        let result = (|| {
            anyhow::ensure!(
                state.renderer.supports_lcd_text(),
                "LCD renderer did not attach"
            );
            let size = state.window.inner_size();
            state.resize_surface(PhysicalSize::new(0, 0));
            state.resize_surface(size);
            let mut finished = false;
            for _ in 0..20 {
                if !state.redraw(&self.config.options)? {
                    finished = true;
                    break;
                }
            }
            anyhow::ensure!(finished, "capture did not stop the frame loop");
            assert_eq!(state.frame.capture_checks.get(), 2);
            assert_eq!(
                state.frame.commands,
                [DevCommand::SelectRow(7), DevCommand::SelectRow(4)]
            );
            assert!(state.frame.draws > SCREENSHOT_FRAME);

            let capture = self.config.options.screenshot.as_ref().unwrap();
            for path in [
                capture.clone(),
                self.config.options.shots_dir.join("script.bmp"),
            ] {
                let bitmap = image::open(path)?.into_rgb8();
                assert_eq!(bitmap.dimensions(), (size.width, size.height));
                assert_eq!(bitmap.get_pixel(60, 60).0, [240, 32, 16]);
                assert_eq!(bitmap.get_pixel(100, 60).0, [240, 32, 16]);
                assert_eq!(bitmap.get_pixel(115, 60).0, [0, 0, 255]);
                assert!(
                    (44..85).any(|y| (135..350).any(|x| bitmap.get_pixel(x, y).0[0] > 0)),
                    "the font atlas did not draw any glyphs"
                );
                assert_eq!(
                    bitmap.get_pixel(size.width - 2, size.height - 2).0,
                    [0, 0, 255]
                );
            }
            Ok(())
        })();
        state.shutdown();
        result?;

        // Without a script, the window backend prepares and presents the frames.
        let config = Config::default();
        let mut state = State::new(event_loop, &config, Probe::new)?;
        let result = (|| {
            assert!(state.redraw(&config.options)?);
            assert!(state.redraw(&config.options)?);
            assert!(!state.redraw(&config.options)?);
            Ok(())
        })();
        state.shutdown();
        result
    }
}

impl ApplicationHandler for HiddenCapture {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.result = Some(self.render(event_loop));
        event_loop.exit();
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _event: WindowEvent,
    ) {
    }
}

#[test]
#[ignore = "requires a Windows desktop OpenGL 3.3 driver"]
fn hidden_shell_renders_the_lcd_atlas_and_images_and_captures_scripted_frames() {
    let _guard = crate::IMGUI_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let scratch = tempfile::tempdir().unwrap();
    let script = scratch.path().join("steps.txt");
    std::fs::write(&script, "row 4\nwait 2\nshot script\nwait 20\n").unwrap();
    let mut app = HiddenCapture {
        config: Config {
            options: DevOptions {
                script: Some(script),
                screenshot: Some(scratch.path().join("capture.bmp")),
                select_row: Some(7),
                shots_dir: scratch.path().to_owned(),
                ..DevOptions::default()
            },
            ..Config::default()
        },
        result: None,
    };
    EventLoop::builder()
        .with_any_thread(true)
        .build()
        .unwrap()
        .run_app(&mut app)
        .unwrap();
    app.result
        .expect("the hidden window did not start")
        .unwrap();
}
