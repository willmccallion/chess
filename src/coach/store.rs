//! Where the coach keeps your games and training history: plain tab-separated files that are
//! only ever appended to, except when a game's analysis is filled in.

use super::bot::Level;
use super::judgement::Eval;
use super::puzzles::{
    LichessPuzzle, PuzzleId, lichess_from_line, lichess_to_line, parse_lichess_csv_row,
};
use super::rating::Elo;
use super::review::AnalysedPosition;
use super::srs::UnixTime;
use crate::board::Board;
use crate::game_runner::{GameEndReason, GameResult};
use crate::search::Score;
use crate::types::{Color, Move};
use crate::uci_io::{format_uci, parse_uci_move};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

const GAMES_FILE: &str = "games.tsv";
const ATTEMPTS_FILE: &str = "attempts.tsv";
const LICHESS_FILE: &str = "lichess_puzzles.tsv";
const GAME_TAG: &str = "game1";
const ATTEMPT_TAG: &str = "attempt1";

/// Identifies a coach game: the millisecond it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GameId(pub u64);

/// How a coach game ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ending {
    Checkmate,
    Stalemate,
    Repetition,
    FiftyMoves,
    InsufficientMaterial,
    Timeout,
    Resigned,
}

impl Ending {
    const ALL: [(Self, &'static str); 7] = [
        (Self::Checkmate, "checkmate"),
        (Self::Stalemate, "stalemate"),
        (Self::Repetition, "repetition"),
        (Self::FiftyMoves, "fifty"),
        (Self::InsufficientMaterial, "insufficient"),
        (Self::Timeout, "timeout"),
        (Self::Resigned, "resigned"),
    ];

    /// The ending for a game stopped by the rules or the clock; None for reasons that only
    /// arise in engine matches.
    pub const fn from_rules(reason: GameEndReason) -> Option<Self> {
        match reason {
            GameEndReason::Checkmate => Some(Self::Checkmate),
            GameEndReason::Stalemate => Some(Self::Stalemate),
            GameEndReason::Repetition => Some(Self::Repetition),
            GameEndReason::FiftyMoveRule => Some(Self::FiftyMoves),
            GameEndReason::InsufficientMaterial => Some(Self::InsufficientMaterial),
            GameEndReason::FlagFall => Some(Self::Timeout),
            GameEndReason::MoveLimitExceeded
            | GameEndReason::IllegalMove
            | GameEndReason::EngineCrash => None,
        }
    }

    fn token(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(e, _)| *e == self)
            .map_or("", |(_, t)| t)
    }

    fn from_token(token: &str) -> Option<Self> {
        Self::ALL.iter().find(|(_, t)| *t == token).map(|(e, _)| *e)
    }

    pub const fn describe(self) -> &'static str {
        match self {
            Self::Checkmate => "checkmate",
            Self::Stalemate => "stalemate",
            Self::Repetition => "repetition",
            Self::FiftyMoves => "the 50-move rule",
            Self::InsufficientMaterial => "insufficient material",
            Self::Timeout => "time",
            Self::Resigned => "resignation",
        }
    }
}

/// A finished game against the coach.
#[derive(Clone, Debug)]
pub struct GameRecord {
    pub id: GameId,
    pub played_at: UnixTime,
    pub human: Color,
    pub level: Level,
    pub result: GameResult,
    pub ending: Ending,
    pub blunder_check: bool,
    pub start_fen: String,
    pub moves: Vec<Move>,
    /// One entry per position of the game, filled in as the review runs.
    pub analysis: Vec<AnalysedPosition>,
}

impl GameRecord {
    /// The starting position, without evaluation state so it is cheap to copy.
    pub fn start(&self) -> Result<Board, String> {
        Board::from_fen(&self.start_fen).map(|b| b.without_eval_state())
    }

    pub fn is_fully_analysed(&self) -> bool {
        self.analysis.len() == self.moves.len() + 1
    }

    /// The result from your side.
    pub fn outcome(&self) -> super::rating::Outcome {
        use super::rating::Outcome;
        match (self.result, self.human) {
            (GameResult::Draw, _) => Outcome::Draw,
            (GameResult::WhiteWin, Color::White) | (GameResult::BlackWin, Color::Black) => {
                Outcome::Win
            }
            _ => Outcome::Loss,
        }
    }
}

