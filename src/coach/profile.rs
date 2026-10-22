//! What your history says: ratings, recurring mistakes, what to practise next and today's plan.

use super::bot::Level;
use super::endgames::{DRILLS, Drill};
use super::habits::{Habit, HabitNote, opening_habits};
use super::judgement::{Phase, overall_accuracy};
use super::puzzles::{LichessPuzzle, Puzzle, PuzzleId};
use super::rating::{Elo, Outcome, rating_after};
use super::repertoire::{Line, all_lines};
use super::review::{CauseKind, MoveReview, review_moves};
use super::srs::{Attempt, Schedule, UnixTime, schedule};
use super::store::{AttemptRecord, GameRecord, TrainingItem};
use crate::board::Board;
use crate::types::Move;
use rand::Rng;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Games in the "recent" window that trends compare against the window before it.
pub const RECENT_GAMES: usize = 10;
/// Fresh puzzles are picked within this many points of your puzzle rating.
const PUZZLE_RATING_WINDOW: i32 = 150;
/// Chance a fresh puzzle is picked from your weakest themes rather than any theme.
const WEAK_THEME_SHARE: f64 = 0.6;

/// Whether a move counts as a known opening move.
pub type IsBook<'a> = &'a dyn Fn(&Board, Move) -> bool;

/// A stored game with its moves judged.
#[derive(Clone, Debug)]
pub struct ReviewedGame {
    pub record: GameRecord,
    pub reviews: Vec<MoveReview>,
    pub habits: Vec<HabitNote>,
}

impl ReviewedGame {
    pub fn new(record: GameRecord, is_book: IsBook) -> Self {
        let (reviews, habits) = match record.start() {
            Ok(start) => (
                review_moves(&start, &record.moves, &record.analysis, is_book),
                opening_habits(&start, &record.moves, record.human),
            ),
            Err(_) => (Vec::new(), Vec::new()),
        };
        Self {
            record,
            reviews,
            habits,
        }
    }

    pub fn your_moves(&self) -> impl Iterator<Item = &MoveReview> {
        self.reviews.iter().filter(|r| r.mover == self.record.human)
    }

    pub fn cause_counts(&self) -> BTreeMap<CauseKind, usize> {
        let mut counts = BTreeMap::new();
        for cause in self
            .your_moves()
            .filter(|r| r.class.is_serious())
            .filter_map(|r| r.cause.as_ref())
        {
            *counts.entry(cause.kind()).or_insert(0) += 1;
        }
        counts
    }

    /// The single most useful lesson from the game, if it had errors.
    pub fn main_lesson(&self) -> Option<CauseKind> {
        self.cause_counts()
            .into_iter()
            .max_by_key(|&(kind, count)| (count, std::cmp::Reverse(kind)))
            .map(|(k, _)| k)
    }
}

/// Your play over a run of games.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PeriodStats {
    pub games: usize,
    pub accuracy: Option<f64>,
    pub phase_accuracy: BTreeMap<Phase, f64>,
    /// Mistakes and blunders per game, by cause.
    pub errors_per_game: BTreeMap<CauseKind, f64>,
    pub blunders_per_game: f64,
    pub habits: BTreeMap<Habit, usize>,
}

impl PeriodStats {
    pub fn of(games: &[&ReviewedGame]) -> Self {
        let analysed: Vec<&&ReviewedGame> = games
            .iter()
            .filter(|g| g.record.is_fully_analysed())
            .collect();
        let n = analysed.len();
        if n == 0 {
            return Self {
                games: games.len(),
                ..Self::default()
            };
        }
        let moves: Vec<&MoveReview> = analysed.iter().flat_map(|g| g.your_moves()).collect();
        let accuracy_of = |filter: &dyn Fn(&MoveReview) -> bool| {
            let values: Vec<f64> = moves
                .iter()
                .filter(|r| filter(r))
                .map(|r| r.accuracy())
                .collect();
            overall_accuracy(&values)
        };
        let mut errors_per_game = BTreeMap::new();
        for game in &analysed {
            for (kind, count) in game.cause_counts() {
                *errors_per_game.entry(kind).or_insert(0.0) += count as f64 / n as f64;
            }
        }
        let mut habits = BTreeMap::new();
        for note in games.iter().flat_map(|g| g.habits.iter()) {
            *habits.entry(note.habit).or_insert(0) += 1;
        }
        let blunders = moves
            .iter()
            .filter(|r| r.class == super::judgement::MoveClass::Blunder)
            .count();
        Self {
            games: games.len(),
            accuracy: accuracy_of(&|_| true),
            phase_accuracy: Phase::ALL
                .into_iter()
                .filter_map(|phase| accuracy_of(&|r| r.phase == phase).map(|a| (phase, a)))
                .collect(),
            errors_per_game,
            blunders_per_game: blunders as f64 / n as f64,
            habits,
        }
    }

