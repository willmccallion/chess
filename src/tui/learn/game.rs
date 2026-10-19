//! A game against the coach's bot, with an optional check that stops you before a blunder.

use super::common::{
    BAD, GOOD, MARK_BAD, TEXT, WARN, colored, dim, draw_board, draw_help, draw_status, heading, move_marks, panel,
    screen_layout,
};
use super::data::{CoachData, new_game_id, now};
use super::review::ReviewScreen;
use super::super::app::{App, Mode, PlayConfig, TimeSetting};
use super::super::engine::SearchLimit;
use super::super::view::{ACCENT, BoardScene, DIM, format_clock};
use super::{Job, JobKind, JobResult, Nav, Screen};
use crate::board::Board;
use crate::coach::bot::{Level, choose_move, pick_weighted};
use crate::coach::explain::{LineStory, line_story};
use crate::coach::judgement::Eval;
use crate::coach::rating::Elo;
use crate::coach::repertoire::{CHAPTERS, lines_continuing};
use crate::coach::review::{AnalysedPosition, review_moves};
use crate::coach::srs::UnixTime;
use crate::coach::store::{Ending, GameId, GameRecord};
use crate::game_runner::{GameClock, GameResult, position_outcome, win_for};
use crate::opening_book::legal_book_moves;
use crate::search::Score;
use crate::time::TimeBudget;
use crate::types::{Color, Move, PieceKind, START_FEN};
use rand::Rng;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use std::time::{Duration, Instant};

/// The bot never answers faster than this, so its moves can be followed.
const BOT_MIN_THINK: Duration = Duration::from_millis(600);
/// Extra random thinking time, up to this, so the bot's pace feels human.
const BOT_EXTRA_THINK_MS: u64 = 900;
/// Search time for each blunder-check evaluation.
const GUARD_SEARCH_MS: u64 = 400;
/// Tags for the blunder check's searches, kept apart from the bot's (which use the ply).
const BASELINE_TAG: u64 = 1 << 40;
const PROPOSAL_TAG: u64 = 1 << 41;

const KEYS: &[(&str, &str)] = &[
    ("click / drag", "move a piece"),
    ("Enter", "type a move (e2e4, Nf3, O-O)"),
    ("q r b n", "choose a promotion piece"),
    ("f", "flip the board"),
    ("p", "pieces as pictures, symbols or letters"),
    ("r", "resign"),
    ("Esc", "leave the game"),
];

/// How the game is set up.
#[derive(Clone, Copy, Debug)]
pub struct GameSettings {
    pub level: Level,
    pub human: Color,
    pub clock: Option<GameClock>,
    pub blunder_check: bool,
    /// The repertoire chapter the coach steers the opening towards.
    pub opening: Option<&'static str>,
}

/// The bot's move for the current turn, chosen now or still being worked out.
struct BotTurn {
    ply: usize,
    ready_at: Instant,
    chosen: Option<Move>,
}

/// A move the blunder check stopped, with why.
struct Warning {
    san: String,
    verdict: String,
    explanation: String,
    /// How the opponent punishes the move.
    punishment: Option<LineStory>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Resign,
    Leave,
}

struct Finished {
    result: GameResult,
    ending: Ending,
    saved: Result<(), String>,
}

pub struct CoachGame {
    app: App,
    settings: GameSettings,
    id: GameId,
    started: UnixTime,
    rating_before: Elo,
    bot: Option<BotTurn>,
    /// The engine's view of the position on your turn, for judging your move.
    baseline: Option<(usize, AnalysedPosition)>,
    warning: Option<Warning>,
    finished: Option<Finished>,
    confirm: Option<Confirm>,
    message: Option<String>,
    help: bool,
}

/// The engine's verdict on `board` from a finished search.
pub fn eval_after_search(board: &Board, score: Option<Score>) -> Eval {
    match position_outcome(board) {
        Some((GameResult::Draw, _)) => Eval::DRAWN,
        Some(_) => Eval::Checkmated,
        None => Eval::Score(score.unwrap_or(Score::Centipawns(0))),
    }
}

impl CoachGame {
    pub fn new(settings: GameSettings, data: &CoachData) -> Self {
        let start = Board::from_fen(START_FEN).unwrap_or_else(|_| Board::empty());
        let config = PlayConfig {
            start,
            mode: Mode::Game,
            human: settings.human,
            time: settings.clock.map_or(TimeSetting::PerMove { ms: 0 }, TimeSetting::Clock),
        };
        let mut app = App::new(config, Instant::now());
        app.set_hold_moves(settings.blunder_check);
        app.piece_style = data.piece_style;
        Self {
            app,
            settings,
            id: new_game_id(),
            started: now(),
            rating_before: data.profile.rating,
            bot: None,
            baseline: None,
            warning: None,
            finished: None,
            confirm: None,
            message: None,
            help: false,
        }
    }