/// Something you can practise and get right or wrong.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TrainingItem {
    Puzzle(PuzzleId),
    Opening(String),
    Endgame(String),
}

impl TrainingItem {
    pub fn key(&self) -> String {
        match self {
            Self::Puzzle(id) => format!("puzzle:{}", id.key()),
            Self::Opening(id) => format!("opening:{id}"),
            Self::Endgame(id) => format!("endgame:{id}"),
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        let (kind, rest) = key.split_once(':')?;
        match kind {
            "puzzle" => PuzzleId::from_key(rest).map(Self::Puzzle),
            "opening" => Some(Self::Opening(rest.to_string())),
            "endgame" => Some(Self::Endgame(rest.to_string())),
            _ => None,
        }
    }
}

/// One try at a training item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptRecord {
    pub at: UnixTime,
    pub item: TrainingItem,
    pub success: bool,
    /// The puzzle's rating, for rated puzzles.
    pub rating: Option<Elo>,
    /// Your moves, for endgame drills.
    pub moves: Option<u32>,
}

/// File contents with the numbers of any lines that could not be read.
#[derive(Debug, Default)]
pub struct Loaded<T> {
    pub items: Vec<T>,
    pub bad_lines: Vec<usize>,
}

/// The coach's data directory.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Opens `dir`, creating it if needed.
    pub fn open(dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// `$XDG_DATA_HOME/chess/coach`, or `~/.local/share/chess/coach`.
    pub fn default_dir() -> Option<PathBuf> {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })?;
        Some(data_home.join("chess").join("coach"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, file: &str) -> PathBuf {
        self.dir.join(file)
    }

    pub fn load_games(&self) -> io::Result<Loaded<GameRecord>> {
        read_lines(&self.path(GAMES_FILE), parse_game)
    }

    pub fn append_game(&self, game: &GameRecord) -> io::Result<()> {
        append_line(&self.path(GAMES_FILE), &format_game(game))
    }

    /// Replaces the stored analysis of `game`, keeping every other line as it was.
    pub fn save_analysis(&self, game: GameId, analysis: &[AnalysedPosition]) -> io::Result<()> {
        let path = self.path(GAMES_FILE);
        let text = fs::read_to_string(&path)?;
        let mut out = String::with_capacity(text.len());
        for line in text.lines() {
            match parse_game(line) {
                Some(mut record) if record.id == game => {
                    record.analysis = analysis.to_vec();
                    out.push_str(&format_game(&record));
                }
                _ => out.push_str(line),
            }
            out.push('\n');
        }
        write_atomically(&path, &out)
    }

    pub fn load_attempts(&self) -> io::Result<Loaded<AttemptRecord>> {
        read_lines(&self.path(ATTEMPTS_FILE), parse_attempt)
    }

    pub fn append_attempt(&self, attempt: &AttemptRecord) -> io::Result<()> {
        append_line(&self.path(ATTEMPTS_FILE), &format_attempt(attempt))
    }

    pub fn load_lichess_puzzles(&self) -> io::Result<Loaded<LichessPuzzle>> {
        read_lines(&self.path(LICHESS_FILE), lichess_from_line)
    }

    /// Writes the chosen puzzles, replacing any earlier import.
    pub fn save_lichess_puzzles(&self, puzzles: &[LichessPuzzle]) -> io::Result<()> {
        let mut text = String::new();
        for p in puzzles {
            text.push_str(&lichess_to_line(p));
            text.push('\n');
        }
        write_atomically(&self.path(LICHESS_FILE), &text)
    }
}

fn read_lines<T>(path: &Path, parse: impl Fn(&str) -> Option<T>) -> io::Result<Loaded<T>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Loaded {
                items: Vec::new(),
                bad_lines: Vec::new(),
            });
        }
        Err(e) => return Err(e),
    };
    let mut loaded = Loaded {
        items: Vec::new(),
        bad_lines: Vec::new(),
    };
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match parse(&line) {
            Some(item) => loaded.items.push(item),
            None => loaded.bad_lines.push(index + 1),
        }
    }
    Ok(loaded)
}

