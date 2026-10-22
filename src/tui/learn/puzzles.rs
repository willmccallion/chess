//! Solving puzzles: mistakes from your games first, then tactics on the themes you need.

use super::super::engine::SearchLimit;
use super::super::view::{DIM, board_layout};
use super::common::{
    BAD, BoardKey, BoardPanel, GOOD, INFO, MARK_GOOD, MARK_HINT, TEXT, colored, dim, draw_board,
    draw_help, draw_status, heading, move_marks, panel, screen_layout,
};
use super::data::CoachData;
use super::game::eval_after_search;
use super::{Job, JobKind, JobResult, Nav, Screen};
use crate::coach::judgement::Eval;
use crate::coach::profile::PuzzleReason;
use crate::coach::puzzles::{Attempt, AttemptState, PuzzleId, Verdict, describe_themes};
use crate::coach::rating::Elo;
use crate::coach::store::TrainingItem;
use crate::notation::{numbered, san_of};
use crate::time::TimeBudget;
use crate::types::{Color, Move};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use std::time::Instant;

/// How long the engine looks at an answer that differs from the stored one.
const JUDGE_MS: u64 = 600;
/// An answer from your own game passes if it keeps this close (in win-chance points) to
/// the engine's best.
const GOOD_ENOUGH_DROP: f64 = 5.0;

const KEYS: &[(&str, &str)] = &[
    ("click / drag", "play a move"),
    ("Enter", "type a move"),
    ("h", "hint: show which piece to move"),
    ("s", "show the solution, one move at a time"),
    ("r", "try this puzzle again"),
    ("n", "next puzzle"),
    ("f", "flip the board"),
    ("Esc", "back"),
];

enum Feedback {
    Start,
    Correct,
    Checking,
    Solved { also_good: bool },
    Wrong { expected: String },
    Revealed,
}

struct Current {
    attempt: Attempt,
    reason: PuzzleReason,
    played_in_game: Option<String>,
    best_before: Option<Eval>,
    feedback: Feedback,
    hint: bool,
    logged: bool,
    /// Your move waiting for the engine's judgement.
    judging: Option<Move>,
}

pub struct Puzzles {
    current: Option<Current>,
    panel: BoardPanel,
    message: Option<String>,
    rating_before: Elo,
    help: bool,
}

impl Puzzles {
    pub fn new(data: &mut CoachData) -> Self {
        let mut screen = Self {
            current: None,
            panel: BoardPanel::new(crate::board::Board::empty(), false),
            message: None,
            rating_before: data.profile.puzzle_rating,
            help: false,
        };
        screen.load_next(data, None);
        screen
    }

    fn load_next(&mut self, data: &mut CoachData, skip: Option<&PuzzleId>) {
        self.rating_before = data.profile.puzzle_rating;
        self.current = data.next_puzzle(skip).map(|(puzzle, reason)| Current {
            played_in_game: data.move_played_in_game(&puzzle.id),
            best_before: data.game_eval_before(&puzzle.id),
            attempt: Attempt::new(puzzle),
            reason,
            feedback: Feedback::Start,
            hint: false,
            logged: false,
            judging: None,
        });
        self.reset_board();
    }

    fn reset_board(&mut self) {
        if let Some(current) = &self.current {
            let board = current.attempt.board.clone();
            self.panel.flipped = board.turn == Color::Black;
            self.panel
                .set_position(board, current.attempt.puzzle.last_move);
        }
    }

    fn retry(&mut self) {
        if let Some(current) = self.current.as_mut() {
            let puzzle = current.attempt.puzzle.clone();
            current.attempt = Attempt::new(puzzle);
            current.feedback = Feedback::Start;
            current.judging = None;
        }
        self.reset_board();
    }

    fn play(&mut self, m: Move, data: &mut CoachData) {
        let Some(current) = self.current.as_mut() else {
            return;
        };
        if current.attempt.state != AttemptState::Solving || current.judging.is_some() {
            return;
        }
        let verdict = current.attempt.play(m);
        self.apply(verdict, m, data);
    }

    fn apply(&mut self, verdict: Verdict, m: Move, data: &mut CoachData) {
        let Some(current) = self.current.as_mut() else {
            return;
        };
        match verdict {
            Verdict::Continue { reply } => {
                self.panel
                    .set_position(current.attempt.board.clone(), Some(reply));
                current.feedback = Feedback::Correct;
            }
            Verdict::Solved => {
                let also_good = current.judging.is_some();
                current.judging = None;
                self.panel
                    .set_position(current.attempt.board.clone(), Some(m));
                current.feedback = Feedback::Solved { also_good };
            }
            Verdict::NeedsEngine => {
                current.judging = Some(m);
                self.panel.play(m);
                current.feedback = Feedback::Checking;
            }
            Verdict::Wrong { expected } => {
                current.judging = None;
                let expected = san_of(&current.attempt.board, expected)
                    .map(|san| numbered(&current.attempt.board, &san, true))
                    .unwrap_or_default();
                self.panel.set_position(
                    current.attempt.board.clone(),
                    current.attempt.puzzle.last_move,
                );
                current.feedback = Feedback::Wrong { expected };
            }
        }
        self.log_if_finished(data);
    }

