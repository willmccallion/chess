//! Tactics puzzles: from your own mistakes and from the Lichess puzzle database.

use super::rating::Elo;
use crate::board::Board;
use crate::types::{Move, MoveList};
use crate::uci_io::parse_uci_move;

/// Names a puzzle wherever it came from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PuzzleId {
    Lichess(String),
    /// The position before your move at `ply` of a coach game.
    Game { game: u64, ply: usize },
}

impl PuzzleId {
    pub fn key(&self) -> String {
        match self {
            Self::Lichess(id) => format!("lichess:{id}"),
            Self::Game { game, ply } => format!("game:{game}:{ply}"),
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        let (source, rest) = key.split_once(':')?;
        match source {
            "lichess" if !rest.is_empty() => Some(Self::Lichess(rest.to_string())),
            "game" => {
                let (game, ply) = rest.split_once(':')?;
                Some(Self::Game { game: game.parse().ok()?, ply: ply.parse().ok()? })
            }
            _ => None,
        }
    }
}

/// A position where you have to find the moves.
#[derive(Clone)]
pub struct Puzzle {
    pub id: PuzzleId,
    /// The position you solve from, with you to move.
    pub board: Board,
    /// The opponent's move that led here, shown before you start.
    pub last_move: Option<Move>,
    /// Your moves and the opponent's replies, alternating, starting with yours.
    pub solution: Vec<Move>,
    pub rating: Option<Elo>,
    pub themes: Vec<String>,
}

impl Puzzle {
    pub fn is_from_your_games(&self) -> bool {
        matches!(self.id, PuzzleId::Game { .. })
    }
}

/// One line of the Lichess puzzle CSV, kept only if it is worth practising.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LichessPuzzle {
    pub id: String,
    pub fen: String,
    /// The opponent's move first, then the solution, in UCI.
    pub moves: Vec<String>,
    pub rating: Elo,
    pub popularity: i32,
    pub plays: u32,
    pub themes: Vec<String>,
}

/// Reads `PuzzleId,FEN,Moves,Rating,RatingDeviation,Popularity,NbPlays,Themes,GameUrl,OpeningTags`.
pub fn parse_lichess_csv_row(row: &str) -> Option<LichessPuzzle> {
    let fields: Vec<&str> = row.split(',').collect();
    let [id, fen, moves, rating, _deviation, popularity, plays, themes, ..] = fields.as_slice() else {
        return None;
    };
    let moves: Vec<String> = moves.split_whitespace().map(str::to_string).collect();
    if moves.len() < 2 || fen.is_empty() || id.is_empty() {
        return None;
    }
    Some(LichessPuzzle {
        id: (*id).to_string(),
        fen: (*fen).to_string(),
        moves,
        rating: Elo(rating.parse().ok()?),
        popularity: popularity.parse().ok()?,
        plays: plays.parse().ok()?,
        themes: themes.split_whitespace().map(str::to_string).collect(),
    })
}

impl LichessPuzzle {
    /// Plays the opponent's first move, giving the puzzle as you will see it.
    pub fn to_puzzle(&self) -> Option<Puzzle> {
        let mut board = Board::from_fen(&self.fen).ok()?.without_eval_state();
        let (first, rest) = self.moves.split_first()?;
        let last_move = parse_uci_move(&mut board, first)?;
        board.make_move(last_move);
        let mut scratch = board.clone();
        let mut solution = Vec::with_capacity(rest.len());
        for uci in rest {
            let m = parse_uci_move(&mut scratch, uci)?;
            scratch.make_move(m);
            solution.push(m);
        }
        Some(Puzzle {
            id: PuzzleId::Lichess(self.id.clone()),
            board,
            last_move: Some(last_move),
            solution,
            rating: Some(self.rating),
            themes: self.themes.clone(),
        })
    }
}

/// What happened when you tried a move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Right; the opponent answers with `reply` and you go on.
    Continue { reply: Move },
    Solved,
    /// Not the expected move, but it might be as good: the engine should judge it.
    NeedsEngine,
    Wrong { expected: Move },
}

/// A puzzle being solved.
#[derive(Clone)]
pub struct Attempt {
    pub puzzle: Puzzle,
    pub board: Board,
    /// How many solution moves have been played, yours and replies.
    pub progress: usize,
    pub state: AttemptState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptState {
    Solving,
    Solved { first_try: bool },
    Failed,
}

impl Attempt {
    pub fn new(puzzle: Puzzle) -> Self {
        Self {
            board: puzzle.board.clone(),
            puzzle,
            progress: 0,
            state: AttemptState::Solving,
        }
    }

