//! Going through a finished game with the coach: every move judged, mistakes explained.

use super::common::{
    BAD, GOOD, INFO, MARK_GOOD, TEXT, WARN, bar, class_color, class_mark, colored, dim, draw_board, draw_help,
    draw_status, heading, move_marks, panel, sparkline,
};
use super::data::CoachData;
use super::game::eval_after_search;
use super::super::engine::SearchLimit;
use super::super::view::{ACCENT, BoardScene, DIM, board_layout, draw_eval_bar, white_share};
use super::{Job, JobKind, JobResult, Nav, Screen};
use crate::board::Board;
use crate::coach::habits::HabitNote;
use crate::coach::judgement::{Eval, MoveClass};
use crate::coach::review::{AnalysedPosition, MoveReview, summarise};
use crate::coach::store::{GameId, GameRecord};
use crate::game_runner::position_outcome;
use crate::search::Score;
use crate::time::TimeBudget;
use crate::types::Color;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use std::time::Instant;

/// Engine time per position when analysing a game.
const ANALYSIS_MS: u64 = 300;
const SUMMARY_HEIGHT: u16 = 8;

const KEYS: &[(&str, &str)] = &[
    ("← →  h l", "step through the game"),
    ("↓ ↑  j k", "your next / previous mistake"),
    ("Home End", "start / end of the game"),
    ("b", "show the better move on the board"),
    ("f", "flip the board"),
    ("p", "pieces as pictures, symbols or letters"),
    ("Esc", "back"),
];

pub struct ReviewScreen {
    record: GameRecord,
    /// The position after each number of moves, from the start to the end.
    positions: Vec<Board>,
    analysis: Vec<AnalysedPosition>,
    reviews: Vec<MoveReview>,
    habits: Vec<HabitNote>,
    /// How many moves have been played on the board shown.
    cursor: usize,
    show_best: bool,
    flipped: bool,
    saved: Option<Result<(), String>>,
    help: bool,
}

impl ReviewScreen {
    pub fn new(id: GameId, data: &CoachData) -> Self {
        let game = data.game(id);
        let record = game.map(|g| g.record.clone());
        let habits = game.map(|g| g.habits.clone()).unwrap_or_default();
        let record = record.unwrap_or_else(|| empty_record(id));
        let mut board = record.start().unwrap_or_else(|_| Board::empty());
        let mut positions = vec![board.clone()];
        for &m in &record.moves {
            board.make_move(m);
            positions.push(board.clone());
        }
        let analysis = record.analysis.clone();
        let mut screen = Self {
            flipped: record.human == Color::Black,
            positions,
            analysis,
            reviews: Vec::new(),
            habits,
            cursor: 0,
            show_best: false,
            saved: None,
            help: false,
            record,
        };
        screen.refresh_reviews();
        screen
    }

    fn refresh_reviews(&mut self) {
        let Some(start) = self.positions.first() else {
            return;
        };
        self.reviews =
            crate::coach::review::review_moves(start, &self.record.moves, &self.analysis, &super::data::is_book);
    }

    fn total_positions(&self) -> usize {
        self.positions.len()
    }

    fn is_analysed(&self) -> bool {
        self.analysis.len() >= self.total_positions()
    }

    fn review_at(&self, ply: usize) -> Option<&MoveReview> {
        self.reviews.iter().find(|r| r.ply == ply)
    }

    /// The review of the move that led to the board shown.
    fn current_review(&self) -> Option<&MoveReview> {
        self.cursor.checked_sub(1).and_then(|ply| self.review_at(ply))
    }

    fn your_errors(&self) -> impl Iterator<Item = &MoveReview> {
        self.reviews.iter().filter(|r| r.mover == self.record.human && r.class.is_error())
    }

    fn jump_to_error(&mut self, forward: bool) {
        let target = if forward {
            self.your_errors().map(|r| r.ply + 1).find(|&c| c > self.cursor)
        } else {
            self.your_errors().map(|r| r.ply + 1).filter(|&c| c < self.cursor).last()
        };
        if let Some(cursor) = target {
            self.cursor = cursor;
            self.show_best = false;
        }
    }

