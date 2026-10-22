//! Going through a finished game with the coach: every move judged, mistakes explained.

use super::super::engine::SearchLimit;
use super::super::view::{ACCENT, BoardScene, DIM, board_layout, draw_eval_bar, white_share};
use super::common::{
    BAD, GOOD, INFO, MARK_GOOD, MARK_HINT, TEXT, WARN, bar, class_color, class_mark, colored, dim,
    draw_board, draw_help, draw_status, heading, move_marks, panel, sparkline,
};
use super::data::CoachData;
use super::game::eval_after_search;
use super::{Job, JobKind, JobResult, Nav, Screen};
use crate::board::Board;
use crate::coach::explain::{LineStory, line_story, positional_differences};
use crate::coach::habits::HabitNote;
use crate::coach::judgement::{Eval, MoveClass};
use crate::coach::review::{AnalysedPosition, MoveReview, summarise};
use crate::coach::store::{GameId, GameRecord};
use crate::game_runner::position_outcome;
use crate::notation::{numbered, san_of};
use crate::search::Score;
use crate::time::TimeBudget;
use crate::types::{Color, Move};
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
    ("t", "play through what happens after the move"),
    ("b", "play through the better move's line"),
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
    /// A line being played through on the board, if any.
    explore: Option<Explore>,
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
            explore: None,
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
        self.reviews = crate::coach::review::review_moves(
            start,
            &self.record.moves,
            &self.analysis,
            &super::data::is_book,
        );
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
        self.cursor
            .checked_sub(1)
            .and_then(|ply| self.review_at(ply))
    }

    fn your_errors(&self) -> impl Iterator<Item = &MoveReview> {
        self.reviews
            .iter()
            .filter(|r| r.mover == self.record.human && r.class.is_error())
    }

    fn jump_to_error(&mut self, forward: bool) {
        let target = if forward {
            self.your_errors()
                .map(|r| r.ply + 1)
                .find(|&c| c > self.cursor)
        } else {
            self.your_errors()
                .map(|r| r.ply + 1)
                .filter(|&c| c < self.cursor)
                .last()
        };
        if let Some(cursor) = target {
            self.cursor = cursor;
            self.explore = None;
        }
    }

    fn summary_lines(&self) -> Vec<Line<'static>> {
        let you = summarise(&self.reviews, self.record.human);
        let coach = summarise(&self.reviews, self.record.human.other());
        let accuracy = |a: Option<f64>| a.map_or_else(|| "–".to_string(), |a| format!("{a:.0}%"));
        let outcome = super::games_list::outcome_word(self.record.outcome());
        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    format!("{outcome} "),
                    Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
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
                Span::styled(
                    format!("you {:<6}", accuracy(you.accuracy)),
                    Style::new().fg(TEXT),
                ),
                Span::styled(
                    format!("coach {}", accuracy(coach.accuracy)),
                    Style::new().fg(DIM),
                ),
            ]),
            Line::from(vec![
                Span::styled("Your errors ", Style::new().fg(DIM)),
                Span::styled(
                    format!("?! {}  ", you.inaccuracies),
                    Style::new().fg(class_color(MoveClass::Inaccuracy)),
                ),
                Span::styled(
                    format!("? {}  ", you.mistakes),
                    Style::new().fg(class_color(MoveClass::Mistake)),
                ),
                Span::styled(
                    format!("?? {}", you.blunders),
                    Style::new().fg(class_color(MoveClass::Blunder)),
                ),
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
        if let Some(explore) = &self.explore {
            return explore.lines();
        }
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
            Span::styled(
                format!("{}{symbol}  ", review.numbered_san),
                Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                review.class.name(),
                Style::new()
                    .fg(class_color(review.class))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("   {who}"), Style::new().fg(DIM)),
        ])];
        if let (Some(before), Some(after)) = (
            self.analysis.get(review.ply),
            self.analysis.get(self.cursor),
        ) {
            lines.push(dim(format!(
                "Evaluation {} → {}   ·   win chance {:.0}% → {:.0}%",
                before.eval.text_for_white(self.positions[review.ply].turn),
                after.eval.text_for_white(self.positions[self.cursor].turn),
                review.before.percent(),
                review.after.percent()
            )));
        }
        if let Some(cause) = &review.cause {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                cause.explain(),
                Style::new().fg(if yours { WARN } else { GOOD }),
            ));
        }
        if !review.class.is_error() || review.class == MoveClass::Book {
            return lines;
        }
        let mover = review.mover;
        if let Some(story) = self.story(Branch::WhatHappens, mover) {
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "What happens next",
                    Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  (t to play it through)", Style::new().fg(DIM)),
            ]));
            lines.push(Line::styled(story.moves, Style::new().fg(TEXT)));
            lines.push(colored(
                format!("→ {}", story.outcome),
                if yours { WARN } else { GOOD },
            ));
        }
        if let (Some(best), Some(story)) = (&review.best_san, self.story(Branch::Better, mover)) {
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "Better was ",
                    Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    best.clone(),
                    Style::new().fg(GOOD).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  (b to play it through)", Style::new().fg(DIM)),
            ]));
            lines.push(Line::styled(story.moves, Style::new().fg(TEXT)));
            lines.push(colored(format!("→ {}", story.outcome), GOOD));
        }
        let notes = review
            .best
            .map(|best| {
                positional_differences(
                    &self.positions[review.ply],
                    review.played,
                    best,
                    review.phase,
                )
            })
            .unwrap_or_default();
        if !notes.is_empty() {
            lines.push(Line::raw(""));
            lines.push(heading("Compared with the better move"));
            for note in notes {
                lines.push(Line::from(vec![
                    Span::styled("• ", Style::new().fg(INFO)),
                    Span::styled(note, Style::new().fg(TEXT)),
                ]));
            }
        }
        if yours && review.class.is_serious() && review.best.is_some() {
            lines.push(Line::raw(""));
            lines.push(colored("Saved as a puzzle for you to practise.", INFO));
        }
        lines
    }

    /// The engine line for `branch` of the move shown: the board it starts from and its moves.
    fn branch_line(&self, branch: Branch) -> Option<(&Board, &[Move])> {
        let ply = self.cursor.checked_sub(1)?;
        let review = self.review_at(ply)?;
        let start = match branch {
            Branch::WhatHappens => ply + 1,
            Branch::Better if review.best.is_some() => ply,
            Branch::Better => return None,
        };
        let line = &self.analysis.get(start)?.best_line;
        (!line.is_empty()).then(|| (&self.positions[start], line.as_slice()))
    }

    fn story(&self, branch: Branch, mover: Color) -> Option<LineStory> {
        let (board, line) = self.branch_line(branch)?;
        line_story(board, line, mover)
    }

    fn start_explore(&mut self, branch: Branch) {
        self.explore = self
            .branch_line(branch)
            .map(|(board, line)| Explore::new(branch, board, line));
    }

    fn overview_lines(&self, data: &CoachData) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if !self.is_analysed() {
            lines.push(dim(
                "The engine is going through the game. You can step through it already.",
            ));
            lines.push(Line::raw(""));
        }
        if let Some(game) = data.game(self.record.id)
            && let Some(lesson) = game.main_lesson()
        {
            lines.push(heading(format!("Lesson: {}", lesson.name())));
            lines.push(Line::styled(
                lesson.advice().to_string(),
                Style::new().fg(TEXT),
            ));
            lines.push(Line::raw(""));
        } else if self.is_analysed() && self.your_errors().all(|r| !r.class.is_serious()) {
            lines.push(colored("No mistakes or blunders: a clean game.", GOOD));
            lines.push(Line::raw(""));
        }
        for note in &self.habits {
            lines.push(heading(format!("Opening habit: {}", note.habit.name())));
            lines.push(Line::styled(
                note.habit.advice().to_string(),
                Style::new().fg(TEXT),
            ));
            lines.push(Line::raw(""));
        }
        lines.push(dim("→ step through the game · ↓ jump to your next mistake"));
        lines
    }

    fn move_list(&self, rows: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let mut current_row = 0;
        for (row, pair) in self.record.moves.chunks(2).enumerate() {
            let mut spans = vec![Span::styled(
                format!("{:>3}. ", row + 1),
                Style::new().fg(DIM),
            )];
            for (i, _) in pair.iter().enumerate() {
                let ply = row * 2 + i;
                let review = self.review_at(ply);
                let text = review.map_or_else(
                    || "…".to_string(),
                    |r| format!("{}{}", r.san, r.class.symbol()),
                );
                let color = review.map_or(DIM, |r| {
                    if r.class.is_error() {
                        class_color(r.class)
                    } else {
                        TEXT
                    }
                });
                let mut style = Style::new().fg(color);
                if ply + 1 == self.cursor {
                    style = style.add_modifier(Modifier::REVERSED);
                    current_row = row;
                }
                spans.push(Span::styled(format!("{text:<9}"), style));
            }
            lines.push(Line::from(spans));
        }
        let skip = (current_row + 1)
            .saturating_sub(rows)
            .min(lines.len().saturating_sub(rows));
        lines.drain(..skip.min(lines.len()));
        lines
    }

    fn scene(&self, data: &CoachData) -> BoardScene<'_> {
        if let Some(explore) = &self.explore {
            let mut scene = BoardScene::plain(
                &explore.boards[explore.step],
                self.flipped,
                data.piece_style,
            );
            if let Some(&m) = explore
                .step
                .checked_sub(1)
                .and_then(|i| explore.moves.get(i))
            {
                scene.marks = move_marks(m, explore.branch.mark());
            }
            return scene;
        }
        let review = self.current_review();
        let mut scene =
            BoardScene::plain(&self.positions[self.cursor], self.flipped, data.piece_style);
        scene.last_move = self
            .cursor
            .checked_sub(1)
            .and_then(|ply| self.record.moves.get(ply).copied());
        if let (Some(review), Some(m)) = (review, scene.last_move)
            && let Some(color) = class_mark(review.class)
        {
            scene.marks = move_marks(m, color);
        }
        scene
    }
}

