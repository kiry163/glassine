//! The application: configuration, content, the render pipeline, and the
//! window's event handler.

use crate::config::Config;
use crate::content::{Clock, Command, ContentState, Mode};
use crate::geometry::Rect;
use crate::layout::{PlacedGlyph, TextEngine};
use crate::platform::win::tray::{self, Tray, TrayAction, TrayEvent};
use crate::platform::win::{LayeredWindow, PlatformError, WindowEvents};
use crate::render;
use crate::status::StatusSnapshot;
use crate::window_state::{self, PositionSource, SavedPosition};
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// What a redraw produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RedrawOutcome {
    /// Whether content did not fit and was cut off.
    pub truncated: bool,
}

/// What the window last reported about itself.
///
/// Cached rather than asked for on demand: while the message loop runs the
/// window is checked out of [`App`], and spec 9.2 requires `/status` to be
/// answerable without involving the window thread at all.
struct WindowFacts {
    rect: Rect,
    monitor_name: String,
    dpi: u32,
}

impl WindowFacts {
    fn of(window: &LayeredWindow) -> WindowFacts {
        WindowFacts {
            rect: window.rect(),
            monitor_name: window.monitor_name(),
            dpi: window.dpi(),
        }
    }

    /// With no window there is nothing to measure: the configured size at the
    /// origin is the only honest answer, and no monitor to name.
    fn headless(config: &Config) -> WindowFacts {
        WindowFacts {
            rect: Rect { x: 0, y: 0, width: config.window.size.0, height: config.window.size.1 },
            monitor_name: String::new(),
            dpi: 0,
        }
    }
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
    /// Whether the last layout truncated, kept so `/status` can report it
    /// without replaying the render.
    last_truncated: bool,
    /// Which authority placed the window, on the snapshot's advice.
    position_source: PositionSource,
    /// The queue the HTTP thread fills and `on_wake` drains, in order.
    commands: Receiver<Command>,
    /// The sending half, handed to the server.
    command_sender: Sender<Command>,
    /// The snapshot `/status` reads, published on every refresh so the HTTP
    /// thread never has to ask the window thread anything.
    shared_status: Arc<Mutex<StatusSnapshot>>,
    /// What the window last said about itself.
    window_facts: WindowFacts,
    /// The notification-area icon, once it has been installed.
    tray: Option<Tray>,
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
        let window_facts = match window.as_ref() {
            Some(window) => WindowFacts::of(window),
            None => WindowFacts::headless(&config),
        };
        let status = snapshot_of(&state, &window_facts, PositionSource::Config, false);
        let (command_sender, commands) = mpsc::channel();
        let shared_status = Arc::new(Mutex::new(status.clone()));

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
            last_truncated: false,
            position_source: PositionSource::Config,
            commands,
            command_sender,
            shared_status,
            window_facts,
            tray: None,
        }
    }

    /// Puts the icon in the notification area.
    ///
    /// A failure is the caller's to report and survive: without the icon the
    /// window and the HTTP interface both still work (spec §13).
    pub fn install_tray(&mut self, tooltip: &str) -> Result<(), PlatformError> {
        let Some(window) = self.window.as_ref() else {
            return Ok(());
        };
        self.tray = Some(Tray::install(window.tray_window(), tooltip)?);
        Ok(())
    }

    /// The sending half of the command queue, for the HTTP server.
    pub fn command_sender(&self) -> Sender<Command> {
        self.command_sender.clone()
    }

    /// The snapshot the HTTP thread reads.
    pub fn shared_status(&self) -> Arc<Mutex<StatusSnapshot>> {
        Arc::clone(&self.shared_status)
    }

    /// Drains the queue the HTTP thread fills, applying commands in arrival
    /// order (spec 9.4: a later caller wins).
    ///
    /// The window arrives as a parameter because the message loop owns it while
    /// it runs — `self.window` is `None` for exactly that stretch — and `Quit`
    /// has to close it. Reading the window off `self` here silently does nothing,
    /// which is what a `/quit` that returns 200 and leaves the process running
    /// looks like.
    pub fn apply_pending_commands(&mut self, window: &mut LayeredWindow) {
        while let Ok(command) = self.commands.try_recv() {
            if !self.apply_command(command) {
                window.close();
                return;
            }
        }
    }

    /// Records which authority placed the window, so `/status` can explain why
    /// it is where it is.
    pub fn set_position_source(&mut self, source: PositionSource) {
        self.position_source = source;
        self.refresh_status();
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
        self.last_truncated = truncated;
        self.refresh_status();
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
        // The window is only reachable here, so this is where its facts are
        // collected for `/status`.
        self.window_facts = WindowFacts::of(window);
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

    /// Applies one command, and reports whether it was a content command.
    ///
    /// `false` means `Quit`: the content state machine refuses it, and carrying
    /// it out needs a window — which the message loop holds, not this struct.
    pub fn apply_command(&mut self, command: Command) -> bool {
        if self.state.apply(&command) {
            self.refresh_status();
            return true;
        }

        // `Quit` is the one command `apply` refuses.
        self.stopped = true;
        false
    }

    /// Enters move mode (spec §7.2): the tray's first item.
    pub fn on_move_mode_begin(&mut self, window: &mut LayeredWindow) {
        window.begin_move_mode();
    }

    /// Ends move mode, remembering where the drag left the window — unless it did
    /// not move at all, in which case the configured anchor keeps applying.
    pub fn on_move_mode_end(&mut self, window: &mut LayeredWindow, position: Option<(i32, i32)>) {
        window.end_move_mode();

        let Some((x, y)) = position else {
            return;
        };
        match window_state::save(&self.state_path, SavedPosition { x, y }) {
            Ok(()) => {
                log::info!("window_state: saved {x},{y} to {}", self.state_path.display())
            }
            // Spec §13: the window has already moved, and a failed write must not
            // undo a move the user made. Record it and keep the new position.
            Err(error) => log::warn!(
                "window_state: cannot write {}: {error}",
                self.state_path.display()
            ),
        }
        self.position_source = PositionSource::Override;
        self.refresh_status();
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
        let status = snapshot_of(
            &self.state,
            &self.window_facts,
            self.position_source,
            self.last_truncated,
        );
        self.status = status;
        publish(&self.shared_status, &self.status);
    }
}

