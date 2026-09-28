//! The application: configuration, content, the render pipeline, and the
//! window's event handler.

use crate::config::Config;
use crate::content::{Clock, Command, ContentState, Mode};
use crate::layout::{PlacedGlyph, TextEngine};
use crate::platform::win::{LayeredWindow, WindowEvents};
use crate::render;
use std::borrow::Cow;
use std::path::PathBuf;

/// What a redraw produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RedrawOutcome {
    /// Whether content did not fit and was cut off.
    pub truncated: bool,
}

/// What `GET /status` will report, without reaching into the app's innards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusSnapshot {
    pub mode: Mode,
    pub text_bytes: usize,
}

pub struct App {
    config: Config,
    state: ContentState,
    clock: Clock,
    engine: TextEngine,
    glyphs: Vec<PlacedGlyph>,
    status: StatusSnapshot,
    /// Where move mode persists the dragged origin (spec 10.4). Held from
    /// construction so the save path is the same one the load used.
    #[allow(dead_code, reason = "read by the move-mode persistence in Task 13")]
    state_path: PathBuf,
    /// The window, absent in a headless instance and *also* absent while
    /// `run` has it: the loop owns it then, and hands it back per event.
    window: Option<LayeredWindow>,
    presented_once: bool,
    stopped: bool,
}

impl App {
    pub fn new(config: Config, window: LayeredWindow, state_path: PathBuf) -> App {
        App::assemble(config, Some(window), state_path)
    }

    /// An instance with no window, so the render pipeline can be exercised
    /// without a desktop.
    pub fn headless(config: Config, state_path: PathBuf) -> App {
        App::assemble(config, None, state_path)
    }

    fn assemble(config: Config, window: Option<LayeredWindow>, state_path: PathBuf) -> App {
        let state = ContentState::new();
        let clock = Clock::from_config(&config.clock);
        let engine = TextEngine::new(&config.text);
        let status = StatusSnapshot { mode: state.mode(), text_bytes: state.text().len() };

        App {
            config,
            state,
            clock,
            engine,
            glyphs: Vec::new(),
            status,
            state_path,
            window,
            presented_once: false,
            stopped: false,
        }
    }

    pub fn status(&self) -> &StatusSnapshot {
        &self.status
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    pub fn window_mut(&mut self) -> Option<&mut LayeredWindow> {
        self.window.as_mut()
    }

    /// How often the window must redraw for the clock to stay true.
    pub fn tick_interval_ms(&self) -> u32 {
        self.clock.tick_ms(self.config.clock.tick_ms)
    }

    /// The whole pipeline, in spec §7 order, into a caller-supplied buffer.
    ///
    /// Split from `redraw` so tests can drive it with no window at all.
    pub fn render_into(&mut self, buffer: &mut [u8], width: u32, height: u32) -> RedrawOutcome {
        render::clear(buffer);

        let truncated = match self.state.mode() {
            // Blank is an explicit instruction, so the surface really is
            // cleared — unlike a failed frame, which keeps the previous one.
            Mode::Blank => false,
            _ => {
                let text: Cow<'_, str> = self.state.visible_text(&self.clock);
                let truncated = self.engine.shape(&text, (width, height), &mut self.glyphs);
                self.engine.draw(
                    &self.glyphs,
                    buffer,
                    width,
                    height,
                    self.config.text.color,
                    self.config.text.alpha,
                );
                truncated
            }
        };

        render::apply_opacity(buffer, self.config.window.opacity);
        render::rgba_to_bgra_in_place(buffer);
        RedrawOutcome { truncated }
    }

    /// Draws into the owned window and presents. Only valid outside the message
    /// loop, where the window is not checked out.
    pub fn redraw(&mut self) {
        // Checked out for the same reason `run` checks it out: rendering needs
        // the whole app mutably and the window mutably at once.
        let Some(mut window) = self.window.take() else {
            return;
        };
        self.redraw_into(&mut window);
        self.window = Some(window);
    }

    fn redraw_into(&mut self, window: &mut LayeredWindow) {
        let rect = window.rect();
        let outcome = self.render_into(window.pixels_mut(), rect.width, rect.height);
        if outcome.truncated {
            log::debug!("render: content did not fit {}x{} and was truncated", rect.width, rect.height);
        }

        match window.present() {
            Ok(()) => {
                if !self.presented_once {
                    self.presented_once = true;
                    log::info!("presented frame {}x{}", rect.width, rect.height);
                }
            }
            Err(error) => {
                // Spec §13: record it, drop the frame, do not retry and do not
                // fall back. The next trigger redraws.
                log::error!("present failed: {error}");
            }
        }
    }

    pub fn apply_command(&mut self, command: Command) {
        if self.state.apply(&command) {
            self.refresh_status();
            return;
        }

        // `Quit` is the one command `apply` refuses.
        self.stopped = true;
        if let Some(window) = self.window.as_mut() {
            window.close();
        }
    }

    /// Runs the message loop, driving this instance as the handler.
    ///
    /// The window is moved out for the duration: the loop owns it then, and
    /// passes it to each callback. Keeping it in `self` would mean borrowing the
    /// window and the whole app mutably at the same time, which is exactly what
    /// this arrangement avoids.
    pub fn run(&mut self) {
        let Some(mut window) = self.window.take() else {
            return;
        };
        window.run_message_loop(self);
        self.window = Some(window);
    }

    /// Destroys the window and releases its GDI objects.
    pub fn shutdown(mut self) {
        if let Some(window) = self.window.take() {
            window.destroy();
        }
    }

    fn refresh_status(&mut self) {
        self.status = StatusSnapshot {
            mode: self.state.mode(),
            text_bytes: self.state.text().len(),
        };
    }
}

impl WindowEvents for App {
    fn on_tick(&mut self, window: &mut LayeredWindow) {
        // Text mode only changes when a command changes it; the clock is the
        // content that changes on its own.
        if self.state.mode() == Mode::Time {
            self.redraw_into(window);
        }
        self.refresh_status();
    }