    fn log_if_finished(&mut self, data: &mut CoachData) {
        let Some(current) = self.current.as_mut() else {
            return;
        };
        if current.logged || current.attempt.state == AttemptState::Solving {
            return;
        }
        current.logged = true;
        let success = matches!(
            current.attempt.state,
            AttemptState::Solved { first_try: true }
        ) && !current.hint;
        let item = TrainingItem::Puzzle(current.attempt.puzzle.id.clone());
        if let Err(e) = data.record_attempt(item, success, current.attempt.puzzle.rating, None) {
            self.message = Some(format!("Could not save the result: {e}"));
        }
    }

    fn reveal(&mut self, data: &mut CoachData) {
        let Some(current) = self.current.as_mut() else {
            return;
        };
        if current.attempt.state == AttemptState::Solving {
            current.attempt.state = AttemptState::Failed;
        }
        if let Some(m) = current.attempt.reveal_next() {
            self.panel
                .set_position(current.attempt.board.clone(), Some(m));
            current.feedback = Feedback::Revealed;
        }
        self.log_if_finished(data);
    }

    fn side_lines(&self, data: &CoachData) -> Vec<Line<'static>> {
        let Some(current) = &self.current else {
            return empty_lines(data);
        };
        let puzzle = &current.attempt.puzzle;
        let mut lines = vec![Line::from(vec![Span::styled(
            current.reason.describe(),
            Style::new().fg(INFO),
        )])];
        let rating = puzzle
            .rating
            .map_or_else(String::new, |r| format!(" · puzzle {}", r.0));
        lines.push(dim(format!(
            "Your puzzle rating {}{rating}",
            data.profile.puzzle_rating.0
        )));
        lines.push(Line::raw(""));
        let side = if puzzle.board.turn == Color::White {
            "White"
        } else {
            "Black"
        };
        let task = if puzzle.is_from_your_games() {
            format!("{side} to move. Find a better move than the one you played.")
        } else {
            format!("{side} to move. Find the best move.")
        };
        lines.push(heading(task));
        if let Some(played) = &current.played_in_game {
            lines.push(dim(format!("In your game you played {played} here.")));
        }
        lines.push(Line::raw(""));
        let finished = current.attempt.state != AttemptState::Solving;
        match &current.feedback {
            Feedback::Start => {}
            Feedback::Correct => lines.push(colored("Correct! Keep going.", GOOD)),
            Feedback::Checking => lines.push(dim("Checking your move with the engine…")),
            Feedback::Solved { also_good } => {
                let text = if *also_good {
                    "Solved! That works just as well."
                } else {
                    "Solved!"
                };
                lines.push(Line::styled(
                    text,
                    Style::new().fg(GOOD).add_modifier(Modifier::BOLD),
                ));
            }
            Feedback::Wrong { expected } => {
                lines.push(Line::styled(
                    "Not quite.",
                    Style::new().fg(BAD).add_modifier(Modifier::BOLD),
                ));
                lines.push(Line::styled(
                    format!("The answer was {expected}."),
                    Style::new().fg(TEXT),
                ));
            }
            Feedback::Revealed => {
                lines.push(dim("Showing the solution: press s for the next move."))
            }
        }
        if finished {
            let change = data.profile.puzzle_rating.0 - self.rating_before.0;
            if puzzle.rating.is_some() && change != 0 {
                lines.push(colored(
                    format!(
                        "Puzzle rating {} → {} ({change:+})",
                        self.rating_before.0, data.profile.puzzle_rating.0
                    ),
                    if change > 0 { GOOD } else { BAD },
                ));
            }
            let themes = describe_themes(&puzzle.themes);
            if !themes.is_empty() {
                lines.push(dim(format!("Themes: {themes}")));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "n: next puzzle   r: try again   s: show solution",
                Style::new().fg(DIM),
            ));
        }
        if let Some(message) = &self.message {
            lines.push(colored(message.clone(), BAD));
        }
        lines
    }

    fn marks(&self) -> Vec<(u8, ratatui::style::Color)> {
        let Some(current) = &self.current else {
            return Vec::new();
        };
        if current.hint
            && current.attempt.state == AttemptState::Solving
            && let Some(expected) = current.attempt.expected()
        {
            return vec![(expected.from, MARK_HINT)];
        }
        if let Feedback::Wrong { .. } = current.feedback
            && let Some(expected) = current.attempt.expected()
        {
            return move_marks(expected, MARK_GOOD);
        }
        Vec::new()
    }

    fn is_solving(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|c| c.attempt.state == AttemptState::Solving && c.judging.is_none())
    }
}