/// Which engine line of a move to play through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Branch {
    /// The engine's line after the move played.
    WhatHappens,
    /// The engine's line from its better move.
    Better,
}

impl Branch {
    const fn title(self) -> &'static str {
        match self {
            Self::WhatHappens => "What happens next",
            Self::Better => "The better line",
        }
    }

    const fn mark(self) -> ratatui::style::Color {
        match self {
            Self::WhatHappens => MARK_HINT,
            Self::Better => MARK_GOOD,
        }
    }
}

/// A line played through move by move on the board.
struct Explore {
    branch: Branch,
    /// The position before each move of the line, then the one after the last.
    boards: Vec<Board>,
    moves: Vec<Move>,
    /// Moves of the line played on the board shown.
    step: usize,
}

impl Explore {
    fn new(branch: Branch, start: &Board, line: &[Move]) -> Self {
        let mut board = start.clone();
        let mut boards = vec![board.clone()];
        let mut moves = Vec::new();
        for &m in line {
            if san_of(&board, m).is_none() {
                break;
            }
            board.make_move(m);
            boards.push(board.clone());
            moves.push(m);
        }
        Self {
            branch,
            boards,
            moves,
            step: 1.min(line.len()),
        }
    }

    fn step(&mut self, forward: bool) {
        self.step = if forward {
            (self.step + 1).min(self.moves.len())
        } else {
            self.step.saturating_sub(1)
        };
    }

