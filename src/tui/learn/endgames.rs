//! Endgame drills: win or hold key positions against the engine's best defence.

use super::common::{
    BAD, BoardKey, BoardPanel, GOOD, INFO, TEXT, centred, colored, dim, draw_board, draw_help, draw_status, heading,
    panel, screen_layout,
};
use super::data::CoachData;
use super::super::engine::SearchLimit;
use super::super::view::{ACCENT, DIM, board_layout};
use super::{Job, JobKind, JobResult, Nav, Screen};
use crate::coach::endgames::{DRILLS, Drill, Goal, has_won_queen_ending};
use crate::coach::store::TrainingItem;
use crate::game_runner::{GameEndReason, GameResult, position_outcome, win_for};
use crate::search::Score;
use crate::time::TimeBudget;
use crate::notation::{numbered, san_of};
use crate::types::{Color, Move};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use std::time::Instant;

const ENGINE_MOVE_MS: u64 = 700;
/// In a drill to win, giving up after this many of your moves.
const MOVE_LIMIT: usize = 60;
/// While holding a draw, the drill is lost once the engine thinks it is this far ahead.
const LOST_HOLD_CP: i32 = 500;
const WIDTH: u16 = 84;

const KEYS: &[(&str, &str)] = &[
    ("click / drag", "play your move"),
    ("Enter", "type a move"),
    ("t", "show the next tip"),
    ("r", "start again"),
    ("f", "flip the board"),
    ("Esc", "back"),
];

/// The list of drills.
#[derive(Default)]
pub struct Endgames {
    selected: usize,
}

fn list_area(area: Rect) -> Rect {
    centred(area, WIDTH, (DRILLS.len() as u16) * 2 + 4)
}

impl Screen for Endgames {
    fn draw(&self, frame: &mut Frame, data: &CoachData, _now: Instant) {
        let area = frame.area();
        let mut lines = Vec::new();
        for (i, drill) in DRILLS.iter().enumerate() {
            let chosen = i == self.selected;
            let item = TrainingItem::Endgame(drill.id.to_string());
            let tries: Vec<_> = data.attempts.iter().filter(|a| a.item == item).collect();
            let best = tries.iter().filter(|a| a.success).filter_map(|a| a.moves).min();
            let status = match (best, tries.is_empty()) {
                (Some(moves), _) => Span::styled(format!("done in {moves} moves"), Style::new().fg(GOOD)),
                (None, false) => Span::styled("not yet", Style::new().fg(BAD)),
                (None, true) => Span::styled("new", Style::new().fg(INFO)),
            };
            lines.push(Line::from(vec![
                Span::styled(if chosen { "▸ " } else { "  " }, Style::new().fg(ACCENT)),
                Span::styled(
                    format!("{:<38}", drill.name),
                    if chosen { Style::new().fg(TEXT).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT) },
                ),
                status,
            ]));
            lines.push(Line::styled(format!("    {}", drill.goal.describe()), Style::new().fg(DIM)));
        }
        frame.render_widget(Paragraph::new(lines).block(panel("Endgames")), list_area(area));
        let status = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, area.height.min(1));
        frame.render_widget(Paragraph::new(dim("  ↑↓ choose · Enter start · Esc back")), status);
    }

    fn key(&mut self, key: KeyEvent, _data: &mut CoachData, _now: Instant) -> Nav {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Nav::Back,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                Nav::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(DRILLS.len() - 1);
                Nav::Stay
            }
            KeyCode::Enter | KeyCode::Right => open(self.selected),
            _ => Nav::Stay,
        }
    }

    fn mouse(&mut self, mouse: MouseEvent, area: Rect, _data: &mut CoachData, _now: Instant) -> Nav {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Nav::Stay;
        }
        let list = list_area(area);
        let Some(offset) = mouse.row.checked_sub(list.y + 1) else {
            return Nav::Stay;
        };
        let index = usize::from(offset / 2);
        if index < DRILLS.len() {
            self.selected = index;
            return open(index);
        }
        Nav::Stay
    }
}