fn empty_lines(data: &CoachData) -> Vec<Line<'static>> {
    vec![
        heading("No puzzles waiting."),
        Line::raw(""),
        Line::styled(
            "Every mistake you make in a game against the coach becomes a puzzle here.".to_string(),
            Style::new().fg(TEXT),
        ),
        Line::raw(""),
        Line::styled(
            "For thousands more, import the free Lichess puzzle database:".to_string(),
            Style::new().fg(TEXT),
        ),
        dim("  1. Download lichess_db_puzzle.csv.zst from database.lichess.org"),
        dim("  2. zstd -dc lichess_db_puzzle.csv.zst | chess import-puzzles -"),
        Line::raw(""),
        dim(format!("Your data lives in {}", data.store_dir().display())),
    ]
}

impl Screen for Puzzles {
    fn draw(&self, frame: &mut Frame, data: &CoachData, _now: Instant) {
        let layout = screen_layout(frame.area());
        if self.current.is_some() {
            draw_board(
                frame,
                &layout,
                &self.panel.scene(data.piece_style, self.marks()),
            );
        }
        let side = if self.current.is_some() {
            layout.side
        } else {
            super::common::centred(frame.area(), 76, 14)
        };
        frame.render_widget(
            Paragraph::new(self.side_lines(data))
                .wrap(Wrap { trim: false })
                .block(panel("Puzzles")),
            side,
        );
        draw_status(
            frame,
            layout.status,
            self.panel.typed.as_deref(),
            None,
            "click or drag to move · h hint · s solution · n next · ? help · Esc back",
        );
        if self.help {
            draw_help(frame, KEYS);
        }
    }

    fn key(&mut self, key: KeyEvent, data: &mut CoachData, _now: Instant) -> Nav {
        if self.help {
            self.help = false;
            return Nav::Stay;
        }
        self.message = None;
        match self.panel.key(key, self.is_solving()) {
            BoardKey::Move(m) => {
                self.play(m, data);
                return Nav::Stay;
            }
            BoardKey::Invalid(text) => {
                self.message = Some(text);
                return Nav::Stay;
            }
            BoardKey::Handled => return Nav::Stay,
            BoardKey::NotHandled => {}
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Nav::Back,
            KeyCode::Char('n') => {
                let skip = self.current.as_ref().map(|c| c.attempt.puzzle.id.clone());
                self.load_next(data, skip.as_ref());
            }
            KeyCode::Char('r') => self.retry(),
            KeyCode::Char('s') => self.reveal(data),
            KeyCode::Char('h') => {
                if let Some(current) = self.current.as_mut() {
                    current.hint = true;
                }
            }
            KeyCode::Char('f') => self.panel.flipped = !self.panel.flipped,
            KeyCode::Char('p') => data.piece_style = data.piece_style.next(),
            KeyCode::Char('?') => self.help = true,
            _ => {}
        }
        Nav::Stay
    }

    fn mouse(&mut self, mouse: MouseEvent, area: Rect, data: &mut CoachData, _now: Instant) -> Nav {
        let geometry = board_layout(area, false).board;
        if let Some(m) = self.panel.mouse(mouse, geometry, self.is_solving()) {
            self.play(m, data);
        }
        Nav::Stay
    }

    fn job(&self, _data: &CoachData) -> Option<Job> {
        let current = self.current.as_ref()?;
        let m = current.judging?;
        let mut board = current.attempt.board.clone();
        board.make_move(m);
        Some(Job {
            tag: u64::from(m.from) << 8 | u64::from(m.to),
            kind: JobKind::Search(SearchLimit::Time(Some(TimeBudget::fixed(JUDGE_MS)))),
            board,
        })
    }

    fn engine(&mut self, _tag: u64, result: JobResult, data: &mut CoachData, _now: Instant) -> Nav {
        let JobResult::Done(outcome) = result else {
            return Nav::Stay;
        };
        let Some(current) = self.current.as_mut() else {
            return Nav::Stay;
        };
        let Some(m) = current.judging else {
            return Nav::Stay;
        };
        let mover = current.attempt.board.turn;
        let mut after = current.attempt.board.clone();
        after.make_move(m);
        let chance = eval_after_search(&after, outcome.score).win_chance_for(after.turn, mover);
        let good_enough = current
            .best_before
            .is_some_and(|best| best.win_chance().drop_to(chance) <= GOOD_ENOUGH_DROP);
        let verdict = current.attempt.engine_judged(m, good_enough);
        self.apply(verdict, m, data);
        Nav::Stay
    }
}