    fn ply(&self) -> usize {
        self.app.history().len()
    }

    fn is_over(&self) -> bool {
        self.finished.is_some() || self.app.outcome().is_some()
    }

    fn is_bot_turn(&self) -> bool {
        !self.is_over() && self.app.board().turn != self.settings.human
    }

    fn moves(&self) -> Vec<Move> {
        self.app.history().iter().map(|&(m, _)| m).collect()
    }

    /// The bot's search size, cut down when its clock runs low.
    fn bot_nodes(&self) -> u64 {
        let bot = self.settings.human.other();
        let remaining = self.app.clocks().map(|c| c.remaining_ms(bot, self.app.board().turn, Instant::now()));
        self.settings.level.spec().nodes_for_clock(remaining)
    }

    fn baseline_for_now(&self) -> Option<&AnalysedPosition> {
        self.baseline.as_ref().filter(|(ply, _)| *ply == self.ply()).map(|(_, a)| a)
    }

    fn guard_search(&self, tag: u64, board: Board) -> Job {
        Job {
            tag,
            kind: JobKind::Search(SearchLimit::Time(Some(TimeBudget::fixed(GUARD_SEARCH_MS)))),
            board,
        }
    }

    /// A book or repertoire move for the bot, when the opening allows one.
    fn opening_move(&self, rng: &mut impl Rng) -> Option<Move> {
        if let Some(chapter) = self.settings.opening.and_then(|name| CHAPTERS.iter().find(|c| c.name == name)) {
            let options: Vec<Move> = lines_continuing(&self.moves())
                .into_iter()
                .filter(|(line, _)| chapter.lines.iter().any(|l| l.id == line.id))
                .map(|(_, m)| m)
                .collect();
            if !options.is_empty() {
                return Some(options[rng.gen_range(0..options.len())]);
            }
        }
        if self.ply() >= self.settings.level.spec().book_plies {
            return None;
        }
        let moves: Vec<(Move, u16)> = legal_book_moves(self.app.board()).iter().map(|w| (w.m, w.weight)).collect();
        pick_weighted(&moves, rng)
    }

    fn start_bot_turn(&mut self, data: &mut CoachData, now: Instant) {
        let delay = BOT_MIN_THINK + Duration::from_millis(data.rng.gen_range(0..=BOT_EXTRA_THINK_MS));
        let chosen = self.opening_move(&mut data.rng);
        self.bot = Some(BotTurn { ply: self.ply(), ready_at: now + delay, chosen });
    }

    fn finish(&mut self, result: GameResult, ending: Ending, data: &mut CoachData) {
        let record = GameRecord {
            id: self.id,
            played_at: self.started,
            human: self.settings.human,
            level: self.settings.level,
            result,
            ending,
            blunder_check: self.settings.blunder_check,
            start_fen: START_FEN.to_string(),
            moves: self.moves(),
            analysis: Vec::new(),
        };
        let saved = data.add_game(record).map_err(|e| format!("Could not save the game: {e}"));
        self.finished = Some(Finished { result, ending, saved });
        self.warning = None;
        self.app.discard_proposed();
    }

    /// Judges your proposed move from the engine's view after it.
    fn judge_proposal(&mut self, m: Move, after: AnalysedPosition, now: Instant) {
        let Some(before) = self.baseline_for_now().cloned() else {
            return;
        };
        let board = self.app.board().clone();
        let mut after_board = board.without_eval_state();
        after_board.make_move(m);
        let punishment = line_story(&after_board, &after.best_line, board.turn);
        let review = review_moves(&board, &[m], &[before, after], &|_, _| false).into_iter().next();
        match review {
            Some(review) if review.class.is_serious() => {
                self.warning = Some(Warning {
                    punishment,
                    san: review.san.clone(),
                    verdict: format!("{} looks like a {}.", review.san, review.class.name().to_lowercase()),
                    explanation: review.cause.map(|c| c.explain()).unwrap_or_default(),
                });
            }
            _ => self.app.play_proposed(now),
        }
    }

