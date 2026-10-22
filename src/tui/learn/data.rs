//! Everything learn mode knows about you, loaded from the store and kept up to date.

use super::super::app::PieceStyle;
use crate::board::Board;
use crate::coach::judgement::Eval;
use crate::coach::profile::{
    Profile, PuzzlePool, PuzzleReason, ReviewedGame, puzzles_from_games, schedules, themes_for,
};
use crate::coach::puzzles::{LichessPuzzle, Puzzle, PuzzleId};
use crate::coach::rating::Elo;
use crate::coach::review::AnalysedPosition;
use crate::coach::srs::{Schedule, UnixTime};
use crate::coach::store::{AttemptRecord, GameId, GameRecord, Store, TrainingItem};
use crate::opening_book::book_moves;
use crate::types::Move;
use rand::SeedableRng;
use rand::rngs::StdRng;
use std::collections::HashMap;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct CoachData {
    store: Store,
    pub games: Vec<ReviewedGame>,
    pub attempts: Vec<AttemptRecord>,
    pub lichess: Vec<LichessPuzzle>,
    pub profile: Profile,
    pub own_puzzles: Vec<Puzzle>,
    pub schedules: HashMap<TrainingItem, Schedule>,
    /// Problems worth telling you about, like unreadable lines in the saved files.
    pub warnings: Vec<String>,
    pub rng: StdRng,
    pub piece_style: PieceStyle,
}

/// Whether `m` is in the opening book here.
pub fn is_book(board: &Board, m: Move) -> bool {
    book_moves(board)
        .iter()
        .any(|w| w.m.from == m.from && w.m.to == m.to && w.m.promotion == m.promotion)
}

pub fn now() -> UnixTime {
    UnixTime(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    )
}

/// A fresh game id: the current time in milliseconds.
pub fn new_game_id() -> GameId {
    GameId(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64),
    )
}

impl CoachData {
    pub fn load(store: Store) -> io::Result<Self> {
        let games = store.load_games()?;
        let attempts = store.load_attempts()?;
        let lichess = store.load_lichess_puzzles()?;
        let mut warnings = Vec::new();
        for (file, bad) in [
            ("games", &games.bad_lines),
            ("attempts", &attempts.bad_lines),
            ("puzzles", &lichess.bad_lines),
        ] {
            if !bad.is_empty() {
                warnings.push(format!(
                    "Skipped {} unreadable line(s) in the {file} file",
                    bad.len()
                ));
            }
        }
        let reviewed: Vec<ReviewedGame> = games
            .items
            .into_iter()
            .map(|g| ReviewedGame::new(g, &is_book))
            .collect();
        let mut data = Self {
            profile: Profile::build(&[], &[], now()),
            store,
            games: reviewed,
            attempts: attempts.items,
            lichess: lichess.items,
            own_puzzles: Vec::new(),
            schedules: HashMap::new(),
            warnings,
            rng: StdRng::from_entropy(),
            piece_style: PieceStyle::Sprites,
        };
        data.refresh();
        Ok(data)
    }

    /// Recomputes everything derived from the games and attempts.
    fn refresh(&mut self) {
        self.profile = Profile::build(&self.games, &self.attempts, now());
        self.own_puzzles = puzzles_from_games(&self.games);
        self.schedules = schedules(&self.attempts);
    }

    pub fn store_dir(&self) -> &std::path::Path {
        self.store.dir()
    }

    pub fn game(&self, id: GameId) -> Option<&ReviewedGame> {
        self.games.iter().find(|g| g.record.id == id)
    }

    pub fn add_game(&mut self, record: GameRecord) -> io::Result<()> {
        self.store.append_game(&record)?;
        self.games.push(ReviewedGame::new(record, &is_book));
        self.refresh();
        Ok(())
    }

    pub fn save_analysis(&mut self, id: GameId, analysis: &[AnalysedPosition]) -> io::Result<()> {
        self.store.save_analysis(id, analysis)?;
        if let Some(game) = self.games.iter_mut().find(|g| g.record.id == id) {
            let mut record = game.record.clone();
            record.analysis = analysis.to_vec();
            *game = ReviewedGame::new(record, &is_book);
        }
        self.refresh();
        Ok(())
    }

    pub fn record_attempt(
        &mut self,
        item: TrainingItem,
        success: bool,
        rating: Option<Elo>,
        moves: Option<u32>,
    ) -> io::Result<()> {
        let attempt = AttemptRecord {
            at: now(),
            item,
            success,
            rating,
            moves,
        };
        self.store.append_attempt(&attempt)?;
        self.attempts.push(attempt);
        self.refresh();
        Ok(())
    }

    pub fn schedule(&self, item: &TrainingItem) -> Option<Schedule> {
        self.schedules.get(item).copied()
    }

    /// Themes that train against your most common mistakes.
    pub fn weak_themes(&self) -> Vec<&'static str> {
        self.profile
            .focus
            .iter()
            .filter_map(|focus| match focus {
                crate::coach::profile::Focus::Mistakes { kind, .. } => Some(themes_for(*kind)),
                _ => None,
            })
            .flatten()
            .copied()
            .collect()
    }

    pub fn next_puzzle(&mut self, skip: Option<&PuzzleId>) -> Option<(Puzzle, PuzzleReason)> {
        let weak = self.weak_themes();
        let pool = PuzzlePool {
            own: &self.own_puzzles,
            lichess: &self.lichess,
            schedules: &self.schedules,
            rating: self.profile.puzzle_rating,
            weak_themes: &weak,
        };
        pool.next(now(), skip, &mut self.rng)
    }

    /// The engine's verdict on the position before your move in a game, for judging
    /// puzzle answers from that game.
    pub fn game_eval_before(&self, id: &PuzzleId) -> Option<Eval> {
        let PuzzleId::Game { game, ply } = id else {
            return None;
        };
        self.game(GameId(*game))?
            .record
            .analysis
            .get(*ply)
            .map(|a| a.eval)
    }

    /// What you actually played in the game a puzzle comes from.
    pub fn move_played_in_game(&self, id: &PuzzleId) -> Option<String> {
        let PuzzleId::Game { game, ply } = id else {
            return None;
        };
        self.game(GameId(*game))?
            .reviews
            .iter()
            .find(|r| r.ply == *ply)
            .map(|r| r.numbered_san.clone())
    }
}