    fn lines(&self) -> Vec<Line<'static>> {
        let mut spans = Vec::new();
        for (i, &m) in self.moves.iter().enumerate() {
            let board = &self.boards[i];
            let san = san_of(board, m).unwrap_or_default();
            let style = if i + 1 == self.step {
                Style::new()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else if i < self.step {
                Style::new().fg(TEXT)
            } else {
                Style::new().fg(DIM)
            };
            spans.push(Span::styled(numbered(board, &san, i == 0), style));
            spans.push(Span::raw(" "));
        }
        vec![
            Line::from(spans),
            Line::raw(""),
            dim(format!("Move {} of {}", self.step, self.moves.len())),
            dim("←→ step through · t / b other line · Esc back to the game"),
        ]
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
            let shown = self.cursor;
            let share = self.analysis.get(shown).map_or(0.5, |a| {
                let turn = self.positions[shown].turn;
                match a.eval {
                    Eval::Checkmated => {
                        if turn == Color::White {
                            0.0
                        } else {
                            1.0
                        }
                    }
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
            Paragraph::new(self.summary_lines())
                .wrap(Wrap { trim: false })
                .block(panel("Game review")),
            Rect::new(side.x, side.y, side.width, summary_height),
        );
        let rest = side.height.saturating_sub(summary_height);
        let detail = self.detail_lines(data);
        let detail_height = (detail.len() as u16 + 4)
            .min(rest.saturating_sub(5).max(rest / 2))
            .min(rest);
        let title = match &self.explore {
            Some(explore) => explore.branch.title(),
            None if self.cursor == 0 => "Coach",
            None => "This move",
        };
        frame.render_widget(
            Paragraph::new(detail)
                .wrap(Wrap { trim: false })
                .block(panel(title)),
            Rect::new(side.x, side.y + summary_height, side.width, detail_height),
        );
        let moves_height = rest.saturating_sub(detail_height);
        if moves_height >= 3 {
            let area = Rect::new(
                side.x,
                side.y + summary_height + detail_height,
                side.width,
                moves_height,
            );
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
            "←→ step · ↓↑ your mistakes · t what happens · b better line · f flip · ? help · Esc back",
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
        if let Some(explore) = self.explore.as_mut() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.explore = None,
                KeyCode::Right | KeyCode::Char('l' | ' ') => explore.step(true),
                KeyCode::Left | KeyCode::Char('h') => explore.step(false),
                KeyCode::Char('t') => self.start_explore(Branch::WhatHappens),
                KeyCode::Char('b') => self.start_explore(Branch::Better),
                KeyCode::Char('f') => self.flipped = !self.flipped,
                _ => {}
            }
            return Nav::Stay;
        }
        let last = self.record.moves.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Nav::Back,
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                self.cursor = (self.cursor + 1).min(last);
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.cursor = self.cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => self.jump_to_error(true),
            KeyCode::Up | KeyCode::Char('k') => self.jump_to_error(false),
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.cursor = last,
            KeyCode::Char('t') => self.start_explore(Branch::WhatHappens),
            KeyCode::Char('b') => self.start_explore(Branch::Better),
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
            self.analysis.push(AnalysedPosition {
                eval,
                best_line: outcome.pv,
            });
            self.refresh_reviews();
        }
        Nav::Stay
    }

