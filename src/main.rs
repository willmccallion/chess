use chess::bench;
use chess::board::Board;
use chess::compare::{CompareConfig, run_compare};
use chess::nnue;
use chess::openings;
use chess::perft::{divide, perft};
use chess::search::{SearchControl, SearchLimits, search};
use chess::sprt::SprtConfig;
use chess::tournament::{self, TournamentConfig};
use chess::time::TimeBudget;
use chess::tt::SharedTransTable;
use chess::types::{Color, Move, MoveList, Piece, PieceKind, START_FEN};
use chess::uci;
use chess::uci_io::{format_uci, parse_uci_move};
use clap::{Parser, Subcommand};
use std::io::{self, Write};
use std::sync::Arc;
use std::thread;

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
    /// Fixed-depth search over the bench positions; prints total nodes and NPS
    Bench {
        #[arg(default_value_t = 13)]
        depth: usize,
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
        /// EPD opening suite; each sampled opening is played twice with colors swapped
        #[arg(long)]
        openings: Option<String>,
        /// Seed for sampling and ordering openings
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Stop early via SPRT: "elo0,elo1" or "elo0,elo1,alpha,beta" (logistic Elo)
        #[arg(long)]
        sprt: Option<String>,
    },
    /// Analyse positions with this engine and a reference engine and compare the results
    Compare {
        /// Position to analyse (repeatable); defaults to the bench positions
        #[arg(long)]
        fen: Vec<String>,
        /// EPD file to sample positions from
        #[arg(long)]
        epd: Option<String>,
        /// Number of positions sampled from --epd
        #[arg(long, default_value_t = 20)]
        count: usize,
        /// Seed for sampling --epd positions
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Search depth for both engines
        #[arg(long, default_value_t = 12)]
        depth: u32,
        /// Search time per position in ms for both engines (overrides --depth)
        #[arg(long)]
        movetime: Option<u64>,
        /// Reference engine search depth (overrides --depth/--movetime for the reference)
        #[arg(long)]
        ref_depth: Option<u32>,
        /// Engine to evaluate (default: this binary)
        #[arg(long)]
        engine: Option<String>,
        /// Reference engine (default: $STOCKFISH, else `stockfish` on PATH)
        #[arg(long)]
        reference: Option<String>,
        /// UCI options for the engine; OwnBook=false is always sent first
        #[arg(long = "e1-option")]
        e1_options: Vec<String>,
        /// UCI options for the reference engine
        #[arg(long = "e2-option")]
        e2_options: Vec<String>,
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
        Cmd::Bench { depth } => match bench::run(depth) {
            Ok(result) => {
                println!("===========================");
                println!("Total time (ms) : {}", result.elapsed_ms);
                println!("Nodes searched  : {}", result.nodes);
                println!("Nodes/second    : {}", result.nps());
            }
            Err(e) => {
                eprintln!("bench failed: {e}");
                std::process::exit(1);
            }
        },
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
            openings,
            seed,
            sprt,
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
            let pair_count = rounds.div_ceil(2);
            let openings = match openings {
                Some(path) => openings::sample_openings(&path, pair_count, seed).unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(1);
                }),
                None => Vec::new(),
            };
            let sprt = sprt.map(|spec| {
                SprtConfig::parse(&spec).unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(1);
                })
            });
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
                openings,
                sprt,
            });
        }
        Cmd::Compare {
            fen,
            epd,
            count,
            seed,
            depth,
            movetime,
            ref_depth,
            engine,
            reference,
            e1_options,
            e2_options,
        } => {
            let fens = match (fen.is_empty(), epd) {
                (false, _) => fen,
                (true, Some(path)) => openings::sample_openings(&path, count, seed)
                    .unwrap_or_else(|e| exit_with_error(&e)),
                (true, None) => bench::BENCH_FENS.iter().map(|f| f.to_string()).collect(),
            };
            let engine_go = movetime.map_or_else(|| format!("depth {depth}"), |ms| format!("movetime {ms}"));
            let reference_go = ref_depth.map_or_else(|| engine_go.clone(), |d| format!("depth {d}"));
            let engine_path = engine.unwrap_or_else(|| {
                std::env::current_exe()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|e| exit_with_error(&format!("cannot locate own binary: {e}")))
            });
            let reference_path = reference
                .or_else(|| std::env::var("STOCKFISH").ok())
                .unwrap_or_else(|| "stockfish".to_string());
            let mut engine_options = vec![("OwnBook".to_string(), "false".to_string())];
            engine_options.extend(parse_options(&e1_options));
            let config = CompareConfig {
                engine_path,
                reference_path,
                engine_options,
                reference_options: parse_options(&e2_options),
                fens,
                engine_go,
                reference_go,
            };
            if let Err(e) = run_compare(&config) {
                exit_with_error(&e);
            }
        }
        Cmd::Uci => uci::run_uci(),
    }
}

fn exit_with_error(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

fn parse_options(options: &[String]) -> Vec<(String, String)> {
    options
        .iter()
        .map(|s| tournament::parse_option(s).unwrap_or_else(|e| exit_with_error(&e)))
        .collect()
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
        let tt = SharedTransTable::new(tt_size_mb);

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

            let control = SearchControl::new(Some(TimeBudget::fixed(time_ms)));
            let limits = SearchLimits {
                max_depth,
                wait_for_stop: false,
            };
            let engine_move_opt = search(&b, &tt, limits, threads_count, &control).best_move;

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
    let tt = SharedTransTable::new(tt_size_mb);

    struct PonderState {
        handle: Option<thread::JoinHandle<()>>,
        control: Option<Arc<SearchControl>>,
    }
    impl PonderState {
        fn stop_and_join(&mut self) {
            if let Some(control) = self.control.take() {
                control.stop();
            }
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }
    let mut ponder_state = PonderState {
        handle: None,
        control: None,
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

            ponder_state.stop_and_join();

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

        let control = SearchControl::new(Some(TimeBudget::fixed(time_ms)));
        let limits = SearchLimits {
            max_depth,
            wait_for_stop: false,
        };
        let result = search(b, &tt, limits, threads_count, &control);
        let engine_move_opt = result.best_move;

        let engine_move = if let Some(m) = engine_move_opt {
            m
        } else {
            println!("Engine has no moves. Game Over.");
            break;
        };

        ponder_move_opt = result.ponder_move;

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
                let ponder_control = SearchControl::new(None);
                let thread_control = Arc::clone(&ponder_control);
                let ponder_limits = SearchLimits {
                    max_depth,
                    wait_for_stop: true,
                };
                let _ = thread::Builder::new()
                    .name("ponder-cli".to_string())
                    .spawn(move || {
                        search(&ponder_board, &tt_clone, ponder_limits, threads_count, &thread_control);
                    })
                    .map(|h| {
                        ponder_state.handle = Some(h);
                        ponder_state.control = Some(ponder_control);
                    });
            }
        }
    }

    ponder_state.stop_and_join();
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
