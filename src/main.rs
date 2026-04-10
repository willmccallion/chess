use chess::board::Board;
use chess::nnue;
use chess::perft::{divide, perft};
use chess::search::{best_move_timed, get_pv_from_tt};
use chess::tournament::{self, TournamentConfig};
use chess::tt::SharedTransTable;
use chess::types::{Color, Move, MoveList, Piece, PieceKind, START_FEN};
use chess::uci;
use chess::uci_io::{format_uci, parse_uci_move};
use clap::{Parser, Subcommand};
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

const SEARCH_THREAD_STACK: usize = 32 * 1024 * 1024; // 32 MiB

#[derive(Parser)]
#[command(
    name = "chess",
    version,
    about = "Chess engine with perft/uci/play modes"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    Perft {
        depth: usize,
        #[arg(long)]
        fen: Option<String>,
        #[arg(long)]
        divide: bool,
    },
    PlayCli {
        #[arg(long)]
        fen: Option<String>,
        #[arg(long, default_value_t = 10000)]
        time: u64,
        #[arg(long, default_value_t = 64)]
        depth: usize,
        #[arg(long, default_value_t = 1)]
        threads: usize,
    },
    SelfPlay {
        #[arg(long, default_value_t = 10)]
        rounds: usize,
        #[arg(long, default_value_t = 5000)]
        time: u64,
        #[arg(long, default_value_t = 64)]
        depth: usize,
        #[arg(long)]
        fen: Option<String>,
        #[arg(long)]
        threads: Option<usize>,
    },
    Eval {
        fen: String,
    },
    Challenge {
        /// Path to engine 1 binary
        engine1: String,
        /// Path to engine 2 binary
        engine2: String,
        /// UCI options for engine 1 (e.g. "Threads=4")
        #[arg(long = "e1-option")]
        e1_options: Vec<String>,
        /// UCI options for engine 2 (e.g. "Hash=256")
        #[arg(long = "e2-option")]
        e2_options: Vec<String>,
        /// Base time in milliseconds per side
        #[arg(long, default_value_t = 5000)]
        time: u64,
        /// Increment in milliseconds per move
        #[arg(long, default_value_t = 50)]
        inc: u64,
        /// Number of games to play
        #[arg(long, default_value_t = 10)]
        rounds: usize,
        /// Maximum concurrent games
        #[arg(long, default_value_t = 1)]
        concurrency: usize,
        /// Starting FEN (default: standard start position)
        #[arg(long)]
        fen: Option<String>,
    },
    Uci,
}

fn main() {
    // Initialize the NNUE network.
    if let Err(e) = nnue::init() {
        panic!("Failed to load embedded NNUE data: {}", e);
    }

    println!("NNUE loaded successfully.");

    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Uci) {
        Cmd::Perft {
            depth,
            fen,
            divide: div,
        } => {
            let fen_str = fen.unwrap_or_else(|| START_FEN.to_string());
            let mut b = Board::from_fen(&fen_str).unwrap_or_else(|e| {
                eprintln!("FEN parse error: {e}");
                std::process::exit(1);
            });
            if div {
                divide(&mut b, depth);
            } else {
                let n = perft(&mut b, depth);
                println!("perft({depth}) = {n}");
            }
        }
        Cmd::PlayCli {
            fen,
            time,
            depth,
            threads,
        } => {
            let fen_str = fen.unwrap_or_else(|| START_FEN.to_string());
            let mut b = Board::from_fen(&fen_str).unwrap_or_else(|e| {
                eprintln!("FEN parse error: {e}");
                std::process::exit(1);
            });
            play_cli(&mut b, time, depth, threads);
        }
        Cmd::SelfPlay {
            rounds,
            time,
            depth,
            fen,
            threads,
        } => {
            let threads_count = threads.unwrap_or_else(num_cpus::get).max(1);
            let fen_str = fen.unwrap_or_else(|| START_FEN.to_string());
            self_play(&fen_str, rounds, time, depth, threads_count);
        }
        Cmd::Eval { fen } => {
            let b = Board::from_fen(&fen).unwrap_or_else(|e| {
                eprintln!("FEN parse error: {e}");
                std::process::exit(1);
            });
            let score = nnue::evaluate(&b);
            let side = if b.turn == Color::White { "white" } else { "black" };
            let sign = if score >= 0 { "+" } else { "" };
            println!("{sign}{score} cp ({side} to move)");
        }
        Cmd::Challenge {
            engine1,
            engine2,
            e1_options,
            e2_options,
            time,
            inc,
            rounds,
            concurrency,
            fen,
        } => {
            let e1_opts: Vec<_> = e1_options
                .iter()
                .map(|s| tournament::parse_option(s).unwrap_or_else(|e| {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }))
                .collect();
            let e2_opts: Vec<_> = e2_options
                .iter()
                .map(|s| tournament::parse_option(s).unwrap_or_else(|e| {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }))
                .collect();
            tournament::run_tournament(TournamentConfig {
                engine1_path: engine1,
                engine2_path: engine2,
                e1_options: e1_opts,
                e2_options: e2_opts,
                base_time_ms: time,
                increment_ms: inc,
                rounds,
                concurrency,
                start_fen: fen,
            });
        }
        Cmd::Uci => uci::run_uci(),
    }
}