    pub fn errors(&self, kind: CauseKind) -> f64 {
        self.errors_per_game.get(&kind).copied().unwrap_or(0.0)
    }
}

/// Something to work on, worst first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Focus {
    Mistakes { kind: CauseKind, per_game: f64 },
    Habit { habit: Habit, games: usize },
    Phase { phase: Phase, accuracy: f64 },
}

#[derive(Clone, Debug)]
pub struct Profile {
    pub rating: Elo,
    /// Your rating after each game, oldest first.
    pub rating_history: Vec<Elo>,
    pub wins: usize,
    pub draws: usize,
    pub losses: usize,
    pub puzzle_rating: Elo,
    pub puzzles_tried: usize,
    pub puzzles_solved: usize,
    pub recent: PeriodStats,
    pub earlier: Option<PeriodStats>,
    pub focus: Vec<Focus>,
    pub streak_days: usize,
}

impl Profile {
    pub fn games(&self) -> usize {
        self.wins + self.draws + self.losses
    }

    /// The level that is an even match for you now.
    pub fn suggested_level(&self) -> Level {
        Level::matching(self.rating)
    }

    pub fn build(games: &[ReviewedGame], attempts: &[AttemptRecord], now: UnixTime) -> Self {
        let results: Vec<(Elo, Outcome)> = games
            .iter()
            .map(|g| (g.record.level.spec().elo, g.record.outcome()))
            .collect();
        let rating_history: Vec<Elo> = (1..=results.len())
            .map(|n| rating_after(results[..n].iter().copied()))
            .collect();
        let count = |outcome: Outcome| results.iter().filter(|(_, o)| *o == outcome).count();
        let rated_puzzles: Vec<(Elo, Outcome)> = attempts
            .iter()
            .filter(|a| matches!(a.item, TrainingItem::Puzzle(PuzzleId::Lichess(_))))
            .filter_map(|a| {
                a.rating.map(|r| {
                    (
                        r,
                        if a.success {
                            Outcome::Win
                        } else {
                            Outcome::Loss
                        },
                    )
                })
            })
            .collect();
        let puzzle_attempts = attempts
            .iter()
            .filter(|a| matches!(a.item, TrainingItem::Puzzle(_)));
        let recent_start = games.len().saturating_sub(RECENT_GAMES);
        let recent_games: Vec<&ReviewedGame> = games[recent_start..].iter().collect();
        let earlier_start = recent_start.saturating_sub(RECENT_GAMES);
        let earlier_games: Vec<&ReviewedGame> = games[earlier_start..recent_start].iter().collect();
        let recent = PeriodStats::of(&recent_games);
        Self {
            rating: rating_history.last().copied().unwrap_or(Elo::START),
            rating_history,
            wins: count(Outcome::Win),
            draws: count(Outcome::Draw),
            losses: count(Outcome::Loss),
            puzzle_rating: rating_after(rated_puzzles),
            puzzles_tried: puzzle_attempts.clone().count(),
            puzzles_solved: puzzle_attempts.filter(|a| a.success).count(),
            focus: focus_areas(&recent),
            earlier: (!earlier_games.is_empty()).then(|| PeriodStats::of(&earlier_games)),
            recent,
            streak_days: streak_days(games, attempts, now),
        }
    }
}