fn append_line(path: &Path, line: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

fn write_atomically(path: &Path, text: &str) -> io::Result<()> {
    let temporary = path.with_extension("tmp");
    {
        let mut writer = BufWriter::new(File::create(&temporary)?);
        writer.write_all(text.as_bytes())?;
        writer
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all()?;
    }
    fs::rename(&temporary, path)
}

fn color_token(color: Color) -> &'static str {
    if color == Color::White { "w" } else { "b" }
}

fn color_from_token(token: &str) -> Option<Color> {
    match token {
        "w" => Some(Color::White),
        "b" => Some(Color::Black),
        _ => None,
    }
}

fn result_token(result: GameResult) -> &'static str {
    match result {
        GameResult::WhiteWin => "1-0",
        GameResult::BlackWin => "0-1",
        GameResult::Draw => "1/2-1/2",
    }
}

fn result_from_token(token: &str) -> Option<GameResult> {
    match token {
        "1-0" => Some(GameResult::WhiteWin),
        "0-1" => Some(GameResult::BlackWin),
        "1/2-1/2" => Some(GameResult::Draw),
        _ => None,
    }
}

fn eval_token(eval: Eval) -> String {
    match eval {
        Eval::Checkmated => "mated".to_string(),
        Eval::Score(Score::Centipawns(cp)) => format!("cp:{cp}"),
        Eval::Score(Score::MateIn(moves)) => format!("mate:{moves}"),
    }
}

fn eval_from_token(token: &str) -> Option<Eval> {
    if token == "mated" {
        return Some(Eval::Checkmated);
    }
    let (kind, value) = token.split_once(':')?;
    let value: i32 = value.parse().ok()?;
    match kind {
        "cp" => Some(Eval::Score(Score::Centipawns(value))),
        "mate" => Some(Eval::Score(Score::MateIn(value))),
        _ => None,
    }
}