fn self_play(fen_str: &str, rounds: usize, time_ms: u64, max_depth: usize, threads_count: usize) {
    let mut white_wins = 0;
    let mut black_wins = 0;
    let mut draws = 0;

    println!("Starting self-play session:");
    println!("- Rounds: {}", rounds);
    println!("- Time per move: {}ms", time_ms);
    println!("- Max depth: {}", max_depth);
    println!("- Threads: {}", threads_count);
    println!("--------------------------------");

    for i in 1..=rounds {
        let mut b = Board::from_fen(fen_str).unwrap_or_else(|e| {
            eprintln!("FEN parse error: {e}");
            std::process::exit(1);
        });

        let tt_size_mb = 1024;
        let mut tt = SharedTransTable::new(tt_size_mb);

        println!("\nGame {}/{}", i, rounds);
        println!("Starting FEN: {}", b.to_fen());

        'gameloop: loop {
            print!("\x1B[2J\x1B[H"); // Clear screen
            println!("Game {}/{}", i, rounds);
            println!("FEN: {}", b.to_fen());
            print_board_ascii(&b);
            println!("Turn: {:?}, Move: {}", b.turn, b.fullmove_number);

            let mut legal_moves = MoveList::new();
            b.generate_legal_moves(&mut legal_moves);

            if legal_moves.is_empty() {
                let king_piece = Piece::from_kind(PieceKind::King, b.turn);
                let king_sq_opt = b.piece_bb[king_piece.index()].trailing_zeros();
                if king_sq_opt < 64 && b.is_square_attacked(king_sq_opt as i32, b.turn.other()) {
                    println!("Result: Checkmate! {:?} wins.", b.turn.other());
                    if b.turn.other() == Color::White {
                        white_wins += 1;
                    } else {
                        black_wins += 1;
                    }
                } else {
                    println!("Result: Stalemate!");
                    draws += 1;
                }
                break 'gameloop;
            }

            if b.is_draw_by_repetition() || b.halfmove_clock >= 100 {
                println!("Result: Draw!");
                draws += 1;
                break 'gameloop;
            }

            println!("Engine ({:?}) is thinking...", b.turn);

            let stop_signal = Arc::new(AtomicBool::new(false));
            let mut helpers = vec![];
            let helper_depth = max_depth.min(64);

            for i in 0..(threads_count - 1) {
                let board_clone = b.clone();
                let tt_clone = tt.clone();
                let stop_clone = Arc::clone(&stop_signal);
                let name = format!("self-play-helper-{}", i);
                let _ = thread::Builder::new()
                    .name(name)
                    .stack_size(SEARCH_THREAD_STACK)
                    .spawn(move || {
                        let mut tt_local = tt_clone;
                        best_move_timed(
                            &board_clone,
                            &mut tt_local,
                            u64::MAX / 4,
                            helper_depth,
                            stop_clone,
                            false,
                        );
                    })
                    .map(|jh| helpers.push(jh));
            }

            let (engine_move_opt, _, _) = best_move_timed(
                &b,
                &mut tt,
                time_ms,
                max_depth,
                Arc::clone(&stop_signal),
                true,
            );

            stop_signal.store(true, Ordering::Relaxed);
            for h in helpers {
                let _ = h.join();
            }

            let engine_move = if let Some(m) = engine_move_opt {
                m
            } else {
                println!("Engine has no moves. Game Over.");
                draws += 1;
                break 'gameloop;
            };

            println!(
                "Engine plays: {} ({})",
                b.to_san(engine_move, legal_moves.as_slice()),
                format_uci(engine_move)
            );
            let _u = b.make_move(engine_move);
            thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    println!("\nSelf-Play Session Complete");
    println!("Final Score:");
    println!("  White Wins: {}", white_wins);
    println!("  Black Wins: {}", black_wins);
    println!("  Draws: {}", draws);
    println!("------------------------------------");
}

fn play_cli(b: &mut Board, time_ms: u64, max_depth: usize, threads_count: usize) {
    {
        let mut _moves = MoveList::new();
        b.generate_legal_moves(&mut _moves);
    }

    let tt_size_mb = 1024;
    let mut tt = SharedTransTable::new(tt_size_mb);

    struct PonderState {
        handle: Option<thread::JoinHandle<()>>,
        stop_signal: Arc<AtomicBool>,
    }
    let mut ponder_state = PonderState {
        handle: None,
        stop_signal: Arc::new(AtomicBool::new(false)),
    };

    let mut ponder_move_opt: Option<Move> = None;

    'gameloop: loop {
        print!("\x1B[2J\x1B[H"); // Clear screen
        println!("FEN: {}", b.to_fen());
        print_board_ascii(b);
        if let Some(pm) = ponder_move_opt {
            println!("(Engine is pondering your move: {})", format_uci(pm));
        }

        let mut legal_moves = MoveList::new();
        b.generate_legal_moves(&mut legal_moves);

        if legal_moves.is_empty() {
            println!("You have no legal moves. Game Over.");
            break;
        }

        let mut user_move_made = false;
        while !user_move_made {
            print!("\nYour move (e.g., Nf3, e2e4, or 'quit'): ");
            io::stdout().flush().unwrap();
            let mut line = String::new();
            if io::stdin().read_line(&mut line).is_err() {
                break 'gameloop;
            }
            let input_str = line.trim();

            if input_str.eq_ignore_ascii_case("quit") {
                break 'gameloop;
            }

            if let Some(handle) = ponder_state.handle.take() {
                ponder_state.stop_signal.store(true, Ordering::Relaxed);
                handle.join().unwrap();
            }

            let mut user_move_opt = parse_uci_move(b, input_str);

            if user_move_opt.is_none() {
                for &legal_move in legal_moves.as_slice() {
                    // remove check/mate suffix to accept "Nf3" style inputs
                    let san_str = b
                        .to_san(legal_move, legal_moves.as_slice())
                        .replace(['+', '#'], "");
                    if san_str == input_str {
                        user_move_opt = Some(legal_move);
                        break;
                    }
                }
            }

            if let Some(user_move) = user_move_opt {
                if legal_moves.as_slice().contains(&user_move) {
                    if Some(user_move) == ponder_move_opt {
                        println!("(Ponder hit!)");
                    }
                    let _u = b.make_move(user_move);
                    user_move_made = true;
                } else {
                    println!("Illegal move. Try again.");
                }
            } else {
                println!("Unrecognized or illegal move format. Try again.");
            }
        }

        print!("\x1B[2J\x1B[H"); // Clear screen
        println!("FEN: {}", b.to_fen());
        print_board_ascii(b);
        println!(
            "\nEngine is thinking for up to {} seconds using {} threads...",
            time_ms / 1000,
            threads_count
        );
        println!("(Search information will appear below)");
        println!("--------------------------------");
        io::stdout().flush().unwrap();

        let stop_signal = Arc::new(AtomicBool::new(false));
        let mut helpers = vec![];

        // Conservative recursion cap for helpers to avoid stack blowups
        let helper_depth = max_depth.min(64);

        for i in 0..(threads_count - 1) {
            let board_clone = b.clone();
            let tt_clone = tt.clone();
            let stop_clone = Arc::clone(&stop_signal);
            let name = format!("helper-{}", i);
            let _ = thread::Builder::new()
                .name(name)
                .stack_size(SEARCH_THREAD_STACK)
                .spawn(move || {
                    let mut tt_local = tt_clone;
                    best_move_timed(
                        &board_clone,
                        &mut tt_local,
                        u64::MAX / 4,
                        helper_depth,
                        stop_clone,
                        false,
                    );
                })
                .map(|jh| helpers.push(jh));
        }

        let (engine_move_opt, _, _) = best_move_timed(
            b,
            &mut tt,
            time_ms,
            max_depth,
            Arc::clone(&stop_signal),
            true,
        );

        stop_signal.store(true, Ordering::Relaxed);
        for h in helpers {
            let _ = h.join();
        }

        let engine_move = if let Some(m) = engine_move_opt {
            m
        } else {
            println!("Engine has no moves. Game Over.");
            break;
        };

        let pv = get_pv_from_tt(b.clone(), &tt, 2);
        ponder_move_opt = pv.get(1).copied();

        println!("\n--------------------------------");
        println!("Engine plays: {}", format_uci(engine_move));
        let _u = b.make_move(engine_move);
        thread::sleep(std::time::Duration::from_millis(500));

        if let Some(ponder_move) = ponder_move_opt {
            let mut legal_moves = MoveList::new();
            b.generate_legal_moves(&mut legal_moves);
            if legal_moves.as_slice().contains(&ponder_move) {
                let mut ponder_board = b.clone();
                let _ = ponder_board.make_move(ponder_move);
                let tt_clone = tt.clone();
                ponder_state.stop_signal.store(false, Ordering::Relaxed);
                let stop_clone = Arc::clone(&ponder_state.stop_signal);

                let _ = thread::Builder::new()
                    .name("ponder-helper-cli".to_string())
                    .stack_size(SEARCH_THREAD_STACK)
                    .spawn(move || {
                        let mut tt_local = tt_clone;
                        best_move_timed(
                            &ponder_board,
                            &mut tt_local,
                            u64::MAX / 4,
                            helper_depth,
                            stop_clone,
                            false,
                        );
                    })
                    .map(|h| ponder_state.handle = Some(h));
            }
        }
    }

    if let Some(handle) = ponder_state.handle.take() {
        ponder_state.stop_signal.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }
    println!("Exiting game.");
}