/// The three things most worth working on, worst first.
fn focus_areas(stats: &PeriodStats) -> Vec<Focus> {
    const MIN_ERRORS_PER_GAME: f64 = 0.3;
    const WEAK_PHASE_ACCURACY: f64 = 70.0;
    let mut mistakes: Vec<(CauseKind, f64)> = stats
        .errors_per_game
        .iter()
        .filter(|&(&kind, &rate)| kind != CauseKind::Positional && rate >= MIN_ERRORS_PER_GAME)
        .map(|(&kind, &rate)| (kind, rate))
        .collect();
    mistakes.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut focus: Vec<Focus> = mistakes
        .into_iter()
        .map(|(kind, per_game)| Focus::Mistakes { kind, per_game })
        .collect();
    if let Some((&habit, &games)) = stats.habits.iter().max_by_key(|&(_, &count)| count)
        && games * 3 >= stats.games.max(1)
    {
        focus.push(Focus::Habit { habit, games });
    }
    if let Some((&phase, &accuracy)) = stats
        .phase_accuracy
        .iter()
        .min_by(|a, b| a.1.total_cmp(b.1))
        && accuracy < WEAK_PHASE_ACCURACY
    {
        focus.push(Focus::Phase { phase, accuracy });
    }
    focus.truncate(3);
    focus
}

/// Days in a row, ending today or yesterday, with a game or any practice.
fn streak_days(games: &[ReviewedGame], attempts: &[AttemptRecord], now: UnixTime) -> usize {
    let days: HashSet<u64> = games
        .iter()
        .map(|g| g.record.played_at.day())
        .chain(attempts.iter().map(|a| a.at.day()))
        .collect();
    let today = now.day();
    let mut day = if days.contains(&today) {
        today
    } else {
        today.saturating_sub(1)
    };
    let mut streak = 0;
    while days.contains(&day) {
        streak += 1;
        let Some(previous) = day.checked_sub(1) else {
            break;
        };
        day = previous;
    }
    streak
}

/// Each training item's schedule from its attempts.
pub fn schedules(attempts: &[AttemptRecord]) -> HashMap<TrainingItem, Schedule> {
    let mut by_item: HashMap<TrainingItem, Vec<Attempt>> = HashMap::new();
    for a in attempts {
        by_item.entry(a.item.clone()).or_default().push(Attempt {
            at: a.at,
            success: a.success,
        });
    }
    by_item
        .into_iter()
        .filter_map(|(item, mut tries)| {
            tries.sort_by_key(|t| t.at);
            schedule(&tries).map(|s| (item, s))
        })
        .collect()
}

/// Lichess themes that train against each kind of mistake.
pub const fn themes_for(kind: CauseKind) -> &'static [&'static str] {
    match kind {
        CauseKind::HungPiece => &["hangingPiece"],
        CauseKind::IgnoredThreat => &["defensiveMove", "hangingPiece"],
        CauseKind::AllowedTactic => &["fork", "pin", "skewer", "discoveredAttack"],
        CauseKind::AllowedMate => &["mateIn1", "mateIn2", "backRankMate"],
        CauseKind::MissedMate => &["mateIn1", "mateIn2"],
        CauseKind::MissedMaterial => &["hangingPiece", "fork", "trappedPiece"],
        CauseKind::Positional => &[],
    }
}