fn open(index: usize) -> Nav {
    match DRILLS.get(index).and_then(EndgameDrill::new) {
        Some(screen) => Nav::Push(Box::new(screen)),
        None => Nav::Stay,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Playing,
    Won,
    Lost,
}

/// One drill being played.
struct EndgameDrill {
    drill: &'static Drill,
    you: Color,
    panel: BoardPanel,
    /// The moves played so far, numbered.
    history: Vec<String>,
    your_moves: usize,
    tips_shown: usize,
    state: State,
    verdict: Option<String>,
    message: Option<String>,
    help: bool,
}

impl EndgameDrill {
    fn new(drill: &'static Drill) -> Option<Self> {
        let board = drill.board().ok()?;
        let you = board.turn;
        Some(Self {
            drill,
            you,
            panel: BoardPanel::new(board, you == Color::Black),
            history: Vec::new(),
            your_moves: 0,
            tips_shown: 0,
            state: State::Playing,
            verdict: None,
            message: None,
            help: false,
        })
    }

    fn restart(&mut self) {
        if let Ok(board) = self.drill.board() {
            self.panel.set_position(board, None);
        }
        self.your_moves = 0;
        self.history.clear();
        self.state = State::Playing;
        self.verdict = None;
        self.message = None;
    }

    fn your_turn(&self) -> bool {
        self.state == State::Playing && self.panel.board().turn == self.you
    }

    fn play_yours(&mut self, m: Move, data: &mut CoachData) {
        if !self.your_turn() {
            return;
        }
        self.play(m);
        self.your_moves += 1;
        self.judge(data);
    }

    fn play(&mut self, m: Move) {
        let board = self.panel.board();
        if let Some(san) = san_of(board, m) {
            self.history.push(numbered(board, &san, self.history.is_empty()));
        }
        self.panel.play(m);
    }

    fn judge(&mut self, data: &mut CoachData) {
        if let Some((won, verdict)) = self.decide() {
            self.end(won, verdict, data);
        }
    }

    /// Whether the position after the last move decides the drill, and how.
    fn decide(&self) -> Option<(bool, String)> {
        let board = self.panel.board();
        let goal = self.drill.goal;
        match position_outcome(board) {
            Some((result, GameEndReason::Checkmate)) if result == win_for(self.you) => {
                Some((true, "Checkmate! Well done.".to_string()))
            }
            Some((GameResult::Draw, reason)) => match goal {
                Goal::Hold { .. } => Some((true, format!("Drawn by {reason}: you held it!"))),
                Goal::Checkmate | Goal::Promote => Some((false, format!("Drawn by {reason}. Careful: that throws the win away."))),
            },
            Some(_) => Some((false, "You were checkmated.".to_string())),
            None => self.goal_reached(),
        }
    }

    fn goal_reached(&self) -> Option<(bool, String)> {
        let board = self.panel.board();
        let after_their_move = board.turn == self.you;
        match self.drill.goal {
            Goal::Promote if after_their_move && has_won_queen_ending(board, self.you) => {
                Some((true, "Promoted, and the new queen is safe: it's a win.".to_string()))
            }
            Goal::Hold { moves } if after_their_move && self.your_moves >= moves => {
                Some((true, format!("You held for {moves} moves: drawn!")))
            }
            Goal::Checkmate | Goal::Promote if self.your_moves >= MOVE_LIMIT => {
                Some((false, format!("{MOVE_LIMIT} moves without finishing: try a more direct plan.")))
            }
            _ => None,
        }
    }

    fn end(&mut self, won: bool, verdict: String, data: &mut CoachData) {
        self.state = if won { State::Won } else { State::Lost };
        self.verdict = Some(verdict);
        let item = TrainingItem::Endgame(self.drill.id.to_string());
        if let Err(e) = data.record_attempt(item, won, None, Some(self.your_moves as u32)) {
            self.message = Some(format!("Could not save the result: {e}"));
        }
    }

    fn side_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![
            heading(self.drill.name),
            Line::styled(self.drill.goal.describe(), Style::new().fg(ACCENT)),
            Line::raw(""),
            Line::styled(self.drill.lesson.to_string(), Style::new().fg(TEXT)),
            Line::raw(""),
        ];
        for tip in self.drill.tips.iter().take(self.tips_shown) {
            lines.push(Line::from(vec![Span::styled("• ", Style::new().fg(INFO)), Span::styled(tip.to_string(), Style::new().fg(TEXT))]));
        }
        if self.tips_shown < self.drill.tips.len() {
            lines.push(dim("Stuck? Press t for a tip."));
        }
        lines.push(Line::raw(""));
        let recent: Vec<&str> = self.history.iter().rev().take(6).rev().map(String::as_str).collect();
        if !recent.is_empty() {
            lines.push(Line::styled(recent.join(" "), Style::new().fg(TEXT)));
        }
        let target = self.drill.target_moves.map_or_else(String::new, |t| format!(" (aim for {t})"));
        lines.push(dim(format!("Your moves: {}{target}", self.your_moves)));
        match self.state {
            State::Playing => {
                let text = if self.your_turn() { "Your move" } else { "The engine is defending…" };
                lines.push(heading(text));
            }
            State::Won => {
                lines.push(Line::styled(self.verdict.clone().unwrap_or_default(), Style::new().fg(GOOD).add_modifier(Modifier::BOLD)));
                if let Some(target) = self.drill.target_moves
                    && self.your_moves > target
                {
                    lines.push(dim("Now try to do it in fewer moves."));
                }
                lines.push(dim("r: try again   Esc: back"));
            }
            State::Lost => {
                lines.push(Line::styled(self.verdict.clone().unwrap_or_default(), Style::new().fg(BAD).add_modifier(Modifier::BOLD)));
                lines.push(dim("r: try again   t: tip   Esc: back"));
            }
        }
        if let Some(message) = &self.message {
            lines.push(colored(message.clone(), BAD));
        }
        lines
    }
}