    fn summary_lines(&self) -> Vec<Line<'static>> {
        let you = summarise(&self.reviews, self.record.human);
        let coach = summarise(&self.reviews, self.record.human.other());
        let accuracy = |a: Option<f64>| a.map_or_else(|| "–".to_string(), |a| format!("{a:.0}%"));
        let outcome = super::games_list::outcome_word(self.record.outcome());
        let mut lines = vec![
            Line::from(vec![
                Span::styled(format!("{outcome} "), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!(
                        "as {} against level {} by {}",
                        color_name(self.record.human),
                        self.record.level.number(),
                        self.record.ending.describe()
                    ),
                    Style::new().fg(DIM),
                ),
            ]),
            Line::from(vec![
                Span::styled("Accuracy   ", Style::new().fg(DIM)),
                Span::styled(format!("you {:<6}", accuracy(you.accuracy)), Style::new().fg(TEXT)),
                Span::styled(format!("coach {}", accuracy(coach.accuracy)), Style::new().fg(DIM)),
            ]),
            Line::from(vec![
                Span::styled("Your errors ", Style::new().fg(DIM)),
                Span::styled(format!("?! {}  ", you.inaccuracies), Style::new().fg(class_color(MoveClass::Inaccuracy))),
                Span::styled(format!("? {}  ", you.mistakes), Style::new().fg(class_color(MoveClass::Mistake))),
                Span::styled(format!("?? {}", you.blunders), Style::new().fg(class_color(MoveClass::Blunder))),
            ]),
        ];
        if self.is_analysed() {
            lines.push(Line::styled(self.graph(40), Style::new().fg(INFO)));
        } else {
            let done = self.analysis.len();
            let total = self.total_positions();
            lines.push(Line::from(vec![
                Span::styled("Analysing  ", Style::new().fg(ACCENT)),
                Span::styled(bar(done as f64 / total as f64, 20), Style::new().fg(ACCENT)),
                Span::styled(format!(" {done}/{total}"), Style::new().fg(DIM)),
            ]));
        }
        if let Some(Err(e)) = &self.saved {
            lines.push(colored(e.clone(), BAD));
        }
        lines
    }

    /// White's winning chances across the game.
    fn graph(&self, width: usize) -> String {
        let chances: Vec<f64> = self
            .analysis
            .iter()
            .zip(&self.positions)
            .map(|(a, board)| a.eval.win_chance_for(board.turn, Color::White).percent())
            .collect();
        sparkline(&chances, width)
    }

    fn detail_lines(&self, data: &CoachData) -> Vec<Line<'static>> {
        if self.cursor == 0 {
            return self.overview_lines(data);
        }
        let Some(review) = self.current_review() else {
            return vec![dim("Waiting for the engine…")];
        };
        let yours = review.mover == self.record.human;
        let who = if yours { "You" } else { "The coach" };
        let symbol = review.class.symbol();
        let mut lines = vec![Line::from(vec![
            Span::styled(format!("{}{symbol}  ", review.numbered_san), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
            Span::styled(review.class.name(), Style::new().fg(class_color(review.class)).add_modifier(Modifier::BOLD)),
            Span::styled(format!("   {who}"), Style::new().fg(DIM)),
        ])];
        if let Some(analysis) = self.analysis.get(self.cursor) {
            let board = &self.positions[self.cursor];
            lines.push(dim(format!(
                "Evaluation {}   ·   win chance {:.0}% → {:.0}%",
                analysis.eval.text_for_white(board.turn),
                review.before.percent(),
                review.after.percent()
            )));
        }
        if let Some(cause) = &review.cause {
            lines.push(Line::raw(""));
            lines.push(Line::styled(cause.explain(), Style::new().fg(if yours { WARN } else { GOOD })));
        }
        if let Some(best) = &review.best_san
            && review.class != MoveClass::Book
        {
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("Better was ", Style::new().fg(DIM)),
                Span::styled(best.clone(), Style::new().fg(GOOD).add_modifier(Modifier::BOLD)),
                Span::styled(if self.show_best { "  (shown)" } else { "  (b to show)" }, Style::new().fg(DIM)),
            ]));
            lines.push(dim(format!("Line: {}", review.best_line)));
        }
        if yours && review.class.is_serious() && review.best.is_some() {
            lines.push(colored("Saved as a puzzle for you to practise.", INFO));
        }
        lines
    }

    fn overview_lines(&self, data: &CoachData) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if !self.is_analysed() {
            lines.push(dim("The engine is going through the game. You can step through it already."));
            lines.push(Line::raw(""));
        }
        if let Some(game) = data.game(self.record.id)
            && let Some(lesson) = game.main_lesson()
        {
            lines.push(heading(format!("Lesson: {}", lesson.name())));
            lines.push(Line::styled(lesson.advice().to_string(), Style::new().fg(TEXT)));
            lines.push(Line::raw(""));
        } else if self.is_analysed() && self.your_errors().all(|r| !r.class.is_serious()) {
            lines.push(colored("No mistakes or blunders: a clean game.", GOOD));
            lines.push(Line::raw(""));
        }
        for note in &self.habits {
            lines.push(heading(format!("Opening habit: {}", note.habit.name())));
            lines.push(Line::styled(note.habit.advice().to_string(), Style::new().fg(TEXT)));
            lines.push(Line::raw(""));
        }
        lines.push(dim("→ step through the game · ↓ jump to your next mistake"));
        lines
    }

    fn move_list(&self, rows: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let mut current_row = 0;
        for (row, pair) in self.record.moves.chunks(2).enumerate() {
            let mut spans = vec![Span::styled(format!("{:>3}. ", row + 1), Style::new().fg(DIM))];
            for (i, _) in pair.iter().enumerate() {
                let ply = row * 2 + i;
                let review = self.review_at(ply);
                let text = review.map_or_else(|| "…".to_string(), |r| format!("{}{}", r.san, r.class.symbol()));
                let color = review.map_or(DIM, |r| if r.class.is_error() { class_color(r.class) } else { TEXT });
                let mut style = Style::new().fg(color);
                if ply + 1 == self.cursor {
                    style = style.add_modifier(Modifier::REVERSED);
                    current_row = row;
                }
                spans.push(Span::styled(format!("{text:<9}"), style));
            }
            lines.push(Line::from(spans));
        }
        let skip = (current_row + 1).saturating_sub(rows).min(lines.len().saturating_sub(rows));
        lines.drain(..skip.min(lines.len()));
        lines
    }

    fn scene(&self, data: &CoachData) -> BoardScene<'_> {
        let review = self.current_review();
        if self.show_best
            && let Some(review) = review
            && let Some(best) = review.best
        {
            let mut scene = BoardScene::plain(&self.positions[review.ply], self.flipped, data.piece_style);
            scene.marks = move_marks(best, MARK_GOOD);
            return scene;
        }
        let mut scene = BoardScene::plain(&self.positions[self.cursor], self.flipped, data.piece_style);
        scene.last_move = self.cursor.checked_sub(1).and_then(|ply| self.record.moves.get(ply).copied());
        if let (Some(review), Some(m)) = (review, scene.last_move)
            && let Some(color) = class_mark(review.class)
        {
            scene.marks = move_marks(m, color);
        }
        scene
    }
}