    pub fn expected(&self) -> Option<Move> {
        self.puzzle.solution.get(self.progress).copied()
    }

    /// Judges your move `m`, which must be legal on the current board.
    pub fn play(&mut self, m: Move) -> Verdict {
        let Some(expected) = self.expected() else {
            return Verdict::Solved;
        };
        if m == expected || (self.is_final_move() && gives_mate(&self.board, m)) {
            return self.advance(m);
        }
        if self.puzzle.is_from_your_games() {
            return Verdict::NeedsEngine;
        }
        self.state = AttemptState::Failed;
        Verdict::Wrong { expected }
    }

    /// The engine's judgement of a move [`Attempt::play`] could not settle.
    pub fn engine_judged(&mut self, m: Move, good_enough: bool) -> Verdict {
        if good_enough {
            return self.advance(m);
        }
        self.state = AttemptState::Failed;
        Verdict::Wrong { expected: self.expected().unwrap_or(m) }
    }

    fn is_final_move(&self) -> bool {
        self.progress + 1 >= self.puzzle.solution.len()
    }

    fn advance(&mut self, m: Move) -> Verdict {
        self.board.make_move(m);
        self.progress += 1;
        match self.puzzle.solution.get(self.progress).copied() {
            Some(reply) if !self.is_solved_early() => {
                self.board.make_move(reply);
                self.progress += 1;
                if self.progress >= self.puzzle.solution.len() {
                    return self.finish();
                }
                Verdict::Continue { reply }
            }
            _ => self.finish(),
        }
    }

    /// Mate ends a puzzle even if the stored line was longer.
    fn is_solved_early(&self) -> bool {
        let mut legal = MoveList::new();
        self.board.generate_legal_moves(&mut legal);
        legal.is_empty()
    }

    fn finish(&mut self) -> Verdict {
        let first_try = self.state == AttemptState::Solving;
        self.state = AttemptState::Solved { first_try };
        Verdict::Solved
    }

    /// Plays the rest of the solution after a wrong answer, to show it.
    pub fn reveal_next(&mut self) -> Option<Move> {
        let m = self.expected()?;
        self.board.make_move(m);
        self.progress += 1;
        Some(m)
    }
}

fn gives_mate(board: &Board, m: Move) -> bool {
    let mut after = board.clone();
    after.make_move(m);
    let mut legal = MoveList::new();
    after.generate_legal_moves(&mut legal);
    let king = after.king_square(after.turn);
    legal.is_empty() && king < 64 && after.is_square_attacked(king as i32, after.turn.other())
}

/// Lichess themes worth naming, with how to say them.
const THEME_NAMES: &[(&str, &str)] = &[
    ("mateIn1", "Mate in 1"),
    ("mateIn2", "Mate in 2"),
    ("mateIn3", "Mate in 3"),
    ("backRankMate", "Back-rank mate"),
    ("fork", "Fork"),
    ("pin", "Pin"),
    ("skewer", "Skewer"),
    ("discoveredAttack", "Discovered attack"),
    ("doubleCheck", "Double check"),
    ("hangingPiece", "Hanging piece"),
    ("trappedPiece", "Trapped piece"),
    ("capturingDefender", "Remove the defender"),
    ("deflection", "Deflection"),
    ("attraction", "Attraction"),
    ("defensiveMove", "Defence"),
    ("promotion", "Promotion"),
    ("sacrifice", "Sacrifice"),
    ("endgame", "Endgame"),
    ("kingsideAttack", "Kingside attack"),
];

pub fn theme_name(theme: &str) -> Option<&'static str> {
    THEME_NAMES.iter().find(|(key, _)| *key == theme).map(|(_, name)| *name)
}

/// A puzzle's themes in words, most telling first.
pub fn describe_themes(themes: &[String]) -> String {
    let names: Vec<&str> = THEME_NAMES
        .iter()
        .filter(|(key, _)| themes.iter().any(|t| t == key))
        .map(|(_, name)| *name)
        .take(3)
        .collect();
    names.join(" · ")
}

/// A one-line form of a stored Lichess puzzle: id, FEN, moves, rating and themes.
pub fn lichess_to_line(p: &LichessPuzzle) -> String {
    format!("{}\t{}\t{}\t{}\t{}", p.id, p.fen, p.moves.join(" "), p.rating.0, p.themes.join(" "))
}