/// Hands the snapshot to the HTTP thread, recovering from a poisoned lock: the
/// snapshot is plain data, so poisoning says nothing about its validity.
fn publish(shared: &Arc<Mutex<StatusSnapshot>>, snapshot: &StatusSnapshot) {
    match shared.lock() {
        Ok(mut guard) => *guard = snapshot.clone(),
        Err(poisoned) => *poisoned.into_inner() = snapshot.clone(),
    }
}

/// Builds the snapshot from the pieces, so construction and every later refresh
/// cannot drift apart.
fn snapshot_of(
    state: &ContentState,
    facts: &WindowFacts,
    position_source: PositionSource,
    last_truncated: bool,
) -> StatusSnapshot {
    StatusSnapshot {
        mode: state.mode(),
        window: facts.rect,
        monitor_name: facts.monitor_name.clone(),
        dpi: facts.dpi,
        // The spec pins this to `false` outside text mode: a truncated clock
        // line is not caller content going missing.
        truncated: state.mode() == Mode::Text && last_truncated,
        // `ContentState` clears the text whenever it leaves text mode, so this
        // is 0 in time and blank mode by construction.
        text_bytes: state.text().len(),
        position_source,
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

    fn on_wake(&mut self, window: &mut LayeredWindow) {
        self.apply_pending_commands(window);
        // A wake means work arrived, so redraw now rather than at the next tick:
        // that immediacy is the whole point of the wake-up path.
        if !self.stopped {
            self.redraw_into(window);
        }
    }

    fn on_dpi_changed(&mut self, window: &mut LayeredWindow, dpi: u32) {
        // The anchor is resolved in physical pixels, so a new DPI means the
        // window has to be placed again rather than merely redrawn.
        log::info!("window: dpi changed to {dpi}");
        self.window_facts = WindowFacts::of(window);
        self.refresh_status();
    }

    fn on_display_change(&mut self, window: &mut LayeredWindow) {
        log::info!("window: display configuration changed");
        self.window_facts = WindowFacts::of(window);
        self.refresh_status();
    }

    fn on_quit_requested(&mut self, _window: &mut LayeredWindow) {
        self.stopped = true;
        // Every exit path takes the icon with it, or it lingers as a ghost until
        // the mouse passes over it (spec §11).
        if let Some(tray) = self.tray.as_mut() {
            tray.remove();
        }
    }

    fn on_drag_finished(&mut self, window: &mut LayeredWindow, position: Option<(i32, i32)>) {
        self.on_move_mode_end(window, position);
    }

    fn on_tray(&mut self, window: &mut LayeredWindow, action: TrayAction) {
        let Some(tray) = self.tray.as_mut() else {
            return;
        };
        let Some(event) = tray.handle_message(action) else {
            return;
        };
        log::info!("tray: menu chose {event:?}");

        match event {
            TrayEvent::MoveWindow => self.on_move_mode_begin(window),
            TrayEvent::OpenConfig => {
                let path = Config::config_path();
                match tray::open_config(&path) {
                    Ok(()) => log::info!("tray: opened {}", path.display()),
                    Err(error) => {
                        log::warn!("tray: cannot open {}: {error}", path.display());
                        // Spec §13 asks for the failure to reach the user through
                        // the balloon the tray already has, rather than a new GUI
                        // element.
                        tray.notify("glassine", &format!("无法打开配置文件：{error}"));
                    }
                }
            }
            TrayEvent::Quit => {
                log::info!("tray: quit requested");
                tray.remove();
                window.close();
            }
        }
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
        assert!(app.apply_command(crate::content::Command::SetText("玻璃纸".into())));
        let mut buf = vec![0u8; 320 * 120 * 4];
        app.render_into(&mut buf, 320, 120);

        assert_eq!(app.status().mode, crate::content::Mode::Text);
        assert_eq!(app.status().text_bytes, "玻璃纸".len());
    }

    #[test]
    fn blank_mode_paints_nothing() {
        let mut app = app_with("[window]\nsize = [320, 120]\n");
        assert!(app.apply_command(crate::content::Command::SetText(String::new())));
        let mut buf = vec![0u8; 320 * 120 * 4];
        app.render_into(&mut buf, 320, 120);

        assert!(buf.iter().all(|&b| b == 0), "blank mode must leave the surface clear");
        assert_eq!(app.status().mode, crate::content::Mode::Blank);
    }

    #[test]
    fn quit_is_not_a_content_command_and_stops_the_app() {
        let mut app = app_with("[window]\nsize = [320, 120]\n");

        // The return value is the contract with the message loop: `false` means
        // the caller has to close the window, because only the loop holds one
        // while it runs.
        assert!(!app.apply_command(crate::content::Command::Quit));
        assert!(app.is_stopped());
        // Quitting changes nothing about the content on the way out.
        assert_eq!(app.status().mode, crate::content::Mode::Time);
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