    fn outcome_text(&self, finished: &Finished) -> String {
        let you_won = finished.result == win_for(self.settings.human);
        let headline = match finished.result {
            GameResult::Draw => "Draw".to_string(),
            _ if you_won => "You won".to_string(),
            _ => "The coach won".to_string(),
        };
        format!("{headline} by {}", finished.ending.describe())
    }

    fn side_lines(&self, data: &CoachData, now: Instant) -> Vec<Line<'static>> {
        let board = self.app.board();
        let mut lines = Vec::new();
        for color in [Color::White, Color::Black] {
            let to_move = board.turn == color && !self.is_over();
            let (name, rating) = if color == self.settings.human {
                ("You".to_string(), self.rating_before.0.to_string())
            } else {
                (format!("Coach level {}", self.settings.level.number()), format!("≈{}", self.settings.level.spec().elo.0))
            };
            let mut spans = vec![
                Span::styled(if to_move { "▶ " } else { "  " }, Style::new().fg(ACCENT)),
                Span::styled(format!("{name:<15}"), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{rating:<7}"), Style::new().fg(DIM)),
            ];
            if let Some(clocks) = self.app.clocks() {
                let ms = clocks.remaining_ms(color, board.turn, now).max(0);
                let style = if to_move { Style::new().fg(ACCENT).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT) };
                spans.push(Span::styled(format_clock(ms), style));
            }
            lines.push(Line::from(spans));
        }
        lines.push(Line::raw(""));
        lines.push(self.state_line());
        if let Some(warning) = &self.warning {
            lines.push(Line::raw(""));
            lines.push(Line::styled("Are you sure?", Style::new().fg(WARN).add_modifier(Modifier::BOLD)));
            lines.push(colored(warning.verdict.clone(), WARN));
            if !warning.explanation.is_empty() {
                lines.push(Line::styled(warning.explanation.clone(), Style::new().fg(TEXT)));
            }
            if let Some(story) = &warning.punishment {
                lines.push(dim(format!("For example: {}", story.moves)));
                lines.push(colored(format!("→ {}", story.outcome), WARN));
            }
            lines.push(dim(format!("Enter: play {} anyway   Esc: think again", warning.san)));
        }
        if let Some(confirm) = self.confirm {
            lines.push(Line::raw(""));
            let question = match confirm {
                Confirm::Resign => "Resign this game? It counts as a loss.",
                Confirm::Leave => "Leave this game? It won't be saved or rated.",
            };
            lines.push(colored(question, WARN));
            lines.push(dim("y: yes   n: no"));
        }
        if let Some(finished) = &self.finished {
            lines.push(Line::raw(""));
            let change = data.profile.rating.0 - self.rating_before.0;
            lines.push(Line::from(vec![
                Span::styled("Rating ", Style::new().fg(DIM)),
                Span::styled(
                    format!("{} → {} ({change:+})", self.rating_before.0, data.profile.rating.0),
                    Style::new().fg(if change >= 0 { GOOD } else { BAD }),
                ),
            ]));
            if let Err(e) = &finished.saved {
                lines.push(colored(e.clone(), BAD));
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled("Enter: review this game with the coach", Style::new().fg(ACCENT)));
            lines.push(dim("n: play again   Esc: back to the menu"));
        }
        lines
    }

    fn state_line(&self) -> Line<'static> {
        if let Some(finished) = &self.finished {
            let color = match finished.result {
                GameResult::Draw => TEXT,
                r if r == win_for(self.settings.human) => GOOD,
                _ => BAD,
            };
            return Line::styled(self.outcome_text(finished), Style::new().fg(color).add_modifier(Modifier::BOLD));
        }
        let text = if self.app.proposed().is_some() && self.warning.is_none() {
            "Checking your move…"
        } else if self.app.pending_promotion().is_some() {
            "Choose a promotion piece"
        } else if self.is_bot_turn() {
            "The coach is thinking…"
        } else {
            "Your move"
        };
        heading(text)
    }

    fn moves_lines(&self, rows: usize) -> Vec<Line<'static>> {
        let sans: Vec<&str> = self.app.history().iter().map(|(_, san)| san.as_str()).collect();
        let mut out: Vec<Line> = sans
            .chunks(2)
            .enumerate()
            .map(|(i, pair)| {
                Line::from(vec![
                    Span::styled(format!("{:>3}. ", i + 1), Style::new().fg(DIM)),
                    Span::styled(format!("{:<8}", pair[0]), Style::new().fg(TEXT)),
                    Span::styled(pair.get(1).copied().unwrap_or("").to_string(), Style::new().fg(TEXT)),
                ])
            })
            .collect();
        let skip = out.len().saturating_sub(rows);
        out.drain(..skip);
        out
    }

    fn handle_board_key(&mut self, key: KeyEvent, now: Instant) -> bool {
        if let Some(typed) = self.app.typed.as_mut() {
            match key.code {
                KeyCode::Enter => self.app.submit_typed(now),
                KeyCode::Esc => self.app.typed = None,
                KeyCode::Backspace => {
                    typed.pop();
                }
                KeyCode::Char(c) if !c.is_control() => typed.push(c),
                _ => {}
            }
            return true;
        }
        if self.app.pending_promotion().is_some() {
            let kind = match key.code.as_char().map(|c| c.to_ascii_lowercase()) {
                Some('q') => Some(PieceKind::Queen),
                Some('r') => Some(PieceKind::Rook),
                Some('b') => Some(PieceKind::Bishop),
                Some('n') => Some(PieceKind::Knight),
                _ => None,
            };
            match kind {
                Some(kind) => self.app.choose_promotion(kind, now),
                None => self.app.cancel(),
            }
            return true;
        }
        false
    }
}