pub fn lichess_from_line(line: &str) -> Option<LichessPuzzle> {
    let fields: Vec<&str> = line.split('\t').collect();
    let [id, fen, moves, rating, themes] = fields.as_slice() else {
        return None;
    };
    Some(LichessPuzzle {
        id: (*id).to_string(),
        fen: (*fen).to_string(),
        moves: moves.split_whitespace().map(str::to_string).collect(),
        rating: Elo(rating.parse().ok()?),
        popularity: 0,
        plays: 0,
        themes: themes.split_whitespace().map(str::to_string).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROW: &str = "00sHx,q3k1nr/1pp1nQpp/3p4/1P2p3/4P3/B1PP1b2/B5PP/5K2 b k - 0 17,e8d7 a2e6 d7d8 f7f8,1760,80,83,72,mate mateIn2 middlegame short,https://lichess.org/yyznGmXs/black#34,Italian_Game";

    #[test]
    fn a_lichess_row_reads_into_its_fields() {
        let row = parse_lichess_csv_row(ROW).unwrap();

        assert_eq!(row.id, "00sHx");
        assert_eq!(row.rating, Elo(1760));
        assert_eq!(row.moves.len(), 4);
        assert!(row.themes.contains(&"mateIn2".to_string()));
    }

    #[test]
    fn the_first_lichess_move_is_the_opponents() {
        let puzzle = parse_lichess_csv_row(ROW).unwrap().to_puzzle().unwrap();

        assert_eq!(puzzle.solution.len(), 3);
        assert_eq!(puzzle.board.turn, crate::types::Color::White);
        assert!(puzzle.last_move.is_some());
    }

    #[test]
    fn solving_plays_the_replies_and_ends_solved() {
        let puzzle = parse_lichess_csv_row(ROW).unwrap().to_puzzle().unwrap();
        let mut attempt = Attempt::new(puzzle.clone());

        let first = attempt.play(puzzle.solution[0]);
        let last = attempt.play(puzzle.solution[2]);

        assert_eq!(first, Verdict::Continue { reply: puzzle.solution[1] });
        assert_eq!(last, Verdict::Solved);
        assert_eq!(attempt.state, AttemptState::Solved { first_try: true });
    }

    #[test]
    fn a_wrong_lichess_move_fails_and_names_the_answer() {
        let puzzle = parse_lichess_csv_row(ROW).unwrap().to_puzzle().unwrap();
        let mut legal = MoveList::new();
        puzzle.board.generate_legal_moves(&mut legal);
        let wrong = legal.as_slice().iter().copied().find(|&m| m != puzzle.solution[0]).unwrap();
        let mut attempt = Attempt::new(puzzle.clone());

        let verdict = attempt.play(wrong);

        assert_eq!(verdict, Verdict::Wrong { expected: puzzle.solution[0] });
        assert_eq!(attempt.state, AttemptState::Failed);
    }

    #[test]
    fn a_different_move_from_your_own_game_goes_to_the_engine() {
        let board = Board::from_fen("4k3/8/8/3q4/8/8/8/3RK3 w - - 0 1").unwrap();
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);
        let rxd5 = legal.as_slice().iter().copied().find(|m| m.capture).unwrap();
        let other = legal.as_slice().iter().copied().find(|m| !m.capture).unwrap();
        let puzzle = Puzzle {
            id: PuzzleId::Game { game: 1, ply: 0 },
            board,
            last_move: None,
            solution: vec![rxd5],
            rating: None,
            themes: Vec::new(),
        };
        let mut attempt = Attempt::new(puzzle);

        assert_eq!(attempt.play(other), Verdict::NeedsEngine);
        assert_eq!(attempt.engine_judged(other, true), Verdict::Solved);
    }

    #[test]
    fn puzzle_ids_survive_their_text_form() {
        for id in [PuzzleId::Lichess("00sHx".into()), PuzzleId::Game { game: 1_700_000_000_123, ply: 17 }] {
            assert_eq!(PuzzleId::from_key(&id.key()), Some(id));
        }
        assert_eq!(PuzzleId::from_key("nonsense"), None);
    }

    #[test]
    fn stored_lichess_puzzles_read_back() {
        let row = parse_lichess_csv_row(ROW).unwrap();

        let back = lichess_from_line(&lichess_to_line(&row)).unwrap();

        assert_eq!((back.id, back.fen, back.moves, back.rating), (row.id, row.fen, row.moves, row.rating));
    }

    #[test]
    fn themes_read_as_words() {
        let themes = vec!["middlegame".to_string(), "mateIn2".to_string(), "fork".to_string()];

        assert_eq!(describe_themes(&themes), "Mate in 2 · Fork");
    }
}
