//! Terminal UI for playing the engine with the mouse or keyboard, or watching it play itself.

mod app;
mod engine;
mod octants;
mod sprites;
mod view;

pub use app::{Mode, PlayConfig, TimeSetting, parse_clock};

use app::{App, EngineTask, TaskKind};
use engine::{Engine, Reply, SearchId, SearchLimit};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;
use std::io::{self, stdout};
use std::time::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_millis(50);

/// Engine resources for a session.
pub struct EngineConfig {
    pub threads: usize,
    pub hash_mb: usize,
}

/// Runs the UI until the user quits, then returns a record of the game.
pub fn run(config: PlayConfig, engine: EngineConfig) -> io::Result<String> {
    let mut engine = Engine::spawn(engine.threads, engine.hash_mb)?;
    let mut app = App::new(config, Instant::now());
    let mut terminal = TerminalGuard::enter()?;
    Session::default().run(&mut terminal.terminal, &mut app, &mut engine)?;
    Ok(app.record())
}

/// The search the engine is running for the app, if any.
#[derive(Default)]
struct Session {
    running: Option<(EngineTask, SearchId)>,
}

impl Session {
    fn run(&mut self, terminal: &mut DefaultTerminal, app: &mut App, engine: &mut Engine) -> io::Result<()> {
        let mut area = Rect::default();
        while !app.quit {
            let now = Instant::now();
            terminal.draw(|frame| {
                area = frame.area();
                view::draw(frame, app, now);
            })?;
            if event::poll(FRAME_INTERVAL)? {
                let event = event::read()?;
                handle_event(app, engine, &event, area, Instant::now());
            }
            while let Some(reply) = engine.try_reply() {
                self.handle_reply(app, reply, Instant::now());
            }
            app.tick(Instant::now());
            self.sync(app, engine, Instant::now());
        }
        engine.stop();
        Ok(())
    }

    /// Starts or stops searches so the engine does what the app currently wants.
    fn sync(&mut self, app: &App, engine: &mut Engine, now: Instant) {
        let wanted = app.engine_task();
        if self.running.map(|(task, _)| task) == wanted {
            return;
        }
        engine.stop();
        self.running = wanted.map(|task| {
            let budget = (task.kind == TaskKind::Think).then(|| app.think_budget(now));
            (task, engine.start(app.board(), SearchLimit::Time(budget)))
        });
    }

    fn handle_reply(&mut self, app: &mut App, reply: Reply, now: Instant) {
        let Some((task, running_id)) = self.running else {
            return;
        };
        if app.engine_task() != Some(task) {
            return;
        }
        match reply {
            Reply::Info { id, info } if id == running_id => {
                let root = app.board().clone();
                app.record_engine_info(task.kind, &root, &info);
            }
            Reply::Done { id, outcome } if id == running_id => {
                self.running = None;
                if let (TaskKind::Think, Some(m)) = (task.kind, outcome.best_move) {
                    app.play_engine_move(m, now);
                }
            }
            Reply::Info { .. } | Reply::Done { .. } | Reply::MoveScores { .. } => {}
        }
    }
}

fn handle_event(app: &mut App, engine: &mut Engine, event: &Event, area: Rect, now: Instant) {
    match event {
        Event::Key(key) if key.is_press() => handle_key(app, engine, *key, now),
        Event::Mouse(mouse) => handle_mouse(app, *mouse, area, now),
        _ => {}
    }
}

fn handle_key(app: &mut App, engine: &mut Engine, key: KeyEvent, now: Instant) {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.quit = true;
        return;
    }
    if app.typed.is_some() {
        handle_typing(app, key.code, now);
        return;
    }
    if app.pending_promotion().is_some() {
        if let Some(kind) = key.code.as_char().and_then(promotion_kind) {
            app.choose_promotion(kind, now);
        } else if key.code == KeyCode::Esc {
            app.cancel();
        }
        return;
    }
    if app.panels.help {
        app.panels.help = false;
        return;
    }
    app.message = None;
    match key.code {
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Esc => app.cancel(),
        KeyCode::Enter | KeyCode::Char(':') => app.typed = Some(String::new()),
        KeyCode::Char('?' | 'h') => app.panels.help = true,
        KeyCode::Char('e') => app.panels.eval = !app.panels.eval,
        KeyCode::Char('l') => app.panels.moves = !app.panels.moves,
        KeyCode::Char('a') => app.toggle_analysis(),
        KeyCode::Char('f') => app.flipped = !app.flipped,
        KeyCode::Char('p') => app.piece_style = app.piece_style.next(),
        KeyCode::Char('u') => app.undo(now),
        KeyCode::Char(' ') => app.toggle_pause(now),
        KeyCode::Char('n') => {
            engine.new_game();
            app.new_game(now);
        }
        KeyCode::Char('s') => {
            engine.new_game();
            app.swap_sides(now);
        }
        _ => {}
    }
}

fn handle_typing(app: &mut App, code: KeyCode, now: Instant) {
    let Some(typed) = app.typed.as_mut() else {
        return;
    };
    match code {
        KeyCode::Enter => app.submit_typed(now),
        KeyCode::Esc => app.typed = None,
        KeyCode::Backspace => {
            typed.pop();
        }
        KeyCode::Char(c) if !c.is_control() => typed.push(c),
        _ => {}
    }
}

fn promotion_kind(c: char) -> Option<crate::types::PieceKind> {
    use crate::types::PieceKind;
    match c.to_ascii_lowercase() {
        'q' => Some(PieceKind::Queen),
        'r' => Some(PieceKind::Rook),
        'b' => Some(PieceKind::Bishop),
        'n' => Some(PieceKind::Knight),
        _ => None,
    }
}

fn handle_mouse(app: &mut App, mouse: MouseEvent, area: Rect, now: Instant) {
    let Some(board) = view::layout(area, app).board else {
        return;
    };
    let square = board.square_at(mouse.column, mouse.row, app.flipped);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if app.panels.help {
                app.panels.help = false;
            } else if app.pending_promotion().is_some() {
                match board.promotion_at(mouse.column, mouse.row) {
                    Some(kind) => app.choose_promotion(kind, now),
                    None => app.cancel(),
                }
            } else if let Some(square) = square {
                app.message = None;
                app.press_square(square, now);
            } else {
                app.cancel();
            }
        }
        MouseEventKind::Up(MouseButton::Left) => match square {
            Some(square) => app.release_square(square, now),
            None => app.release_off_board(),
        },
        _ => {}
    }
}

/// Raw mode, the alternate screen and mouse capture, undone on drop or panic.
struct TerminalGuard {
    terminal: DefaultTerminal,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        let terminal = ratatui::try_init()?;
        let restore_terminal = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = execute!(stdout(), DisableMouseCapture);
            restore_terminal(info);
        }));
        if let Err(e) = execute!(stdout(), EnableMouseCapture) {
            ratatui::restore();
            return Err(e);
        }
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}