impl Screen for CoachGame {
    fn draw(&self, frame: &mut Frame, data: &CoachData, now: Instant) {
        let layout = screen_layout(frame.area());
        let mut marks = Vec::new();
        if let Some(m) = self.app.proposed() {
            marks = move_marks(m, if self.warning.is_some() { MARK_BAD } else { super::common::MARK_HINT });
        }
        let scene = BoardScene {
            board: self.app.board(),
            flipped: self.app.flipped,
            piece_style: data.piece_style,
            selected: self.app.selected(),
            targets: self.app.targets(),
            last_move: self.app.last_move(),
            marks,
            promotion_open: self.app.pending_promotion().is_some(),
        };
        draw_board(frame, &layout, &scene);
        let side = layout.side;
        let info = self.side_lines(data, now);
        let info_height = (info.len() as u16 + 2 + 3).min(side.height);
        let title = format!("Game · level {}", self.settings.level.number());
        frame.render_widget(
            Paragraph::new(info).wrap(Wrap { trim: false }).block(panel(&title)),
            Rect::new(side.x, side.y, side.width, info_height),
        );
        if side.height > info_height + 3 {
            let moves_area = Rect::new(side.x, side.y + info_height, side.width, side.height - info_height);
            let rows = usize::from(moves_area.height.saturating_sub(2));
            frame.render_widget(Paragraph::new(self.moves_lines(rows)).block(panel("Moves")), moves_area);
        }
        let hints = if self.finished.is_some() {
            "Enter review · n play again · Esc menu"
        } else {
            "click or drag to move · Enter type a move · f flip · r resign · ? help · Esc leave"
        };
        draw_status(frame, layout.status, self.app.typed.as_deref(), self.message.as_deref(), hints);
        if self.help {
            draw_help(frame, KEYS);
        }
    }