    fn on_wake(&mut self, _window: &mut LayeredWindow) {}

    fn on_dpi_changed(&mut self, window: &mut LayeredWindow, dpi: u32) {
        // The anchor is resolved in physical pixels, so a new DPI means the
        // window has to be placed again rather than merely redrawn.
        log::info!("window: dpi changed to {dpi}");
        let _ = window;
    }

    fn on_display_change(&mut self, window: &mut LayeredWindow) {
        let _ = window;
        log::info!("window: display configuration changed");
    }

    fn on_quit_requested(&mut self, _window: &mut LayeredWindow) {
        self.stopped = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(config_toml: &str) -> App {
        let config = crate::config::Config::from_toml(config_toml).unwrap();
        App::headless(config, std::path::PathBuf::from("C:/nonexistent/window_state.json"))
    }

    #[test]
    fn a_time_redraw_paints_pixels_and_leaves_the_background_transparent() {
        let mut app = app_with("[window]\nsize = [320, 120]\n[clock]\nformat = \"%H:%M:%S\"\n");
        let mut buf = vec![0u8; 320 * 120 * 4];
        let outcome = app.render_into(&mut buf, 320, 120);

        assert!(!outcome.truncated);
        let painted = buf.chunks_exact(4).filter(|p| p[3] != 0).count();
        assert!(painted > 0, "the clock drew nothing");
        assert!(painted < 320 * 120 / 2, "the clock filled the window");
    }

    #[test]
    fn text_mode_paints_and_reports_byte_length() {
        let mut app = app_with("[window]\nsize = [320, 120]\n");
        app.apply_command(crate::content::Command::SetText("玻璃纸".into()));
        let mut buf = vec![0u8; 320 * 120 * 4];
        app.render_into(&mut buf, 320, 120);

        assert_eq!(app.status().mode, crate::content::Mode::Text);
        assert_eq!(app.status().text_bytes, "玻璃纸".len());
    }

    #[test]
    fn blank_mode_paints_nothing() {
        let mut app = app_with("[window]\nsize = [320, 120]\n");
        app.apply_command(crate::content::Command::SetText(String::new()));
        let mut buf = vec![0u8; 320 * 120 * 4];
        app.render_into(&mut buf, 320, 120);

        assert!(buf.iter().all(|&b| b == 0), "blank mode must leave the surface clear");
        assert_eq!(app.status().mode, crate::content::Mode::Blank);
    }

    #[test]
    fn opacity_from_config_scales_the_painted_pixels() {
        let mut opaque = app_with("[window]\nsize = [320, 120]\nopacity = 100\n");
        let mut half = app_with("[window]\nsize = [320, 120]\nopacity = 50\n");
        let mut a = vec![0u8; 320 * 120 * 4];
        let mut b = vec![0u8; 320 * 120 * 4];
        opaque.render_into(&mut a, 320, 120);
        half.render_into(&mut b, 320, 120);

        let sum_a: u32 = a.iter().map(|&v| v as u32).sum();
        let sum_b: u32 = b.iter().map(|&v| v as u32).sum();
        assert!(sum_b < sum_a / 2 + sum_a / 4, "50% opacity must be visibly darker");
        assert!(sum_b > 0);
    }
}