/// Puzzles made from your serious mistakes: find the move you should have played.
pub fn puzzles_from_games(games: &[ReviewedGame]) -> Vec<Puzzle> {
    let mut puzzles = Vec::new();
    for game in games {
        let Ok(mut board) = game.record.start().map(|b| b.without_eval_state()) else {
            continue;
        };
        let mut previous = None;
        let mut reviews = game.reviews.iter().peekable();
        for (ply, &m) in game.record.moves.iter().enumerate() {
            let review = reviews.next_if(|r| r.ply == ply);
            if let Some(review) = review
                && review.mover == game.record.human
                && review.class.is_serious()
                && let Some(best) = review.best
            {
                let themes = review
                    .cause
                    .as_ref()
                    .map(|c| {
                        themes_for(c.kind())
                            .iter()
                            .map(|t| (*t).to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                puzzles.push(Puzzle {
                    id: PuzzleId::Game {
                        game: game.record.id.0,
                        ply,
                    },
                    board: board.clone(),
                    last_move: previous,
                    solution: vec![best],
                    rating: None,
                    themes,
                });
            }
            board.make_move(m);
            previous = Some(m);
        }
    }
    puzzles
}

/// Why a puzzle was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PuzzleReason {
    /// A puzzle you missed or are reviewing, due again.
    Review,
    /// A mistake from one of your games, not tried yet.
    FromYourGame,
    /// A new puzzle on a theme you need.
    WeakTheme,
    New,
}

impl PuzzleReason {
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Review => "Review: you've seen this one before",
            Self::FromYourGame => "From one of your games",
            Self::WeakTheme => "Practice for your weak spots",
            Self::New => "New puzzle",
        }
    }
}

/// What puzzle selection needs to know.
pub struct PuzzlePool<'a> {
    pub own: &'a [Puzzle],
    pub lichess: &'a [LichessPuzzle],
    pub schedules: &'a HashMap<TrainingItem, Schedule>,
    pub rating: Elo,
    pub weak_themes: &'a [&'a str],
}

impl PuzzlePool<'_> {
    fn schedule_of(&self, id: &PuzzleId) -> Option<Schedule> {
        self.schedules
            .get(&TrainingItem::Puzzle(id.clone()))
            .copied()
    }

    /// Due reviews first, then untried mistakes from your games, then a fresh puzzle near
    /// your rating, usually on a theme you need. `skip` is the puzzle just played.
    pub fn next(
        &self,
        now: UnixTime,
        skip: Option<&PuzzleId>,
        rng: &mut impl Rng,
    ) -> Option<(Puzzle, PuzzleReason)> {
        let not_skipped = |id: &PuzzleId| Some(id) != skip;
        let is_due_review = |id: &PuzzleId| {
            not_skipped(id)
                && self
                    .schedule_of(id)
                    .is_some_and(|s| s.is_due(now) && !s.is_learned())
        };
        let due_lichess = self
            .lichess
            .iter()
            .filter(|p| is_due_review(&PuzzleId::Lichess(p.id.clone())))
            .filter_map(LichessPuzzle::to_puzzle);
        let due = self
            .own
            .iter()
            .filter(|p| {
                not_skipped(&p.id) && self.schedule_of(&p.id).is_some_and(|s| s.is_due(now))
            })
            .cloned()
            .chain(due_lichess)
            .min_by_key(|p| self.schedule_of(&p.id).map(|s| s.due));
        if let Some(puzzle) = due {
            return Some((puzzle, PuzzleReason::Review));
        }
        if let Some(puzzle) = self
            .own
            .iter()
            .find(|p| not_skipped(&p.id) && self.schedule_of(&p.id).is_none())
        {
            return Some((puzzle.clone(), PuzzleReason::FromYourGame));
        }
        self.fresh_lichess(skip, rng)
    }

    fn fresh_lichess(
        &self,
        skip: Option<&PuzzleId>,
        rng: &mut impl Rng,
    ) -> Option<(Puzzle, PuzzleReason)> {
        let near = |window: i32| -> Vec<&LichessPuzzle> {
            self.lichess
                .iter()
                .filter(|p| (p.rating.0 - self.rating.0).abs() <= window)
                .filter(|p| {
                    let id = PuzzleId::Lichess(p.id.clone());
                    Some(&id) != skip && self.schedule_of(&id).is_none()
                })
                .collect()
        };
        let mut candidates = near(PUZZLE_RATING_WINDOW);
        if candidates.is_empty() {
            candidates = near(i32::MAX);
        }
        let on_weak_theme: Vec<&LichessPuzzle> = candidates
            .iter()
            .copied()
            .filter(|p| {
                p.themes
                    .iter()
                    .any(|t| self.weak_themes.contains(&t.as_str()))
            })
            .collect();
        let (pool, reason) = if !on_weak_theme.is_empty() && rng.gen_bool(WEAK_THEME_SHARE) {
            (on_weak_theme, PuzzleReason::WeakTheme)
        } else {
            (candidates, PuzzleReason::New)
        };
        if pool.is_empty() {
            return None;
        }
        pool[rng.gen_range(0..pool.len())]
            .to_puzzle()
            .map(|p| (p, reason))
    }
}