impl Screen for EndgameDrill {
    fn draw(&self, frame: &mut Frame, data: &CoachData, _now: Instant) {
        let layout = screen_layout(frame.area());
        draw_board(frame, &layout, &self.panel.scene(data.piece_style, Vec::new()));
        frame.render_widget(
            Paragraph::new(self.side_lines()).wrap(Wrap { trim: false }).block(panel("Endgame drill")),
            layout.side,
        );
        draw_status(
            frame,
            layout.status,
            self.panel.typed.as_deref(),
            None,
            "click or drag to move · t tip · r restart · f flip · ? help · Esc back",
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
        match self.panel.key(key, self.your_turn()) {
            BoardKey::Move(m) => {
                self.play_yours(m, data);
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
            KeyCode::Char('t') => self.tips_shown = (self.tips_shown + 1).min(self.drill.tips.len()),
            KeyCode::Char('r') => self.restart(),
            KeyCode::Char('f') => self.panel.flipped = !self.panel.flipped,
            KeyCode::Char('p') => data.piece_style = data.piece_style.next(),
            KeyCode::Char('?') => self.help = true,
            _ => {}
        }
        Nav::Stay
    }

    fn mouse(&mut self, mouse: MouseEvent, area: Rect, data: &mut CoachData, _now: Instant) -> Nav {
        let geometry = board_layout(area, false).board;
        if let Some(m) = self.panel.mouse(mouse, geometry, self.your_turn()) {
            self.play_yours(m, data);
        }
        Nav::Stay
    }

    fn job(&self, _data: &CoachData) -> Option<Job> {
        if self.state != State::Playing || self.your_turn() {
            return None;
        }
        Some(Job {
            tag: self.your_moves as u64,
            kind: JobKind::Search(SearchLimit::Time(Some(TimeBudget::fixed(ENGINE_MOVE_MS)))),
            board: self.panel.board().clone(),
        })
    }

    fn engine(&mut self, tag: u64, result: JobResult, data: &mut CoachData, _now: Instant) -> Nav {
        let JobResult::Done(outcome) = result else {
            return Nav::Stay;
        };
        if tag != self.your_moves as u64 || self.your_turn() || self.state != State::Playing {
            return Nav::Stay;
        }
        let Some(m) = outcome.best_move.filter(|m| self.panel.legal().contains(m)) else {
            return Nav::Stay;
        };
        let engine_winning = match outcome.score {
            Some(Score::MateIn(n)) => n > 0,
            Some(Score::Centipawns(cp)) => cp >= LOST_HOLD_CP,
            None => false,
        };
        self.play(m);
        if matches!(self.drill.goal, Goal::Hold { .. }) && engine_winning && position_outcome(self.panel.board()).is_none() {
            self.end(false, "The position can't be held any more: replay and look for the drawing method.".to_string(), data);
            return Nav::Stay;
        }
        self.judge(data);
        Nav::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;
    use crate::coach::endgames::drill_by_id;

    fn drill_at(id: &str, fen: &str, your_moves: usize) -> EndgameDrill {
        let mut drill = EndgameDrill::new(drill_by_id(id).unwrap()).unwrap();
        drill.panel.set_position(Board::from_fen(fen).unwrap(), None);
        drill.your_moves = your_moves;
        drill
    }

    #[test]
    fn mating_the_lone_king_wins_the_drill() {
        let drill = drill_at("two-rooks", "3k3R/8/3K4/8/8/8/8/R7 b - - 0 1", 5);

        assert!(matches!(drill.decide(), Some((true, _))));
    }

    #[test]
    fn stalemating_the_lone_king_loses_a_mating_drill() {
        let drill = drill_at("queen-mate", "7k/5Q2/6K1/8/8/8/8/8 b - - 0 1", 5);

        assert!(matches!(drill.decide(), Some((false, _))));
    }

    #[test]
    fn a_safe_new_queen_wins_the_promotion_drill() {
        let drill = drill_at("kp-key-squares", "4Q3/8/8/2k5/8/8/8/4K3 w - - 1 9", 9);

        assert!(matches!(drill.decide(), Some((true, _))));
    }

    #[test]
    fn losing_the_pawn_draws_and_loses_the_promotion_drill() {
        let drill = drill_at("kp-key-squares", "4k3/8/8/8/8/8/8/4K3 w - - 0 9", 9);

        assert!(matches!(drill.decide(), Some((false, _))));
    }

    #[test]
    fn surviving_long_enough_holds_the_draw() {
        let drill = drill_at("kp-defend", "4k3/8/4K3/4P3/8/8/8/8 b - - 0 30", 20);

        assert!(matches!(drill.decide(), Some((true, _))));
    }

    #[test]
    fn an_undecided_position_plays_on() {
        let drill = drill_at("rook-mate", "8/8/8/4k3/8/8/8/R3K3 w - - 0 1", 1);

        assert_eq!(drill.decide(), None);
    }
}