    fn tick(&mut self, data: &mut CoachData, _now: Instant) -> Nav {
        let next = self.analysis.len();
        if let Some(board) = self.positions.get(next)
            && position_outcome(board).is_some()
        {
            self.analysis.push(AnalysedPosition {
                eval: eval_after_search(board, None),
                best_line: Vec::new(),
            });
            self.refresh_reviews();
        }
        if self.is_analysed()
            && self.saved.is_none()
            && self.record.analysis.len() < self.analysis.len()
        {
            let result = data
                .save_analysis(self.record.id, &self.analysis)
                .map_err(|e| format!("Could not save the analysis: {e}"));
            self.saved = Some(result);
            self.record.analysis = self.analysis.clone();
        }
        Nav::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::SearchOutcome;
    use crate::coach::bot::Level;
    use crate::coach::srs::UnixTime;
    use crate::coach::store::{Ending, Store};
    use crate::game_runner::GameResult;
    use crate::notation::parse_san_line;
    use crate::types::START_FEN;

    /// Fool's mate, reviewed with the engine's lines filled in by hand.
    fn reviewed_fools_mate() -> (ReviewScreen, CoachData) {
        let dir = std::env::temp_dir().join(format!("chess-review-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut data = CoachData::load(Store::open(dir).unwrap()).unwrap();
        let start = Board::from_fen(START_FEN).unwrap();
        let id = GameId(7);
        data.add_game(GameRecord {
            id,
            played_at: UnixTime(1_700_000_000),
            human: Color::White,
            level: Level::WEAKEST,
            result: GameResult::BlackWin,
            ending: Ending::Checkmate,
            blunder_check: false,
            start_fen: START_FEN.to_string(),
            moves: parse_san_line(&start, &["f3", "e5", "g4", "Qh4#"]).unwrap(),
            analysis: Vec::new(),
        })
        .unwrap();
        let mut screen = ReviewScreen::new(id, &data);
        let lines: [(Score, &[&str]); 4] = [
            (Score::Centipawns(20), &["e4", "e5"]),
            (Score::Centipawns(-10), &["e5"]),
            (Score::Centipawns(-60), &["e4", "Nc6"]),
            (Score::MateIn(1), &["Qh4#"]),
        ];
        while let Some(job) = screen.job(&data) {
            let (score, line) = lines[job.tag as usize];
            let pv = parse_san_line(&job.board, line).unwrap();
            let outcome = SearchOutcome {
                best_move: pv.first().copied(),
                score: Some(score),
                pv,
            };
            screen.engine(job.tag, JobResult::Done(outcome), &mut data, Instant::now());
            screen.tick(&mut data, Instant::now());
        }
        (screen, data)
    }

    fn text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }

    #[test]
    fn a_blunder_shows_what_happens_and_the_better_line() {
        let (mut screen, data) = reviewed_fools_mate();
        screen.cursor = 3;

        let shown = text(&screen.detail_lines(&data));

        assert!(shown.contains("What happens next"), "{shown}");
        assert!(
            shown.contains("2... Qh4#") && shown.contains("White is checkmated"),
            "{shown}"
        );
        assert!(
            shown.contains("Better was e4") && shown.contains("2. e4 Nc6"),
            "{shown}"
        );
    }

    #[test]
    fn playing_through_a_line_steps_the_board_along_it() {
        let (mut screen, _data) = reviewed_fools_mate();
        screen.cursor = 3;

        screen.start_explore(Branch::Better);
        let first = screen.explore.as_ref().map(|e| (e.step, e.moves.len()));
        if let Some(explore) = screen.explore.as_mut() {
            explore.step(true);
            explore.step(true);
        }

        assert_eq!(first, Some((1, 2)));
        assert_eq!(screen.explore.as_ref().map(|e| e.step), Some(2));
    }
}