fn print_board_ascii(b: &Board) {
    use chess::types::Piece;
    const BLUE: &str = "\x1b[34m";
    const RESET: &str = "\x1b[0m";
    println!("\n   a b c d e f g h");
    println!(" +-----------------+");
    for r in (0..8).rev() {
        print!("{}| ", r + 1);
        for f in 0..8 {
            let p = b.piece_on[(r * 8 + f) as usize];
            let s = match p {
                Piece::Empty => ".".to_string(),
                Piece::WP => "P".to_string(),
                Piece::WN => "N".to_string(),
                Piece::WB => "B".to_string(),
                Piece::WR => "R".to_string(),
                Piece::WQ => "Q".to_string(),
                Piece::WK => "K".to_string(),
                Piece::BP => format!("{BLUE}p{RESET}"),
                Piece::BN => format!("{BLUE}n{RESET}"),
                Piece::BB => format!("{BLUE}b{RESET}"),
                Piece::BR => format!("{BLUE}r{RESET}"),
                Piece::BQ => format!("{BLUE}q{RESET}"),
                Piece::BK => format!("{BLUE}k{RESET}"),
            };
            print!("{s} ");
        }
        println!("|{}", r + 1);
    }
    println!(" +-----------------+");
    println!("   a b c d e f g h\n");
}