fn moves_text(moves: &[Move]) -> String {
    moves
        .iter()
        .map(|&m| format_uci(m))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Reads UCI moves played in order from `board`.
fn moves_from_text(board: &Board, text: &str) -> Option<Vec<Move>> {
    let mut board = board.clone();
    text.split_whitespace()
        .map(|uci| {
            let m = parse_uci_move(&mut board, uci)?;
            board.make_move(m);
            Some(m)
        })
        .collect()
}

fn format_game(game: &GameRecord) -> String {
    let analysis: Vec<String> = game
        .analysis
        .iter()
        .map(|a| format!("{}|{}", eval_token(a.eval), moves_text(&a.best_line)))
        .collect();
    [
        GAME_TAG.to_string(),
        game.id.0.to_string(),
        game.played_at.0.to_string(),
        color_token(game.human).to_string(),
        game.level.number().to_string(),
        result_token(game.result).to_string(),
        game.ending.token().to_string(),
        u8::from(game.blunder_check).to_string(),
        game.start_fen.clone(),
        moves_text(&game.moves),
        analysis.join(";"),
    ]
    .join("\t")
}

fn parse_game(line: &str) -> Option<GameRecord> {
    let fields: Vec<&str> = line.split('\t').collect();
    let [
        tag,
        id,
        played_at,
        human,
        level,
        result,
        ending,
        blunder_check,
        start_fen,
        moves,
        analysis,
    ] = fields.as_slice()
    else {
        return None;
    };
    if *tag != GAME_TAG {
        return None;
    }
    let start = Board::from_fen(start_fen).ok()?.without_eval_state();
    let moves = moves_from_text(&start, moves)?;
    let analysis = parse_analysis(&start, &moves, analysis)?;
    Some(GameRecord {
        id: GameId(id.parse().ok()?),
        played_at: UnixTime(played_at.parse().ok()?),
        human: color_from_token(human)?,
        level: Level::new(level.parse().ok()?)?,
        result: result_from_token(result)?,
        ending: Ending::from_token(ending)?,
        blunder_check: *blunder_check == "1",
        start_fen: (*start_fen).to_string(),
        moves,
        analysis,
    })
}

fn parse_analysis(start: &Board, moves: &[Move], text: &str) -> Option<Vec<AnalysedPosition>> {
    if text.is_empty() {
        return Some(Vec::new());
    }
    let mut board = start.clone();
    let mut analysis = Vec::new();
    for (i, entry) in text.split(';').enumerate() {
        let (eval, line) = entry.split_once('|')?;
        analysis.push(AnalysedPosition {
            eval: eval_from_token(eval)?,
            best_line: moves_from_text(&board, line)?,
        });
        if let Some(&m) = moves.get(i) {
            board.make_move(m);
        }
    }
    Some(analysis)
}

fn format_attempt(attempt: &AttemptRecord) -> String {
    let optional = |value: Option<String>| value.unwrap_or_else(|| "-".to_string());
    [
        ATTEMPT_TAG.to_string(),
        attempt.at.0.to_string(),
        attempt.item.key(),
        u8::from(attempt.success).to_string(),
        optional(attempt.rating.map(|r| r.0.to_string())),
        optional(attempt.moves.map(|m| m.to_string())),
    ]
    .join("\t")
}

fn parse_attempt(line: &str) -> Option<AttemptRecord> {
    let fields: Vec<&str> = line.split('\t').collect();
    let [tag, at, item, success, rating, moves] = fields.as_slice() else {
        return None;
    };
    if *tag != ATTEMPT_TAG {
        return None;
    }
    Some(AttemptRecord {
        at: UnixTime(at.parse().ok()?),
        item: TrainingItem::from_key(item)?,
        success: match *success {
            "1" => true,
            "0" => false,
            _ => return None,
        },
        rating: optional_field::<i32>(rating)?.map(Elo),
        moves: optional_field(moves)?,
    })
}

/// A field that is `-` when absent: Some(None) for `-`, None if it does not parse.
fn optional_field<T: std::str::FromStr>(text: &str) -> Option<Option<T>> {
    if text == "-" {
        Some(None)
    } else {
        text.parse().ok().map(Some)
    }
}

/// Which Lichess puzzles to keep from the full database.
#[derive(Clone, Copy, Debug)]
pub struct ImportFilter {
    pub min_rating: i32,
    pub max_rating: i32,
    pub min_popularity: i32,
    pub min_plays: u32,
    /// Puzzles kept per 100-point rating band.
    pub per_band: usize,
}

impl Default for ImportFilter {
    fn default() -> Self {
        Self {
            min_rating: 400,
            max_rating: 2400,
            min_popularity: 85,
            min_plays: 500,
            per_band: 2500,
        }
    }
}

/// Reads the Lichess puzzle CSV and keeps a random sample of good puzzles in each rating
/// band (reservoir sampling, so the whole file never has to be in memory).
pub fn sample_lichess_csv(
    reader: impl BufRead,
    filter: ImportFilter,
    rng: &mut impl rand::Rng,
) -> io::Result<Vec<LichessPuzzle>> {
    let band_of = |rating: i32| ((rating - filter.min_rating) / 100) as usize;
    let bands = band_of(filter.max_rating) + 1;
    let mut reservoirs: Vec<Vec<LichessPuzzle>> = vec![Vec::new(); bands];
    let mut seen = vec![0usize; bands];
    for line in reader.lines() {
        let line = line?;
        let Some(puzzle) = parse_lichess_csv_row(&line) else {
            continue;
        };
        let rating = puzzle.rating.0;
        let wanted = (filter.min_rating..=filter.max_rating).contains(&rating)
            && puzzle.popularity >= filter.min_popularity
            && puzzle.plays >= filter.min_plays;
        if !wanted {
            continue;
        }
        let band = band_of(rating);
        seen[band] += 1;
        if reservoirs[band].len() < filter.per_band {
            reservoirs[band].push(puzzle);
        } else {
            let slot = rng.gen_range(0..seen[band]);
            if slot < filter.per_band {
                reservoirs[band][slot] = puzzle;
            }
        }
    }
    Ok(reservoirs.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notation::parse_san_line;
    use crate::types::START_FEN;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn temp_store(name: &str) -> Store {
        let dir =
            std::env::temp_dir().join(format!("chess-coach-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Store::open(dir).unwrap()
    }

    fn sample_game(id: u64) -> GameRecord {
        let start = Board::from_fen(START_FEN).unwrap();
        let moves = parse_san_line(&start, &["f3", "e5", "g4", "Qh4#"]).unwrap();
        GameRecord {
            id: GameId(id),
            played_at: UnixTime(1_700_000_000),
            human: Color::White,
            level: Level::new(3).unwrap(),
            result: GameResult::BlackWin,
            ending: Ending::Checkmate,
            blunder_check: true,
            start_fen: START_FEN.to_string(),
            moves,
            analysis: Vec::new(),
        }
    }

    #[test]
    fn games_read_back_as_they_were_written() {
        let store = temp_store("games");
        let game = sample_game(1);

        store.append_game(&game).unwrap();
        let loaded = store.load_games().unwrap();

        assert!(loaded.bad_lines.is_empty());
        let back = &loaded.items[0];
        assert_eq!(
            (back.id, back.human, back.level, back.result),
            (game.id, game.human, game.level, game.result)
        );
        assert_eq!(back.moves, game.moves);
        assert_eq!(back.ending, Ending::Checkmate);
        assert!(back.blunder_check);
    }

    #[test]
    fn saved_analysis_replaces_only_its_own_game() {
        let store = temp_store("analysis");
        store.append_game(&sample_game(1)).unwrap();
        store.append_game(&sample_game(2)).unwrap();
        let start = Board::from_fen(START_FEN).unwrap();
        let analysis = vec![
            AnalysedPosition {
                eval: Eval::Score(Score::Centipawns(25)),
                best_line: parse_san_line(&start, &["e4", "e5"]).unwrap(),
            },
            AnalysedPosition {
                eval: Eval::Score(Score::MateIn(-3)),
                best_line: Vec::new(),
            },
        ];

        store.save_analysis(GameId(2), &analysis).unwrap();
        let games = store.load_games().unwrap().items;

        assert!(games[0].analysis.is_empty());
        assert_eq!(games[1].analysis, analysis);
    }

    #[test]
    fn attempts_read_back_and_bad_lines_are_reported() {
        let store = temp_store("attempts");
        let attempt = AttemptRecord {
            at: UnixTime(5),
            item: TrainingItem::Puzzle(PuzzleId::Lichess("abc".into())),
            success: true,
            rating: Some(Elo(900)),
            moves: None,
        };
        store.append_attempt(&attempt).unwrap();
        append_line(&store.path(ATTEMPTS_FILE), "garbage").unwrap();

        let loaded = store.load_attempts().unwrap();

        assert_eq!(loaded.items, vec![attempt]);
        assert_eq!(loaded.bad_lines, vec![2]);
    }

    #[test]
    fn a_missing_file_is_empty() {
        let store = temp_store("missing");

        assert!(store.load_games().unwrap().items.is_empty());
    }

    #[test]
    fn importing_keeps_only_good_puzzles_in_range() {
        let csv = "PuzzleId,FEN,Moves,Rating,RatingDeviation,Popularity,NbPlays,Themes,GameUrl,OpeningTags\n\
            good,q3k1nr/1pp1nQpp/3p4/1P2p3/4P3/B1PP1b2/B5PP/5K2 b k - 0 17,e8d7 a2e6 d7d8 f7f8,900,80,95,5000,mateIn2,url,\n\
            unpopular,q3k1nr/1pp1nQpp/3p4/1P2p3/4P3/B1PP1b2/B5PP/5K2 b k - 0 17,e8d7 a2e6 d7d8 f7f8,900,80,10,5000,mateIn2,url,\n\
            toohard,q3k1nr/1pp1nQpp/3p4/1P2p3/4P3/B1PP1b2/B5PP/5K2 b k - 0 17,e8d7 a2e6 d7d8 f7f8,2900,80,95,5000,mateIn2,url,\n";
        let mut rng = StdRng::seed_from_u64(1);

        let kept = sample_lichess_csv(csv.as_bytes(), ImportFilter::default(), &mut rng).unwrap();

        let ids: Vec<&str> = kept.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["good"]);
    }

    #[test]
    fn training_items_survive_their_text_form() {
        for item in [
            TrainingItem::Opening("w-slav".into()),
            TrainingItem::Endgame("lucena".into()),
            TrainingItem::Puzzle(PuzzleId::Game { game: 3, ply: 9 }),
        ] {
            assert_eq!(TrainingItem::from_key(&item.key()), Some(item));
        }
    }
}
