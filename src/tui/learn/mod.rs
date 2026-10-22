//! Learn mode: a coach that teaches openings, sets puzzles and endgame drills, plays you at
//! your level and reviews your games.

mod common;
mod data;
mod endgames;
mod game;
mod games_list;
mod home;
mod line;
mod openings;
mod progress;
mod puzzles;
mod review;
mod setup;

use super::engine::{Engine, Reply, SearchId, SearchLimit, SearchOutcome};
use super::{EngineConfig, TerminalGuard};
use crate::board::Board;
use crate::coach::store::Store;
use crate::search::Score;
use crate::types::Move;
use data::CoachData;
use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::layout::Rect;
use std::io;
use std::time::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_millis(50);

/// Runs learn mode until you quit.
pub fn run(engine: EngineConfig, store: Store) -> io::Result<()> {
    let mut data = CoachData::load(store)?;
    let mut engine = Engine::spawn(engine.threads, engine.hash_mb)?;
    let mut terminal = TerminalGuard::enter()?;
    let mut session = Session {
        stack: vec![Box::new(home::Home::default())],
        running: None,
    };
    session.run(&mut terminal.terminal, &mut data, &mut engine)
}

/// Work a screen wants from the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Search(SearchLimit),
    ScoreMoves { nodes_per_move: u64 },
}

/// A job for the engine; `tag` tells the screen which of its requests a result answers.
pub struct Job {
    pub tag: u64,
    pub kind: JobKind,
    pub board: Board,
}

pub enum JobResult {
    Done(SearchOutcome),
    Scores(Vec<(Move, Score)>),
}

/// Where to go after handling an event.
pub enum Nav {
    Stay,
    Push(Box<dyn Screen>),
    Replace(Box<dyn Screen>),
    Back,
    Quit,
}

/// One screen of learn mode.
pub trait Screen {
    fn draw(&self, frame: &mut Frame, data: &CoachData, now: Instant);
    fn key(&mut self, key: KeyEvent, data: &mut CoachData, now: Instant) -> Nav;
    fn mouse(
        &mut self,
        _mouse: MouseEvent,
        _area: Rect,
        _data: &mut CoachData,
        _now: Instant,
    ) -> Nav {
        Nav::Stay
    }
    /// What the engine should be doing for this screen now.
    fn job(&self, _data: &CoachData) -> Option<Job> {
        None
    }
    fn engine(
        &mut self,
        _tag: u64,
        _result: JobResult,
        _data: &mut CoachData,
        _now: Instant,
    ) -> Nav {
        Nav::Stay
    }
    fn tick(&mut self, _data: &mut CoachData, _now: Instant) -> Nav {
        Nav::Stay
    }
}

struct Session {
    stack: Vec<Box<dyn Screen>>,
    running: Option<(u64, JobKind, SearchId)>,
}

