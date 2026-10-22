//! Searches for analysis and coaching: limited by time or nodes, or one per legal move.

use crate::board::Board;
use crate::game_runner::{GameEndReason, position_outcome};
use crate::search::{
    IterationInfo, NodeLimit, Score, SearchControl, SearchLimits, SearchThreads, search_reporting,
};
use crate::search_params::SearchParams;
use crate::time::TimeBudget;
use crate::tt::SharedTransTable;
use crate::types::{Move, MoveList};
use std::sync::Arc;

const MAX_DEPTH: usize = 64;
/// A node-limited search stops mid-iteration at this multiple of its node count.
const HARD_NODE_FACTOR: u64 = 4;

/// How long a search runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchLimit {
    /// Until the budget runs out, or until stopped when there is none.
    Time(Option<TimeBudget>),
    /// About this many nodes, on one thread so the result does not depend on timing.
    Nodes(u64),
}

/// What a finished search found; scores are for the side to move at its root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchOutcome {
    pub best_move: Option<Move>,
    pub score: Option<Score>,
    pub pv: Vec<Move>,
}

/// Search threads and a hash table kept between searches.
pub struct Searcher {
    tt: SharedTransTable,
    threads: SearchThreads,
    single_thread: SearchThreads,
}

impl Searcher {
    pub fn new(threads: usize, hash_mb: usize) -> Self {
        Self {
            tt: SharedTransTable::new(hash_mb),
            threads: SearchThreads::new(threads),
            single_thread: SearchThreads::new(1),
        }
    }

    /// Forgets the hash table and move-ordering statistics.
    pub fn clear(&mut self) {
        self.tt.clear();
        self.threads.clear();
        self.single_thread.clear();
    }

    pub fn search(
        &mut self,
        board: &Board,
        limit: SearchLimit,
        control: &Arc<SearchControl>,
        report: &(dyn Fn(&IterationInfo) + Sync),
    ) -> SearchOutcome {
        let (nodes, wait_for_stop, threads) = match limit {
            SearchLimit::Time(budget) => (None, budget.is_none(), &mut self.threads),
            SearchLimit::Nodes(nodes) => {
                let nodes = nodes.max(1);
                let limit = NodeLimit {
                    soft: nodes,
                    hard: nodes.saturating_mul(HARD_NODE_FACTOR),
                };
                (Some(limit), false, &mut self.single_thread)
            }
        };
        let limits = SearchLimits {
            max_depth: MAX_DEPTH,
            params: SearchParams::default(),
            wait_for_stop,
            nodes,
        };
        let last_pv = std::sync::Mutex::new(Vec::new());
        let reporter = |info: &IterationInfo| {
            if let Ok(mut pv) = last_pv.lock() {
                pv.clone_from(&info.pv);
            }
            report(info);
        };
        let result = search_reporting(board, &self.tt, limits, threads, control, &reporter);
        let pv = last_pv.into_inner().unwrap_or_default();
        SearchOutcome {
            best_move: result.best_move,
            score: result.score,
            pv,
        }
    }

    /// Scores each legal move for its mover, or None if stopped first.
    pub fn score_moves(
        &mut self,
        board: &Board,
        nodes_per_move: u64,
        control: &Arc<SearchControl>,
    ) -> Option<Vec<(Move, Score)>> {
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);
        let mut scores = Vec::with_capacity(legal.len());
        for &m in legal.as_slice() {
            let mut child = board.clone();
            child.make_move(m);
            let score = match position_outcome(&child) {
                Some((_, GameEndReason::Checkmate)) => Score::MateIn(1),
                Some(_) => Score::Centipawns(0),
                None => {
                    let child_control = SearchControl::new(None);
                    let outcome = self.search(
                        &child,
                        SearchLimit::Nodes(nodes_per_move),
                        &child_control,
                        &|_| {},
                    );
                    for_parent(outcome.score.unwrap_or(Score::Centipawns(0)))
                }
            };
            if control.is_stopped() {
                return None;
            }
            scores.push((m, score));
        }
        Some(scores)
    }
}

/// A child position's score seen from the side that moved into it.
const fn for_parent(child: Score) -> Score {
    match child {
        Score::Centipawns(cp) => Score::Centipawns(-cp),
        Score::MateIn(moves) if moves > 0 => Score::MateIn(-moves),
        Score::MateIn(moves) => Score::MateIn(-moves + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_scores_flip_to_the_parents_side() {
        assert_eq!(for_parent(Score::Centipawns(40)), Score::Centipawns(-40));
        assert_eq!(for_parent(Score::MateIn(2)), Score::MateIn(-2));
        assert_eq!(for_parent(Score::MateIn(-1)), Score::MateIn(2));
    }
}