    fn key(&mut self, key: KeyEvent, data: &mut CoachData, now: Instant) -> Nav {
        self.message = None;
        if self.help {
            self.help = false;
            return Nav::Stay;
        }
        if let Some(confirm) = self.confirm {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.confirm = None;
                    return match confirm {
                        Confirm::Leave => Nav::Back,
                        Confirm::Resign => {
                            self.finish(win_for(self.settings.human.other()), Ending::Resigned, data);
                            Nav::Stay
                        }
                    };
                }
                KeyCode::Char('n') | KeyCode::Esc => self.confirm = None,
                _ => {}
            }
            return Nav::Stay;
        }
        if self.warning.is_some() {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    self.warning = None;
                    self.app.play_proposed(now);
                }
                KeyCode::Esc | KeyCode::Char('n' | 'u') => {
                    self.warning = None;
                    self.app.discard_proposed();
                }
                _ => {}
            }
            return Nav::Stay;
        }
        if self.finished.is_some() {
            return match key.code {
                KeyCode::Enter => Nav::Replace(Box::new(ReviewScreen::new(self.id, data))),
                KeyCode::Char('n') => Nav::Replace(Box::new(CoachGame::new(self.settings, data))),
                KeyCode::Esc | KeyCode::Char('q') => Nav::Back,
                KeyCode::Char('f') => {
                    self.app.flipped = !self.app.flipped;
                    Nav::Stay
                }
                _ => Nav::Stay,
            };
        }
        if self.handle_board_key(key, now) {
            return Nav::Stay;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.app.history().is_empty() {
                    return Nav::Back;
                }
                self.confirm = Some(Confirm::Leave);
            }
            KeyCode::Char('r') => self.confirm = Some(Confirm::Resign),
            KeyCode::Char('f') => self.app.flipped = !self.app.flipped,
            KeyCode::Char('p') => {
                data.piece_style = data.piece_style.next();
                self.app.piece_style = data.piece_style;
            }
            KeyCode::Char('?' | 'h') => self.help = true,
            KeyCode::Enter | KeyCode::Char(':') if !self.is_bot_turn() => self.app.typed = Some(String::new()),
            _ => {}
        }
        Nav::Stay
    }

    fn mouse(&mut self, mouse: MouseEvent, area: Rect, _data: &mut CoachData, now: Instant) -> Nav {
        if self.help && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            self.help = false;
            return Nav::Stay;
        }
        if self.warning.is_some() || self.confirm.is_some() || self.finished.is_some() {
            return Nav::Stay;
        }
        let Some(geometry) = screen_layout(area).board else {
            return Nav::Stay;
        };
        let square = geometry.square_at(mouse.column, mouse.row, self.app.flipped);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.app.pending_promotion().is_some() {
                    match geometry.promotion_at(mouse.column, mouse.row) {
                        Some(kind) => self.app.choose_promotion(kind, now),
                        None => self.app.cancel(),
                    }
                } else if let Some(square) = square {
                    self.app.press_square(square, now);
                } else {
                    self.app.cancel();
                }
            }
            MouseEventKind::Up(MouseButton::Left) => match square {
                Some(square) => self.app.release_square(square, now),
                None => self.app.release_off_board(),
            },
            _ => {}
        }
        Nav::Stay
    }

    fn job(&self, _data: &CoachData) -> Option<Job> {
        if self.is_over() || self.confirm.is_some() {
            return None;
        }
        let ply = self.ply();
        if self.is_bot_turn() {
            let waiting = self.bot.as_ref().is_some_and(|bot| bot.ply == ply && bot.chosen.is_none());
            return waiting.then(|| Job {
                tag: ply as u64,
                kind: JobKind::ScoreMoves { nodes_per_move: self.bot_nodes() },
                board: self.app.board().clone(),
            });
        }
        if !self.settings.blunder_check {
            return None;
        }
        if self.baseline_for_now().is_none() {
            return Some(self.guard_search(BASELINE_TAG | ply as u64, self.app.board().clone()));
        }
        let proposed = self.app.proposed().filter(|_| self.warning.is_none())?;
        let mut after = self.app.board().clone();
        after.make_move(proposed);
        let tag = PROPOSAL_TAG | (ply as u64) << 16 | u64::from(proposed.from) << 8 | u64::from(proposed.to);
        Some(self.guard_search(tag, after))
    }

    fn engine(&mut self, tag: u64, result: JobResult, data: &mut CoachData, now: Instant) -> Nav {
        let ply = self.ply();
        match result {
            JobResult::Scores(scores) if tag == ply as u64 && self.is_bot_turn() => {
                let spec = self.settings.level.spec();
                if let Some(bot) = self.bot.as_mut().filter(|bot| bot.ply == ply) {
                    bot.chosen = choose_move(&scores, spec, &mut data.rng);
                }
            }
            JobResult::Done(outcome) if tag == BASELINE_TAG | ply as u64 => {
                let eval = eval_after_search(self.app.board(), outcome.score);
                self.baseline = Some((ply, AnalysedPosition { eval, best_line: outcome.pv }));
            }
            JobResult::Done(outcome) if tag & PROPOSAL_TAG != 0 => {
                if let Some(m) = self.app.proposed() {
                    let mut after = self.app.board().clone();
                    after.make_move(m);
                    let eval = eval_after_search(&after, outcome.score);
                    self.judge_proposal(m, AnalysedPosition { eval, best_line: outcome.pv }, now);
                }
            }
            _ => {}
        }
        Nav::Stay
    }

    fn tick(&mut self, data: &mut CoachData, now: Instant) -> Nav {
        if self.finished.is_some() {
            return Nav::Stay;
        }
        self.app.tick(now);
        if let Some(end) = self.app.outcome() {
            match Ending::from_rules(end.reason) {
                Some(ending) => self.finish(end.result, ending, data),
                None => self.message = Some(format!("The game stopped: {}", end.reason)),
            }
            return Nav::Stay;
        }
        if self.is_bot_turn() {
            if self.bot.as_ref().is_none_or(|bot| bot.ply != self.ply()) {
                self.start_bot_turn(data, now);
            }
            if let Some(bot) = &self.bot
                && let Some(m) = bot.chosen
                && now >= bot.ready_at
            {
                self.bot = None;
                self.app.play_engine_move(m, now);
            }
            return Nav::Stay;
        }
        if let Some(m) = self.app.proposed()
            && self.warning.is_none()
        {
            let mut after = self.app.board().clone();
            after.make_move(m);
            let forced = self.app.legal_moves().len() == 1;
            if forced || position_outcome(&after).is_some() {
                self.app.play_proposed(now);
            }
        }
        Nav::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::SearchOutcome;
    use crate::coach::store::Store;
    use crate::notation::parse_san_line;

    fn data(name: &str) -> CoachData {
        let dir = std::env::temp_dir().join(format!("chess-game-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        CoachData::load(Store::open(dir).unwrap()).unwrap()
    }

    fn guarded_game(data: &CoachData) -> CoachGame {
        let settings = GameSettings {
            level: Level::WEAKEST,
            human: Color::White,
            clock: None,
            blunder_check: true,
            opening: None,
        };
        CoachGame::new(settings, data)
    }

    fn san(game: &CoachGame, text: &str) -> Move {
        parse_san_line(game.app.board(), &[text]).unwrap()[0]
    }

    /// Answers the game's current engine job with `score` and `line` (SAN from the job's board).
    fn answer(game: &mut CoachGame, data: &mut CoachData, score: Score, line: &[&str]) {
        let job = game.job(data).unwrap();
        let pv = parse_san_line(&job.board, line).unwrap();
        let outcome = SearchOutcome { best_move: pv.first().copied(), score: Some(score), pv };
        game.engine(job.tag, JobResult::Done(outcome), data, Instant::now());
    }

    fn propose(game: &mut CoachGame, text: &str) {
        game.app.typed = Some(text.to_string());
        game.app.submit_typed(Instant::now());
    }

    #[test]
    fn a_sound_move_is_played_once_checked() {
        let mut data = data("sound");
        let mut game = guarded_game(&data);
        propose(&mut game, "e4");

        answer(&mut game, &mut data, Score::Centipawns(20), &["e4", "e5"]);
        answer(&mut game, &mut data, Score::Centipawns(-20), &["e5"]);

        assert_eq!(game.app.history().len(), 1);
        assert!(game.warning.is_none());
    }

    #[test]
    fn a_move_allowing_mate_is_held_back_with_the_reason() {
        let mut data = data("blunder");
        let mut game = guarded_game(&data);
        let now = Instant::now();
        game.app.set_hold_moves(false);
        propose(&mut game, "f3");
        let e5 = san(&game, "e5");
        game.app.play_engine_move(e5, now);
        game.app.set_hold_moves(true);
        propose(&mut game, "g4");

        answer(&mut game, &mut data, Score::Centipawns(-60), &["e4"]);
        answer(&mut game, &mut data, Score::MateIn(1), &["Qh4#"]);

        assert_eq!(game.app.history().len(), 2);
        let warning = game.warning.as_ref().unwrap();
        assert!(warning.explanation.contains("checkmate in 1"), "{}", warning.explanation);
        assert_eq!(warning.punishment.as_ref().map(|p| p.outcome.as_str()), Some("White is checkmated"));
        assert_eq!(game.job(&data).map(|j| j.tag), None);
    }

    #[test]
    fn resigning_saves_a_loss() {
        let mut data = data("resign");
        let mut game = guarded_game(&data);

        game.finish(win_for(Color::Black), Ending::Resigned, &mut data);

        assert_eq!(data.games.len(), 1);
        assert_eq!(data.games[0].record.ending, Ending::Resigned);
        assert!(data.profile.rating < Elo::START);
    }

    #[test]
    fn a_checkmated_position_needs_no_search() {
        let mated = Board::from_fen("rnb1kbnr/pppp1ppp/8/4p3/6Pq/5P2/PPPPP2P/RNBQKBNR w KQkq - 1 3").unwrap();
        let stalemate = Board::from_fen("7k/5Q2/6K1/8/8/8/8/8 b - - 0 1").unwrap();

        assert_eq!(eval_after_search(&mated, None), Eval::Checkmated);
        assert_eq!(eval_after_search(&stalemate, None), Eval::DRAWN);
    }
}