impl Session {
    fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
        data: &mut CoachData,
        engine: &mut Engine,
    ) -> io::Result<()> {
        let mut area = Rect::default();
        while let Some(screen) = self.stack.last() {
            let now = Instant::now();
            terminal.draw(|frame| {
                area = frame.area();
                screen.draw(frame, data, now);
            })?;
            let mut nav = Nav::Stay;
            if event::poll(FRAME_INTERVAL)? {
                nav = self.handle_event(&event::read()?, area, data);
            }
            while matches!(nav, Nav::Stay)
                && let Some(reply) = engine.try_reply()
            {
                nav = self.handle_reply(reply, data);
            }
            if matches!(nav, Nav::Stay)
                && let Some(screen) = self.stack.last_mut()
            {
                nav = screen.tick(data, Instant::now());
            }
            if !self.navigate(nav, engine) {
                break;
            }
            self.sync(data, engine);
        }
        engine.stop();
        Ok(())
    }

    fn handle_event(&mut self, event: &Event, area: Rect, data: &mut CoachData) -> Nav {
        let Some(screen) = self.stack.last_mut() else {
            return Nav::Quit;
        };
        let now = Instant::now();
        match event {
            Event::Key(key)
                if key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                Nav::Quit
            }
            Event::Key(key) if key.is_press() => screen.key(*key, data, now),
            Event::Mouse(mouse) => screen.mouse(*mouse, area, data, now),
            _ => Nav::Stay,
        }
    }

    fn handle_reply(&mut self, reply: Reply, data: &mut CoachData) -> Nav {
        let Some((tag, _, running_id)) = self.running else {
            return Nav::Stay;
        };
        let (id, result) = match reply {
            Reply::Info { .. } => return Nav::Stay,
            Reply::Done { id, outcome } => (id, JobResult::Done(outcome)),
            Reply::MoveScores { id, scores } => (id, JobResult::Scores(scores)),
        };
        if id != running_id {
            return Nav::Stay;
        }
        self.running = None;
        match self.stack.last_mut() {
            Some(screen) => screen.engine(tag, result, data, Instant::now()),
            None => Nav::Stay,
        }
    }

    /// Applies `nav`; false once there is nothing left to show.
    fn navigate(&mut self, nav: Nav, engine: &mut Engine) -> bool {
        let changes_screen = !matches!(nav, Nav::Stay);
        match nav {
            Nav::Stay => {}
            Nav::Push(screen) => self.stack.push(screen),
            Nav::Replace(screen) => {
                self.stack.pop();
                self.stack.push(screen);
            }
            Nav::Back => {
                self.stack.pop();
            }
            Nav::Quit => self.stack.clear(),
        }
        if changes_screen {
            engine.stop();
            self.running = None;
        }
        !self.stack.is_empty()
    }

    /// Starts or stops engine work so it matches what the current screen wants.
    fn sync(&mut self, data: &CoachData, engine: &mut Engine) {
        let wanted = self.stack.last().and_then(|screen| screen.job(data));
        let wanted_key = wanted.as_ref().map(|job| (job.tag, job.kind));
        if self.running.map(|(tag, kind, _)| (tag, kind)) == wanted_key {
            return;
        }
        engine.stop();
        self.running = wanted.map(|job| {
            let id = match job.kind {
                JobKind::Search(limit) => engine.start(&job.board, limit),
                JobKind::ScoreMoves { nodes_per_move } => {
                    engine.score_moves(&job.board, nodes_per_move)
                }
            };
            (job.tag, job.kind, id)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coach::bot::Level;
    use crate::coach::repertoire::all_lines;
    use crate::coach::srs::UnixTime;
    use crate::coach::store::{Ending, GameId, GameRecord};
    use crate::game_runner::GameResult;
    use crate::notation::parse_san_line;
    use crate::types::{Color, START_FEN};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn temp_data(name: &str) -> CoachData {
        let dir =
            std::env::temp_dir().join(format!("chess-learn-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        CoachData::load(Store::open(dir).unwrap()).unwrap()
    }

    fn played_game(data: &mut CoachData) -> GameId {
        let start = Board::from_fen(START_FEN).unwrap();
        let moves = parse_san_line(&start, &["f3", "e5", "g4", "Qh4#"]).unwrap();
        let id = GameId(42);
        data.add_game(GameRecord {
            id,
            played_at: UnixTime(1_700_000_000),
            human: Color::White,
            level: Level::new(2).unwrap(),
            result: GameResult::BlackWin,
            ending: Ending::Checkmate,
            blunder_check: false,
            start_fen: START_FEN.to_string(),
            moves,
            analysis: Vec::new(),
        })
        .unwrap();
        id
    }

    fn render_everywhere(screen: &dyn Screen, data: &CoachData) {
        for (width, height) in [(1, 1), (20, 6), (40, 12), (80, 24), (140, 42), (250, 80)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| screen.draw(frame, data, Instant::now()))
                .unwrap();
        }
    }

    #[test]
    fn every_screen_draws_at_any_terminal_size() {
        let mut data = temp_data("draw");
        let game = played_game(&mut data);
        let line = all_lines().next().unwrap();
        let settings = game::GameSettings {
            level: Level::WEAKEST,
            human: Color::Black,
            clock: None,
            blunder_check: true,
            opening: None,
        };
        let screens: Vec<Box<dyn Screen>> = vec![
            Box::new(home::Home::default()),
            Box::new(setup::Setup::new(&data)),
            Box::new(game::CoachGame::new(settings, &data)),
            Box::new(review::ReviewScreen::new(game, &data)),
            Box::new(games_list::GamesList::default()),
            Box::new(puzzles::Puzzles::new(&mut data)),
            Box::new(openings::Openings::new(&data)),
            Box::new(line::LineScreen::lesson(line, Vec::new())),
            Box::new(line::LineScreen::drill(line, Vec::new())),
            Box::new(endgames::Endgames::default()),
            Box::new(progress::Progress::default()),
        ];
        for screen in &screens {
            render_everywhere(screen.as_ref(), &data);
        }
    }

    #[test]
    fn a_blunder_in_a_saved_game_becomes_a_puzzle_once_reviewed() {
        let mut data = temp_data("puzzle");
        let game = played_game(&mut data);
        let mut screen = review::ReviewScreen::new(game, &data);
        let analysis: [(Score, &str); 4] = [
            (Score::Centipawns(20), "e4"),
            (Score::Centipawns(-10), "e5"),
            (Score::Centipawns(-60), "e4"),
            (Score::MateIn(1), "Qh4#"),
        ];
        while let Some(job) = screen.job(&data) {
            let (score, best) = analysis[job.tag as usize];
            let pv = parse_san_line(&job.board, &[best]).unwrap();
            let outcome = SearchOutcome {
                best_move: pv.first().copied(),
                score: Some(score),
                pv,
            };
            screen.engine(job.tag, JobResult::Done(outcome), &mut data, Instant::now());
            screen.tick(&mut data, Instant::now());
        }
        screen.tick(&mut data, Instant::now());

        assert!(data.game(game).unwrap().record.is_fully_analysed());
        assert_eq!(data.own_puzzles.len(), 1);
    }
}
