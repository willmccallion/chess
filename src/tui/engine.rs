use crate::analysis::Searcher;
pub use crate::analysis::{SearchLimit, SearchOutcome};
use crate::board::Board;
use crate::search::{IterationInfo, Score, SearchControl};
use crate::time::TimeBudget;
use crate::types::Move;
use std::io;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

/// Identifies one search, so replies from a search that was stopped can be ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchId(u64);

enum Request {
    Search {
        id: SearchId,
        board: Box<Board>,
        limit: SearchLimit,
        control: Arc<SearchControl>,
    },
    ScoreMoves {
        id: SearchId,
        board: Box<Board>,
        nodes_per_move: u64,
        control: Arc<SearchControl>,
    },
    NewGame,
}

pub enum Reply {
    Info {
        id: SearchId,
        info: IterationInfo,
    },
    Done {
        id: SearchId,
        outcome: SearchOutcome,
    },
    /// Every legal move with its score for the side that plays it.
    MoveScores {
        id: SearchId,
        scores: Vec<(Move, Score)>,
    },
}

/// A search thread pool running in the background, one search at a time.
pub struct Engine {
    requests: Option<Sender<Request>>,
    replies: Receiver<Reply>,
    running: Option<Arc<SearchControl>>,
    next_id: u64,
    worker: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn spawn(threads: usize, hash_mb: usize) -> io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("tui-engine".into())
            .spawn(move || serve(&request_rx, &reply_tx, threads, hash_mb))?;
        Ok(Self {
            requests: Some(request_tx),
            replies: reply_rx,
            running: None,
            next_id: 0,
            worker: Some(worker),
        })
    }

    /// Stops any running search and starts one on `board`.
    pub fn start(&mut self, board: &Board, limit: SearchLimit) -> SearchId {
        let budget = match limit {
            SearchLimit::Time(budget) => budget,
            SearchLimit::Nodes(_) => None,
        };
        let (id, control) = self.begin(budget);
        self.send(Request::Search {
            id,
            board: Box::new(board.clone()),
            limit,
            control,
        });
        id
    }

    /// Stops any running search and scores every legal move on `board` with its own search.
    pub fn score_moves(&mut self, board: &Board, nodes_per_move: u64) -> SearchId {
        let (id, control) = self.begin(None);
        self.send(Request::ScoreMoves {
            id,
            board: Box::new(board.clone()),
            nodes_per_move,
            control,
        });
        id
    }

    fn begin(&mut self, budget: Option<TimeBudget>) -> (SearchId, Arc<SearchControl>) {
        self.stop();
        self.next_id += 1;
        let control = SearchControl::new(budget);
        self.running = Some(Arc::clone(&control));
        (SearchId(self.next_id), control)
    }

    pub fn stop(&mut self) {
        if let Some(control) = self.running.take() {
            control.stop();
        }
    }

    /// Forgets the hash table and move-ordering statistics.
    pub fn new_game(&mut self) {
        self.stop();
        self.send(Request::NewGame);
    }

    pub fn try_reply(&self) -> Option<Reply> {
        self.replies.try_recv().ok()
    }

    fn send(&self, request: Request) {
        if let Some(requests) = &self.requests {
            let _ = requests.send(request);
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
        self.requests = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve(requests: &Receiver<Request>, replies: &Sender<Reply>, threads: usize, hash_mb: usize) {
    let mut searcher = Searcher::new(threads, hash_mb);
    for request in requests {
        match request {
            Request::Search {
                id,
                board,
                limit,
                control,
            } => {
                let report = |info: &IterationInfo| {
                    let _ = replies.send(Reply::Info {
                        id,
                        info: info.clone(),
                    });
                };
                let outcome = searcher.search(&board, limit, &control, &report);
                let _ = replies.send(Reply::Done { id, outcome });
            }
            Request::ScoreMoves {
                id,
                board,
                nodes_per_move,
                control,
            } => {
                if let Some(scores) = searcher.score_moves(&board, nodes_per_move, &control) {
                    let _ = replies.send(Reply::MoveScores { id, scores });
                }
            }
            Request::NewGame => searcher.clear(),
        }
    }
}