fn empty_record(id: GameId) -> GameRecord {
    GameRecord {
        id,
        played_at: crate::coach::srs::UnixTime(0),
        human: Color::White,
        level: crate::coach::bot::Level::WEAKEST,
        result: crate::game_runner::GameResult::Draw,
        ending: crate::coach::store::Ending::Resigned,
        blunder_check: false,
        start_fen: crate::types::START_FEN.to_string(),
        moves: Vec::new(),
        analysis: Vec::new(),
    }
}

const fn color_name(color: Color) -> &'static str {
    match color {
        Color::White => "White",
        Color::Black => "Black",
    }
}

impl Screen for ReviewScreen {
    fn draw(&self, frame: &mut Frame, data: &CoachData, _now: Instant) {
        let layout = board_layout(frame.area(), true);
        let scene = self.scene(data);
        draw_board(frame, &layout, &scene);
        if let Some(bar_area) = layout.eval_bar {
            let shown = if self.show_best { self.cursor.saturating_sub(1) } else { self.cursor };
            let share = self.analysis.get(shown).map_or(0.5, |a| {
                let turn = self.positions[shown].turn;
                match a.eval {
                    Eval::Checkmated => if turn == Color::White { 0.0 } else { 1.0 },
                    Eval::Score(score) => {
                        let white = match (score, turn) {
                            (s, Color::White) => s,
                            (Score::Centipawns(cp), Color::Black) => Score::Centipawns(-cp),
                            (Score::MateIn(n), Color::Black) => Score::MateIn(-n),
                        };
                        white_share(white)
                    }
                }
            });
            draw_eval_bar(frame.buffer_mut(), share, self.flipped, bar_area);
        }
        let side = layout.side;
        let summary_height = SUMMARY_HEIGHT.min(side.height);
        frame.render_widget(
            Paragraph::new(self.summary_lines()).wrap(Wrap { trim: false }).block(panel("Game review")),
            Rect::new(side.x, side.y, side.width, summary_height),
        );
        let rest = side.height.saturating_sub(summary_height);
        let detail = self.detail_lines(data);
        let detail_height = (detail.len() as u16 + 4).min(rest.saturating_sub(5).max(rest / 2)).min(rest);
        let title = if self.cursor == 0 { "Coach" } else { "This move" };
        frame.render_widget(
            Paragraph::new(detail).wrap(Wrap { trim: false }).block(panel(title)),
            Rect::new(side.x, side.y + summary_height, side.width, detail_height),
        );
        let moves_height = rest.saturating_sub(detail_height);
        if moves_height >= 3 {
            let area = Rect::new(side.x, side.y + summary_height + detail_height, side.width, moves_height);
            frame.render_widget(
                Paragraph::new(self.move_list(usize::from(moves_height - 2))).block(panel("Moves")),
                area,
            );
        }
        draw_status(
            frame,
            layout.status,
            None,
            None,
            "←→ step · ↓↑ your mistakes · b better move · f flip · ? help · Esc back",
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
        let last = self.record.moves.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Nav::Back,
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                self.cursor = (self.cursor + 1).min(last);
                self.show_best = false;
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.cursor = self.cursor.saturating_sub(1);
                self.show_best = false;
            }
            KeyCode::Down | KeyCode::Char('j') => self.jump_to_error(true),
            KeyCode::Up | KeyCode::Char('k') => self.jump_to_error(false),
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.cursor = last,
            KeyCode::Char('b') => self.show_best = !self.show_best && self.current_review().is_some_and(|r| r.best.is_some()),
            KeyCode::Char('f') => self.flipped = !self.flipped,
            KeyCode::Char('p') => data.piece_style = data.piece_style.next(),
            KeyCode::Char('?') => self.help = true,
            _ => {}
        }
        Nav::Stay
    }

    fn job(&self, _data: &CoachData) -> Option<Job> {
        let next = self.analysis.len();
        let board = self.positions.get(next)?;
        if position_outcome(board).is_some() {
            return None;
        }
        Some(Job {
            tag: next as u64,
            kind: JobKind::Search(SearchLimit::Time(Some(TimeBudget::fixed(ANALYSIS_MS)))),
            board: board.clone(),
        })
    }

    fn engine(&mut self, tag: u64, result: JobResult, _data: &mut CoachData, _now: Instant) -> Nav {
        let next = self.analysis.len();
        if let JobResult::Done(outcome) = result
            && tag == next as u64
            && let Some(board) = self.positions.get(next)
        {
            let eval = eval_after_search(board, outcome.score);
            self.analysis.push(AnalysedPosition { eval, best_line: outcome.pv });
            self.refresh_reviews();
        }
        Nav::Stay
    }

    fn tick(&mut self, data: &mut CoachData, _now: Instant) -> Nav {
        let next = self.analysis.len();
        if let Some(board) = self.positions.get(next)
            && position_outcome(board).is_some()
        {
            self.analysis.push(AnalysedPosition { eval: eval_after_search(board, None), best_line: Vec::new() });
            self.refresh_reviews();
        }
        if self.is_analysed() && self.saved.is_none() && self.record.analysis.len() < self.analysis.len() {
            let result = data
                .save_analysis(self.record.id, &self.analysis)
                .map_err(|e| format!("Could not save the analysis: {e}"));
            self.saved = Some(result);
            self.record.analysis = self.analysis.clone();
        }
        Nav::Stay
    }
}