/// Where you stand with an opening line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineStatus {
    New,
    Due,
    Learning,
    Learned,
}

pub fn line_status(
    line: &Line,
    schedules: &HashMap<TrainingItem, Schedule>,
    now: UnixTime,
) -> LineStatus {
    match schedules.get(&TrainingItem::Opening(line.id.to_string())) {
        None => LineStatus::New,
        Some(s) if s.is_due(now) => LineStatus::Due,
        Some(s) if s.is_learned() => LineStatus::Learned,
        Some(_) => LineStatus::Learning,
    }
}

/// One suggestion for today.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Task {
    ReviewPuzzles(usize),
    ReviewLines(usize),
    LearnLine(&'static Line),
    Endgame(&'static Drill),
    Play(Level),
}

/// A short plan for today: what's due first, then something new, then a game.
pub fn daily_plan(
    profile: &Profile,
    own_puzzles: &[Puzzle],
    schedules: &HashMap<TrainingItem, Schedule>,
    now: UnixTime,
) -> Vec<Task> {
    let mut plan = Vec::new();
    let due_puzzles = schedules
        .iter()
        .filter(|(item, s)| {
            matches!(item, TrainingItem::Puzzle(_)) && s.is_due(now) && !s.is_learned()
        })
        .count();
    let untried_own = own_puzzles
        .iter()
        .filter(|p| !schedules.contains_key(&TrainingItem::Puzzle(p.id.clone())))
        .count();
    if due_puzzles + untried_own > 0 {
        plan.push(Task::ReviewPuzzles(due_puzzles + untried_own));
    }
    let due_lines = all_lines()
        .filter(|l| line_status(l, schedules, now) == LineStatus::Due)
        .count();
    if due_lines > 0 {
        plan.push(Task::ReviewLines(due_lines));
    } else if let Some(line) =
        all_lines().find(|l| line_status(l, schedules, now) == LineStatus::New)
    {
        plan.push(Task::LearnLine(line));
    }
    let solved = |drill: &Drill| {
        schedules
            .get(&TrainingItem::Endgame(drill.id.to_string()))
            .is_some_and(|s| s.level > 0)
    };
    if let Some(drill) = DRILLS.iter().find(|d| !solved(d)) {
        plan.push(Task::Endgame(drill));
    }
    plan.push(Task::Play(profile.suggested_level()));
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coach::judgement::Eval;
    use crate::coach::review::AnalysedPosition;
    use crate::coach::store::{Ending, GameId};
    use crate::game_runner::GameResult;
    use crate::notation::parse_san_line;
    use crate::search::Score;
    use crate::types::{Color, START_FEN};
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    const DAY: u64 = 86_400;

    fn game(id: u64, sans: &[&str], evals: &[Eval], result: GameResult) -> ReviewedGame {
        let start = Board::from_fen(START_FEN).unwrap();
        let moves = parse_san_line(&start, sans).unwrap();
        let mut board = start.clone();
        let mut analysis = Vec::new();
        for (i, &eval) in evals.iter().enumerate() {
            let best_line = if i + 1 < evals.len() && i < moves.len() {
                vec![first_legal_other_than(&board, moves.get(i).copied())]
            } else {
                Vec::new()
            };
            analysis.push(AnalysedPosition { eval, best_line });
            if let Some(&m) = moves.get(i) {
                board.make_move(m);
            }
        }
        let record = GameRecord {
            id: GameId(id),
            played_at: UnixTime(id * DAY),
            human: Color::White,
            level: Level::new(2).unwrap(),
            result,
            ending: Ending::Checkmate,
            blunder_check: false,
            start_fen: START_FEN.to_string(),
            moves,
            analysis,
        };
        ReviewedGame::new(record, &|_, _| false)
    }

    fn first_legal_other_than(board: &Board, m: Option<Move>) -> Move {
        let mut legal = crate::types::MoveList::new();
        board.generate_legal_moves(&mut legal);
        legal
            .as_slice()
            .iter()
            .copied()
            .find(|&x| Some(x) != m)
            .unwrap()
    }

    fn cp(cp: i32) -> Eval {
        Eval::Score(Score::Centipawns(cp))
    }

    fn fools_mate(id: u64) -> ReviewedGame {
        game(
            id,
            &["f3", "e5", "g4", "Qh4#"],
            &[
                cp(20),
                cp(60),
                cp(-80),
                Eval::Score(Score::MateIn(1)),
                Eval::Checkmated,
            ],
            GameResult::BlackWin,
        )
    }

    #[test]
    fn losses_lower_the_rating_from_the_start() {
        let games = [fools_mate(1), fools_mate(2)];

        let profile = Profile::build(&games, &[], UnixTime(3 * DAY));

        assert_eq!(profile.losses, 2);
        assert!(profile.rating < Elo::START);
        assert_eq!(profile.rating_history.len(), 2);
    }

    #[test]
    fn a_blunder_into_mate_becomes_a_focus_and_a_puzzle() {
        let games = [fools_mate(1)];

        let profile = Profile::build(&games, &[], UnixTime(DAY));
        let puzzles = puzzles_from_games(&games);

        assert!(matches!(
            profile.focus.first(),
            Some(Focus::Mistakes {
                kind: CauseKind::AllowedMate,
                ..
            })
        ));
        assert_eq!(puzzles.len(), 1);
        assert_eq!(puzzles[0].id, PuzzleId::Game { game: 1, ply: 2 });
    }

    #[test]
    fn the_streak_counts_consecutive_active_days() {
        let games = [
            fools_mate(10),
            fools_mate(11),
            fools_mate(13),
            fools_mate(14),
        ];

        let streak = Profile::build(&games, &[], UnixTime(15 * DAY + 100)).streak_days;

        assert_eq!(streak, 2);
    }

    #[test]
    fn untried_mistakes_come_before_fresh_puzzles() {
        let games = [fools_mate(1)];
        let own = puzzles_from_games(&games);
        let schedules = HashMap::new();
        let pool = PuzzlePool {
            own: &own,
            lichess: &[],
            schedules: &schedules,
            rating: Elo(600),
            weak_themes: &[],
        };
        let mut rng = StdRng::seed_from_u64(1);

        let (puzzle, reason) = pool.next(UnixTime(DAY), None, &mut rng).unwrap();

        assert_eq!(reason, PuzzleReason::FromYourGame);
        assert_eq!(puzzle.id, own[0].id);
    }

    #[test]
    fn a_missed_puzzle_comes_back_once_due() {
        let games = [fools_mate(1)];
        let own = puzzles_from_games(&games);
        let missed = AttemptRecord {
            at: UnixTime(DAY),
            item: TrainingItem::Puzzle(own[0].id.clone()),
            success: false,
            rating: None,
            moves: None,
        };
        let schedules = schedules(&[missed]);
        let pool = PuzzlePool {
            own: &own,
            lichess: &[],
            schedules: &schedules,
            rating: Elo(600),
            weak_themes: &[],
        };
        let mut rng = StdRng::seed_from_u64(1);

        assert!(pool.next(UnixTime(DAY + 60), None, &mut rng).is_none());
        assert_eq!(
            pool.next(UnixTime(DAY + 3600), None, &mut rng)
                .map(|(_, r)| r),
            Some(PuzzleReason::Review)
        );
    }

    #[test]
    fn a_new_player_is_told_to_learn_a_line_and_play() {
        let profile = Profile::build(&[], &[], UnixTime(DAY));

        let plan = daily_plan(&profile, &[], &HashMap::new(), UnixTime(DAY));

        assert!(matches!(plan[0], Task::LearnLine(line) if line.id == "w-qgd-exchange"));
        assert!(matches!(plan.last(), Some(Task::Play(_))));
    }
}
