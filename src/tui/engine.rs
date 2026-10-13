use crate::board::Board;
use crate::search::{IterationInfo, SearchControl, SearchLimits, SearchThreads, search_reporting};
use crate::search_params::SearchParams;
use crate::time::TimeBudget;
use crate::tt::SharedTransTable;
use crate::types::Move;
use std::io;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

const MAX_DEPTH: usize = 64;

/// Identifies one search, so replies from a search that was stopped can be ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchId(u64);

enum Request {
    Search {
        id: SearchId,
        board: Box<Board>,
        budget: Option<TimeBudget>,
        control: Arc<SearchControl>,
    },
    NewGame,
}

pub enum Reply {
    Info { id: SearchId, info: IterationInfo },
    Done { id: SearchId, best_move: Option<Move> },
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

    /// Stops any running search and starts one on `board`; no budget means until stopped.
    pub fn start(&mut self, board: &Board, budget: Option<TimeBudget>) -> SearchId {
        self.stop();
        self.next_id += 1;
        let id = SearchId(self.next_id);
        let control = SearchControl::new(budget);
        self.running = Some(Arc::clone(&control));
        self.send(Request::Search {
            id,
            board: Box::new(board.clone()),
            budget,
            control,
        });
        id
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
    let tt = SharedTransTable::new(hash_mb);
    let mut search_threads = SearchThreads::new(threads);
    for request in requests {
        match request {
            Request::Search {
                id,
                board,
                budget,
                control,
            } => {
                let limits = SearchLimits {
                    max_depth: MAX_DEPTH,
                    params: SearchParams::default(),
                    wait_for_stop: budget.is_none(),
                };
                let report = |info: &IterationInfo| {
                    let _ = replies.send(Reply::Info {
                        id,
                        info: info.clone(),
                    });
                };
                let result = search_reporting(&board, &tt, limits, &mut search_threads, &control, &report);
                let _ = replies.send(Reply::Done {
                    id,
                    best_move: result.best_move,
                });
            }
            Request::NewGame => {
                tt.clear();
                search_threads.clear();
            }
        }
    }
}
