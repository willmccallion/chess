use crate::board::Board;
use crate::game_runner::{GameClock, GameEndReason, GameResult, position_outcome, win_for};
use crate::search::{IterationInfo, Score};
use crate::time::{TimeBudget, TimeControl};
use crate::types::{Color, Move, MoveList, PieceKind};
use crate::uci_io::parse_uci_move;
use std::time::Instant;

const ENGINE_MOVE_OVERHEAD_MS: i64 = 50;

/// How a session is played.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// You against the engine, with its evaluation, analysis and takebacks available.
    Casual,
    /// You against the engine with no help: engine output stays hidden until the game ends.
    Game,
    /// The engine plays both sides.
    Watch,
}

/// How long the engine thinks: a fixed time per move, or a game clock for both sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeSetting {
    PerMove { ms: u64 },
    Clock(GameClock),
}

pub struct PlayConfig {
    pub start: Board,
    pub mode: Mode,
    /// The side you play; ignored in [`Mode::Watch`].
    pub human: Color,
    pub time: TimeSetting,
}

/// Parses a clock like `5+3`: minutes, then the increment in seconds.
pub fn parse_clock(text: &str) -> Result<GameClock, String> {
    let (minutes, increment) = text
        .split_once('+')
        .ok_or_else(|| format!("expected minutes+increment, like 5+3, got '{text}'"))?;
    let minutes: f64 = minutes
        .trim()
        .parse()
        .map_err(|_| format!("bad minutes '{minutes}'"))?;
    let increment: f64 = increment
        .trim()
        .parse()
        .map_err(|_| format!("bad increment '{increment}'"))?;
    if !(minutes > 0.0 && increment >= 0.0 && minutes.is_finite() && increment.is_finite()) {
        return Err(format!("clock must have positive minutes and no negative increment: '{text}'"));
    }
    Ok(GameClock {
        base_ms: (minutes * 60_000.0).round() as u64,
        increment_ms: (increment * 1000.0).round() as u64,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskKind {
    /// Choosing the engine's move.
    Think,
    /// Analysing the position without playing.
    Analyse,
}

/// What the engine should be doing; a change of position gives a different task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineTask {
    pub kind: TaskKind,
    pub position_version: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameEnd {
    pub result: GameResult,
    pub reason: GameEndReason,
}

/// The latest search report, ready to display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineLine {
    pub source: TaskKind,
    pub depth: usize,
    pub seldepth: usize,
    pub nodes: u64,
    pub nps: u64,
    /// From White's point of view.
    pub score: Score,
    pub pv_san: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PieceStyle {
    /// Pixel art filling the square; falls back to glyphs on small boards.
    Sprites,
    Glyphs,
    Letters,
}

impl PieceStyle {
    pub const fn next(self) -> Self {
        match self {
            Self::Sprites => Self::Glyphs,
            Self::Glyphs => Self::Letters,
            Self::Letters => Self::Sprites,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Panels {
    pub eval: bool,
    pub moves: bool,
    pub help: bool,
}

/// Both players' remaining time; the side to move's clock runs from `running_since`.
#[derive(Clone, Copy, Debug)]
pub struct Clocks {
    remaining_ms: [i64; 2],
    increment_ms: i64,
    running_since: Option<Instant>,
}

impl Clocks {
    fn new(clock: GameClock, now: Instant) -> Self {
        Self {
            remaining_ms: [clock.base_ms as i64; 2],
            increment_ms: clock.increment_ms as i64,
            running_since: Some(now),
        }
    }

    pub fn remaining_ms(&self, color: Color, to_move: Color, now: Instant) -> i64 {
        let stored = self.remaining_ms[color as usize];
        match self.running_since {
            Some(since) if color == to_move => stored - since_ms(since, now),
            _ => stored,
        }
    }

    /// Charges the mover's thinking time and adds the increment; false if they flagged.
    fn complete_move(&mut self, mover: Color, now: Instant) -> bool {
        let remaining = self.remaining_ms(mover, mover, now);
        if remaining <= 0 {
            self.remaining_ms[mover as usize] = 0;
            return false;
        }
        self.remaining_ms[mover as usize] = remaining + self.increment_ms;
        self.running_since = self.running_since.map(|_| now);
        true
    }

    fn pause(&mut self, to_move: Color, now: Instant) {
        self.remaining_ms[to_move as usize] = self.remaining_ms(to_move, to_move, now);
        self.running_since = None;
    }

    fn resume(&mut self, now: Instant) {
        self.running_since = Some(now);
    }

    fn time_control(&self, to_move: Color, now: Instant) -> TimeControl {
        TimeControl {
            wtime: self.remaining_ms(Color::White, to_move, now),
            btime: self.remaining_ms(Color::Black, to_move, now),
            winc: self.increment_ms,
            binc: self.increment_ms,
            movestogo: 0,
            move_overhead_ms: ENGINE_MOVE_OVERHEAD_MS,
        }
    }
}

fn since_ms(since: Instant, now: Instant) -> i64 {
    now.saturating_duration_since(since).as_millis() as i64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingPromotion {
    pub from: u8,
    pub to: u8,
}

pub struct App {
    config: PlayConfig,
    human: Color,
    board: Board,
    history: Vec<(Move, String)>,
    legal: MoveList,
    position_version: u64,
    outcome: Option<GameEnd>,
    clocks: Option<Clocks>,
    paused: bool,
    selected: Option<u8>,
    dragging: bool,
    pending_promotion: Option<PendingPromotion>,
    analysis: bool,
    engine_line: Option<EngineLine>,
    pub panels: Panels,
    pub flipped: bool,
    pub piece_style: PieceStyle,
    /// The move being typed, when move entry is open.
    pub typed: Option<String>,
    pub message: Option<String>,
    pub quit: bool,
}

impl App {
    pub fn new(config: PlayConfig, now: Instant) -> Self {
        let human = config.human;
        let mut app = Self {
            board: config.start.clone(),
            human,
            history: Vec::new(),
            legal: MoveList::new(),
            position_version: 0,
            outcome: None,
            clocks: None,
            paused: false,
            selected: None,
            dragging: false,
            pending_promotion: None,
            analysis: config.mode == Mode::Casual,
            engine_line: None,
            panels: Panels {
                eval: true,
                moves: true,
                help: false,
            },
            flipped: human == Color::Black && config.mode != Mode::Watch,
            piece_style: PieceStyle::Sprites,
            typed: None,
            message: None,
            quit: false,
            config,
        };
        app.new_game(now);
        app
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    pub fn mode(&self) -> Mode {
        self.config.mode
    }

    pub fn human(&self) -> Color {
        self.human
    }

    pub fn time_setting(&self) -> TimeSetting {
        self.config.time
    }

    pub fn start_position(&self) -> &Board {
        &self.config.start
    }

    pub fn history(&self) -> &[(Move, String)] {
        &self.history
    }

    pub fn outcome(&self) -> Option<GameEnd> {
        self.outcome
    }

    pub fn clocks(&self) -> Option<&Clocks> {
        self.clocks.as_ref()
    }

    pub fn selected(&self) -> Option<u8> {
        self.selected
    }

    pub fn pending_promotion(&self) -> Option<PendingPromotion> {
        self.pending_promotion
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn last_move(&self) -> Option<Move> {
        self.history.last().map(|&(m, _)| m)
    }

    /// The engine's latest report, if it may be shown now.
    pub fn engine_line(&self) -> Option<&EngineLine> {
        self.shows_engine_output().then_some(self.engine_line.as_ref()).flatten()
    }

    /// False during a game in [`Mode::Game`], where engine help is withheld until the end.
    pub fn assistance_allowed(&self) -> bool {
        self.config.mode != Mode::Game || self.outcome.is_some()
    }

    pub fn shows_engine_output(&self) -> bool {
        self.panels.eval && self.assistance_allowed()
    }

    pub fn is_engine_turn(&self) -> bool {
        self.config.mode == Mode::Watch || self.board.turn != self.human
    }

    fn is_human_turn(&self) -> bool {
        self.outcome.is_none() && !self.is_engine_turn()
    }

    /// Squares the selected piece can move to.
    pub fn targets(&self) -> Vec<u8> {
        let Some(from) = self.selected else {
            return Vec::new();
        };
        self.legal
            .as_slice()
            .iter()
            .filter(|m| m.from == from)
            .map(|m| m.to)
            .collect()
    }

    pub fn new_game(&mut self, now: Instant) {
        self.board = self.config.start.clone();
        self.history.clear();
        self.clocks = match self.config.time {
            TimeSetting::Clock(clock) => Some(Clocks::new(clock, now)),
            TimeSetting::PerMove { .. } => None,
        };
        self.paused = false;
        self.engine_line = None;
        self.message = None;
        self.position_changed();
    }

    /// Starts a new game with you playing the other colour.
    pub fn swap_sides(&mut self, now: Instant) {
        if self.config.mode == Mode::Watch {
            return;
        }
        self.human = self.human.other();
        self.flipped = self.human == Color::Black;
        self.new_game(now);
    }

    fn position_changed(&mut self) {
        self.position_version += 1;
        self.legal = MoveList::new();
        self.board.generate_legal_moves(&mut self.legal);
        self.selected = None;
        self.dragging = false;
        self.pending_promotion = None;
        self.outcome = position_outcome(&self.board).map(|(result, reason)| GameEnd { result, reason });
    }

    /// What the engine should be working on right now, if anything.
    pub fn engine_task(&self) -> Option<EngineTask> {
        let wants_analysis = self.analysis && self.assistance_allowed() && !self.legal.is_empty();
        let kind = if self.outcome.is_some() {
            wants_analysis.then_some(TaskKind::Analyse)
        } else if self.is_engine_turn() {
            (!self.paused).then_some(TaskKind::Think)
        } else {
            wants_analysis.then_some(TaskKind::Analyse)
        }?;
        Some(EngineTask {
            kind,
            position_version: self.position_version,
        })
    }

    /// The time limit for the engine's move in the current position.
    pub fn think_budget(&self, now: Instant) -> TimeBudget {
        match (&self.clocks, self.config.time) {
            (Some(clocks), _) => clocks.time_control(self.board.turn, now).budget(self.board.turn),
            (None, TimeSetting::PerMove { ms }) => TimeBudget::fixed(ms),
            (None, TimeSetting::Clock(clock)) => TimeBudget::fixed(clock.base_ms / 40),
        }
    }

    /// Records a search report for the position `root` it was produced in.
    pub fn record_engine_info(&mut self, source: TaskKind, root: &Board, info: &IterationInfo) {
        let score = match (info.score, root.turn) {
            (score, Color::White) => score,
            (Score::Centipawns(cp), Color::Black) => Score::Centipawns(-cp),
            (Score::MateIn(moves), Color::Black) => Score::MateIn(-moves),
        };
        self.engine_line = Some(EngineLine {
            source,
            depth: info.depth,
            seldepth: info.seldepth,
            nodes: info.nodes,
            nps: info.nps,
            score,
            pv_san: line_in_san(root, &info.pv),
        });
    }

    /// Plays the engine's chosen move if it is still the engine's turn.
    pub fn play_engine_move(&mut self, m: Move, now: Instant) {
        if self.outcome.is_none() && self.is_engine_turn() && self.legal.as_slice().contains(&m) {
            self.play(m, now);
        }
    }

    fn play(&mut self, m: Move, now: Instant) {
        let mover = self.board.turn;
        if let Some(clocks) = self.clocks.as_mut()
            && !clocks.complete_move(mover, now)
        {
            self.flag(mover);
            return;
        }
        let san = self.board.to_san(m, self.legal.as_slice());
        self.board.make_move(m);
        self.history.push((m, san));
        self.message = None;
        self.position_changed();
    }

    fn flag(&mut self, loser: Color) {
        self.outcome = Some(GameEnd {
            result: win_for(loser.other()),
            reason: GameEndReason::FlagFall,
        });
        self.selected = None;
        self.pending_promotion = None;
    }

    /// Ends the game if the side to move has run out of time.
    pub fn tick(&mut self, now: Instant) {
        if self.outcome.is_some() {
            return;
        }
        let to_move = self.board.turn;
        if let Some(clocks) = &self.clocks
            && clocks.remaining_ms(to_move, to_move, now) <= 0
        {
            self.flag(to_move);
        }
    }

    pub fn toggle_pause(&mut self, now: Instant) {
        if self.config.mode != Mode::Watch || self.outcome.is_some() {
            return;
        }
        self.paused = !self.paused;
        let to_move = self.board.turn;
        if let Some(clocks) = self.clocks.as_mut() {
            if self.paused {
                clocks.pause(to_move, now);
            } else {
                clocks.resume(now);
            }
        }
    }

    pub fn toggle_analysis(&mut self) {
        if self.config.mode == Mode::Watch {
            return;
        }
        if !self.assistance_allowed() {
            self.message = Some("Analysis is off until the game ends in game mode".into());
            return;
        }
        self.analysis = !self.analysis;
    }

    /// Takes back moves until it is your turn again.
    pub fn undo(&mut self, now: Instant) {
        match self.config.mode {
            Mode::Game => {
                self.message = Some("No takebacks in game mode".into());
                return;
            }
            Mode::Watch => return,
            Mode::Casual => {}
        }
        if self.history.is_empty() {
            return;
        }
        self.history.pop();
        while self.board_after_history().turn != self.human && !self.history.is_empty() {
            self.history.pop();
        }
        self.board = self.board_after_history();
        if let Some(clocks) = self.clocks.as_mut() {
            clocks.running_since = Some(now);
        }
        self.message = None;
        self.position_changed();
    }

    fn board_after_history(&self) -> Board {
        let mut board = self.config.start.clone();
        for &(m, _) in &self.history {
            board.make_move(m);
        }
        board
    }

    /// The game so far: its moves in numbered SAN and the position reached.
    pub fn record(&self) -> String {
        let moves: Vec<Move> = self.history.iter().map(|&(m, _)| m).collect();
        let mut record = String::new();
        if self.config.start.to_fen() != crate::types::START_FEN {
            record.push_str(&format!("Start: {}\n", self.config.start.to_fen()));
        }
        record.push_str(&format!("Moves: {}\n", line_in_san(&self.config.start, &moves)));
        record.push_str(&format!("Final: {}", self.board.to_fen()));
        if let Some(end) = self.outcome {
            record.push_str(&format!("\nResult: {:?} by {}", end.result, end.reason));
        }
        record
    }

    /// A press of the left mouse button on `square`.
    pub fn press_square(&mut self, square: u8, now: Instant) {
        if !self.is_human_turn() || self.pending_promotion.is_some() {
            return;
        }
        if let Some(from) = self.selected
            && from != square
            && !self.moves_between(from, square).is_empty()
        {
            self.move_piece(from, square, now);
            return;
        }
        if self.own_piece_on(square) {
            self.selected = Some(square);
            self.dragging = true;
        } else {
            self.selected = None;
        }
    }

    /// A release of the left mouse button on `square`; completes a drag.
    pub fn release_square(&mut self, square: u8, now: Instant) {
        let was_dragging = std::mem::take(&mut self.dragging);
        if let Some(from) = self.selected
            && was_dragging
            && from != square
            && !self.moves_between(from, square).is_empty()
        {
            self.move_piece(from, square, now);
        }
    }

    pub fn release_off_board(&mut self) {
        self.dragging = false;
    }

    fn own_piece_on(&self, square: u8) -> bool {
        self.board.piece_on[square as usize].color() == Some(self.board.turn)
    }

    /// Legal moves taking the piece on `from` to `to`; several when promoting. Moving the
    /// king onto its own rook castles on that side.
    fn moves_between(&self, from: u8, to: u8) -> Vec<Move> {
        let legal = self.legal.as_slice();
        let direct: Vec<Move> = legal.iter().filter(|m| m.from == from && m.to == to).copied().collect();
        if !direct.is_empty() {
            return direct;
        }
        let mover = self.board.piece_on[from as usize];
        let target = self.board.piece_on[to as usize];
        let king_onto_own_rook = mover.kind() == Some(PieceKind::King)
            && target.kind() == Some(PieceKind::Rook)
            && target.color() == mover.color();
        if !king_onto_own_rook {
            return Vec::new();
        }
        legal
            .iter()
            .filter(|m| m.castle && m.from == from && (m.to > m.from) == (to > from))
            .copied()
            .collect()
    }

    fn move_piece(&mut self, from: u8, to: u8, now: Instant) {
        let candidates = self.moves_between(from, to);
        match candidates.as_slice() {
            [] => {}
            [only] => self.play(*only, now),
            _ => {
                self.pending_promotion = Some(PendingPromotion { from, to });
                self.selected = None;
                self.dragging = false;
            }
        }
    }

    pub fn choose_promotion(&mut self, kind: PieceKind, now: Instant) {
        let Some(PendingPromotion { from, to }) = self.pending_promotion.take() else {
            return;
        };
        let chosen = self
            .legal
            .as_slice()
            .iter()
            .find(|m| m.from == from && m.to == to && m.promotion == Some(kind))
            .copied();
        if let Some(m) = chosen {
            self.play(m, now);
        }
    }

    pub fn cancel(&mut self) {
        self.pending_promotion = None;
        self.selected = None;
        self.dragging = false;
        self.typed = None;
        self.panels.help = false;
    }

    /// Plays the typed move (UCI like `e2e4` or SAN like `Nf3`), or explains why not.
    pub fn submit_typed(&mut self, now: Instant) {
        let Some(text) = self.typed.take() else {
            return;
        };
        if !self.is_human_turn() {
            self.message = Some("It is not your turn".into());
            return;
        }
        match parse_typed_move(&self.board, self.legal.as_slice(), &text) {
            Some(m) => self.play(m, now),
            None => self.message = Some(format!("'{}' is not a legal move here", text.trim())),
        }
    }
}

/// Reads a move typed in UCI (`e7e8q`) or SAN (`Nf3`, `exd5`, `O-O`), ignoring check marks.
pub fn parse_typed_move(board: &Board, legal: &[Move], text: &str) -> Option<Move> {
    let text = text.trim();
    let mut scratch = board.clone();
    if let Some(m) = parse_uci_move(&mut scratch, text) {
        return Some(m);
    }
    let wanted = normalize_san(text);
    legal
        .iter()
        .copied()
        .find(|&m| normalize_san(&board.to_san(m, legal)) == wanted)
}

fn normalize_san(san: &str) -> String {
    san.chars()
        .filter(|c| !matches!(c, '+' | '#' | '!' | '?'))
        .map(|c| if c == '0' { 'O' } else { c })
        .collect()
}

/// `moves` from `root` in numbered SAN, stopping at the first move that is not legal.
pub fn line_in_san(root: &Board, moves: &[Move]) -> String {
    let mut board = root.clone();
    let mut parts = Vec::new();
    for (i, &m) in moves.iter().enumerate() {
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);
        if !legal.as_slice().contains(&m) {
            break;
        }
        let san = board.to_san(m, legal.as_slice());
        match board.turn {
            Color::White => parts.push(format!("{}. {san}", board.fullmove_number)),
            Color::Black if i == 0 => parts.push(format!("{}... {san}", board.fullmove_number)),
            Color::Black => parts.push(san),
        }
        board.make_move(m);
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::START_FEN;
    use std::time::Duration;

    fn square(name: &str) -> u8 {
        let bytes = name.as_bytes();
        (bytes[1] - b'1') * 8 + (bytes[0] - b'a')
    }

    fn app_at(fen: &str, mode: Mode, human: Color, time: TimeSetting, now: Instant) -> App {
        let config = PlayConfig {
            start: Board::from_fen(fen).unwrap(),
            mode,
            human,
            time,
        };
        App::new(config, now)
    }

    fn app(fen: &str, mode: Mode, human: Color, time: TimeSetting) -> App {
        app_at(fen, mode, human, time, Instant::now())
    }

    fn casual(fen: &str) -> App {
        app(fen, Mode::Casual, Color::White, TimeSetting::PerMove { ms: 100 })
    }

    #[test]
    fn clock_text_is_minutes_plus_increment_seconds() {
        let clock = parse_clock("5+3").unwrap();
        assert_eq!(clock, GameClock { base_ms: 300_000, increment_ms: 3_000 });
        assert_eq!(parse_clock("0.5+0").unwrap().base_ms, 30_000);
        assert!(parse_clock("5").is_err());
        assert!(parse_clock("0+1").is_err());
        assert!(parse_clock("5+-1").is_err());
    }

    #[test]
    fn clicking_a_piece_then_a_target_plays_the_move() {
        let mut app = casual(START_FEN);
        let now = Instant::now();

        app.press_square(square("e2"), now);
        app.release_square(square("e2"), now);
        app.press_square(square("e4"), now);

        assert_eq!(app.history().len(), 1);
        assert_eq!(app.history()[0].1, "e4");
        assert_eq!(app.board().turn, Color::Black);
    }

    #[test]
    fn dragging_a_piece_onto_a_target_plays_the_move() {
        let mut app = casual(START_FEN);
        let now = Instant::now();

        app.press_square(square("g1"), now);
        app.release_square(square("f3"), now);

        assert_eq!(app.history()[0].1, "Nf3");
    }

    #[test]
    fn clicking_an_illegal_square_plays_nothing_and_drops_the_selection() {
        let mut app = casual(START_FEN);
        let now = Instant::now();

        app.press_square(square("e2"), now);
        app.release_square(square("e2"), now);
        app.press_square(square("e5"), now);

        assert!(app.history().is_empty());
        assert_eq!(app.selected(), None);
    }

    #[test]
    fn pieces_cannot_be_moved_on_the_engines_turn() {
        let mut app = casual("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1");
        let now = Instant::now();

        app.press_square(square("e7"), now);

        assert_eq!(app.selected(), None);
        assert_eq!(app.engine_task().map(|t| t.kind), Some(TaskKind::Think));
    }

    #[test]
    fn the_king_castles_by_moving_two_squares_or_onto_its_rook() {
        let fen = "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1";
        let now = Instant::now();
        let mut two_squares = casual(fen);
        let mut onto_rook = casual(fen);
        let mut queenside = casual(fen);

        two_squares.press_square(square("e1"), now);
        two_squares.release_square(square("g1"), now);
        onto_rook.press_square(square("e1"), now);
        onto_rook.release_square(square("e1"), now);
        onto_rook.press_square(square("h1"), now);
        queenside.press_square(square("e1"), now);
        queenside.release_square(square("a1"), now);

        assert_eq!(two_squares.history()[0].1, "O-O");
        assert_eq!(onto_rook.history()[0].1, "O-O");
        assert_eq!(queenside.history()[0].1, "O-O-O");
    }

    #[test]
    fn en_passant_is_offered_only_onto_the_square_behind_the_pawn() {
        let mut app = casual("rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3");
        let now = Instant::now();

        app.press_square(square("e5"), now);
        let mut targets = app.targets();
        targets.sort_unstable();
        app.release_square(square("e5"), now);
        app.press_square(square("d6"), now);

        assert_eq!(targets, vec![square("d6"), square("e6")]);
        assert_eq!(app.history()[0].1, "exd6");
        assert_eq!(app.board().piece_on[square("d5") as usize], crate::types::Piece::Empty);
    }

    #[test]
    fn promotion_waits_for_a_choice_of_piece() {
        let mut app = casual("8/4P3/8/8/8/8/k7/4K3 w - - 0 1");
        let now = Instant::now();

        app.press_square(square("e7"), now);
        app.release_square(square("e8"), now);
        assert!(app.pending_promotion().is_some());
        assert!(app.history().is_empty());

        app.choose_promotion(PieceKind::Knight, now);

        assert_eq!(app.history()[0].1, "e8=N");
    }

    #[test]
    fn typed_moves_accept_uci_and_san() {
        let board = Board::from_fen(START_FEN).unwrap();
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);

        let uci = parse_typed_move(&board, legal.as_slice(), "g1f3").unwrap();
        let san = parse_typed_move(&board, legal.as_slice(), "Nf3").unwrap();

        assert_eq!(uci, san);
        assert!(parse_typed_move(&board, legal.as_slice(), "Nf4").is_none());
        assert!(parse_typed_move(&board, legal.as_slice(), "zz").is_none());
    }

    #[test]
    fn castling_can_be_typed_with_zeros_or_letters() {
        let board = Board::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1").unwrap();
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);

        let letters = parse_typed_move(&board, legal.as_slice(), "O-O").unwrap();
        let zeros = parse_typed_move(&board, legal.as_slice(), "0-0").unwrap();

        assert!(letters.castle && letters == zeros);
    }

    #[test]
    fn undo_returns_to_your_turn() {
        let mut app = casual(START_FEN);
        let now = Instant::now();
        app.typed = Some("e4".into());
        app.submit_typed(now);
        let reply = parse_typed_move(app.board(), app.legal.as_slice(), "e5").unwrap();
        app.play_engine_move(reply, now);

        app.undo(now);

        assert!(app.history().is_empty());
        assert_eq!(app.board().turn, Color::White);
    }

    #[test]
    fn game_mode_hides_engine_output_and_refuses_takebacks_until_the_end() {
        let mut app = app(START_FEN, Mode::Game, Color::White, TimeSetting::PerMove { ms: 100 });
        let now = Instant::now();
        app.typed = Some("e4".into());
        app.submit_typed(now);
        let info = IterationInfo {
            depth: 5,
            seldepth: 7,
            score: Score::Centipawns(20),
            hashfull_permill: 0,
            nodes: 1000,
            nps: 1000,
            time_ms: 1,
            pv: Vec::new(),
        };
        let root = app.board().clone();
        app.record_engine_info(TaskKind::Think, &root, &info);

        app.undo(now);
        app.toggle_analysis();

        assert_eq!(app.history().len(), 1);
        assert!(app.engine_line().is_none());
        assert_eq!(app.engine_task().map(|t| t.kind), Some(TaskKind::Think));
    }

    #[test]
    fn analysis_runs_on_your_turn_in_casual_mode_only() {
        let casual = casual(START_FEN);
        let game = app(START_FEN, Mode::Game, Color::White, TimeSetting::PerMove { ms: 100 });

        assert_eq!(casual.engine_task().map(|t| t.kind), Some(TaskKind::Analyse));
        assert_eq!(game.engine_task(), None);
    }

    #[test]
    fn engine_scores_are_shown_from_whites_side() {
        let mut app = casual(START_FEN);
        let root = Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1").unwrap();
        let info = IterationInfo {
            depth: 1,
            seldepth: 1,
            score: Score::MateIn(3),
            hashfull_permill: 0,
            nodes: 1,
            nps: 1,
            time_ms: 1,
            pv: Vec::new(),
        };

        app.record_engine_info(TaskKind::Analyse, &root, &info);

        assert_eq!(app.engine_line().map(|l| l.score), Some(Score::MateIn(-3)));
    }

    #[test]
    fn running_out_of_time_loses() {
        let clock = GameClock { base_ms: 1_000, increment_ms: 0 };
        let mut app = app(START_FEN, Mode::Casual, Color::White, TimeSetting::Clock(clock));
        let later = Instant::now() + Duration::from_millis(1_500);

        app.tick(later);

        assert_eq!(
            app.outcome(),
            Some(GameEnd { result: GameResult::BlackWin, reason: GameEndReason::FlagFall })
        );
        assert_ne!(app.engine_task().map(|t| t.kind), Some(TaskKind::Think));
    }

    #[test]
    fn a_move_adds_the_increment_to_the_movers_clock() {
        let clock = GameClock { base_ms: 10_000, increment_ms: 2_000 };
        let start = Instant::now();
        let mut app = app_at(START_FEN, Mode::Casual, Color::White, TimeSetting::Clock(clock), start);
        let moved_at = start + Duration::from_millis(3_000);

        app.typed = Some("e4".into());
        app.submit_typed(moved_at);

        let clocks = app.clocks().unwrap();
        assert_eq!(clocks.remaining_ms(Color::White, Color::Black, moved_at), 9_000);
    }

    #[test]
    fn pv_is_numbered_from_the_side_to_move() {
        let board = Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1").unwrap();
        let mut scratch = board.clone();
        let e5 = parse_uci_move(&mut scratch, "e7e5").unwrap();
        scratch.make_move(e5);
        let nf3 = parse_uci_move(&mut scratch, "g1f3").unwrap();

        assert_eq!(line_in_san(&board, &[e5, nf3]), "1... e5 2. Nf3");
    }
}
