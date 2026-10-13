use chess::bench;
use chess::board::Board;
use chess::compare::{CompareConfig, run_compare};
use chess::eval;
use chess::game_runner::GameClock;
use chess::openings;
use chess::perft::{divide, perft};
use chess::search_params::PARAM_SPECS;
use chess::spsa;
use chess::sprt::SprtConfig;
use chess::tournament::{self, TournamentConfig};
use chess::tui;
use chess::types::{Color, START_FEN};
use chess::uci;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "chess",
    version,
    about = "Chess engine with perft/uci/play modes"
)]
struct Cli {
    /// Evaluation to use: stockfish (when built in) or bootstrap
    #[arg(long, global = true, default_value = eval::EvalKind::DEFAULT.name())]
    eval: eval::EvalKind,
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
    /// Play the engine in an interactive terminal board, or watch it play itself
    Play {
        /// casual: engine evaluation, analysis and takebacks; game: no help until the game
        /// ends; watch: the engine plays both sides
        #[arg(long, value_enum, default_value_t = PlayMode::Casual)]
        mode: PlayMode,
        /// The colour you play
        #[arg(long, value_enum, default_value_t = SideChoice::White)]
        side: SideChoice,
        /// Clock for both sides as minutes+increment seconds, e.g. 5+3
        #[arg(long, value_parser = tui::parse_clock)]
        clock: Option<GameClock>,
        /// Engine thinking time per move in milliseconds when there is no clock
        #[arg(long, default_value_t = 3000)]
        movetime: u64,
        #[arg(long)]
        fen: Option<String>,
        #[arg(long, default_value_t = 4)]
        threads: usize,
        /// Hash table size in MB
        #[arg(long, default_value_t = 256)]
        hash: usize,
    },
    Eval {
        fen: String,
    },
    /// Print the Stockfish 18 network's per-bucket contributions, as Stockfish's `eval` does
    #[cfg(feature = "stockfish-nnue")]
    EvalTrace {
        /// Position to trace; reads one FEN per line from stdin when omitted
        fen: Option<String>,
        /// Big network file (defaults to the path baked in at build time)
        #[arg(long)]
        big: Option<String>,
        /// Small network file (defaults to the path baked in at build time)
        #[arg(long)]
        small: Option<String>,
    },
    /// Fixed-depth search over the bench positions; prints total nodes and NPS
    Bench {
        #[arg(default_value_t = 13)]
        depth: usize,
        /// Transposition table size in MB
        #[arg(long, default_value_t = bench::BENCH_HASH_MB)]
        hash: usize,
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
        /// Base time for engine 2 when it gets a different clock (time odds); defaults to --time
        #[arg(long = "e2-time")]
        e2_time: Option<u64>,
        /// Increment for engine 2; defaults to --inc
        #[arg(long = "e2-inc")]
        e2_inc: Option<u64>,
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
    /// List the tunable search parameters with their defaults, ranges and SPSA steps
    Params,
    /// Tune the search parameters with SPSA self-play game pairs
    Tune {
        /// EPD opening suite (e.g. $CHESS_BOOK_UHO)
        #[arg(long)]
        openings: String,
        /// Engine binary to tune; defaults to this executable
        #[arg(long)]
        engine: Option<String>,
        /// Planned number of game pairs (SPSA iterations)
        #[arg(long, default_value_t = 10_000)]
        pairs: usize,
        /// Base time per game in milliseconds
        #[arg(long, default_value_t = 5000)]
        time: u64,
        /// Increment per move in milliseconds
        #[arg(long, default_value_t = 50)]
        inc: u64,
        /// Unpinned workers, used when --workers is not given
        #[arg(long, default_value_t = 8)]
        concurrency: usize,
        /// Pinned worker groups as cpus:count, e.g. "0-15:8,16-27:10"; both engines of a
        /// pair run on the same group
        #[arg(long)]
        workers: Option<String>,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// File the current estimate is written to
        #[arg(long, default_value = "spsa.txt")]
        output: String,
        /// Start from an estimate file written by an earlier run
        #[arg(long)]
        resume: Option<String>,
        /// Extra UCI options for both engines (e.g. "Hash=16")
        #[arg(long = "option")]
        options: Vec<String>,
    },
    Uci,
}

#[derive(Clone, Copy, ValueEnum)]
enum PlayMode {
    Casual,
    Game,
    Watch,
}

impl From<PlayMode> for tui::Mode {
    fn from(mode: PlayMode) -> Self {
        match mode {
            PlayMode::Casual => Self::Casual,
            PlayMode::Game => Self::Game,
            PlayMode::Watch => Self::Watch,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum SideChoice {
    White,
    Black,
    Random,
}

impl SideChoice {
    fn color(self) -> Color {
        match self {
            Self::White => Color::White,
            Self::Black => Color::Black,
            Self::Random if rand::random::<bool>() => Color::White,
            Self::Random => Color::Black,
        }
    }
}

/// Loads the networks baked in at build time, or exits: nothing can be evaluated without them.
fn require_networks(kind: eval::EvalKind) {
    if let Err(e) = eval::EvalOptions::with_kind(kind).load() {
        exit_with_error(&format!("failed to load the {} evaluator: {e}", kind.name()));
    }
}

fn main() {
    let mut cli = Cli::parse();
    match cli.cmd.take().unwrap_or(Cmd::Uci) {
        Cmd::Perft {
            depth,
            fen,
            divide: div,
        } => {
            require_networks(cli.eval);
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
        Cmd::Play {
            mode,
            side,
            clock,
            movetime,
            fen,
            threads,
            hash,
        } => {
            require_networks(cli.eval);
            let fen_str = fen.unwrap_or_else(|| START_FEN.to_string());
            let start = Board::from_fen(&fen_str)
                .unwrap_or_else(|e| exit_with_error(&format!("FEN parse error: {e}")));
            let config = tui::PlayConfig {
                start,
                mode: mode.into(),
                human: side.color(),
                time: clock.map_or(tui::TimeSetting::PerMove { ms: movetime }, tui::TimeSetting::Clock),
            };
            let engine = tui::EngineConfig {
                threads: threads.max(1),
                hash_mb: hash.max(1),
            };
            match tui::run(config, engine) {
                Ok(record) => println!("{record}"),
                Err(e) => exit_with_error(&format!("terminal error: {e}")),
            }
        }
        Cmd::Eval { fen } => {
            require_networks(cli.eval);
            let mut b = Board::from_fen(&fen).unwrap_or_else(|e| {
                eprintln!("FEN parse error: {e}");
                std::process::exit(1);
            });
            let score = eval::evaluate(&mut b);
            let side = if b.turn == Color::White { "white" } else { "black" };
            let sign = if score >= 0 { "+" } else { "" };
            println!("{sign}{score} cp ({side} to move)");
        }
        #[cfg(feature = "stockfish-nnue")]
        Cmd::EvalTrace { fen, big, small } => {
            use eval::stockfish::{NetPaths, ensure_loaded, trace_text};
            let built_in = NetPaths::built_in();
            let paths = match (big, small, built_in) {
                (Some(big), Some(small), _) => NetPaths { big: big.into(), small: small.into() },
                (big, small, Some(defaults)) => NetPaths {
                    big: big.map_or(defaults.big, Into::into),
                    small: small.map_or(defaults.small, Into::into),
                },
                _ => exit_with_error("no networks: pass --big and --small or build inside the dev shell"),
            };
            if let Err(e) = ensure_loaded(&paths) {
                exit_with_error(&format!("failed to load NNUE: {e}"));
            }
            let fens: Vec<String> = match fen {
                Some(fen) => vec![fen],
                None => std::io::stdin().lines().map_while(Result::ok).filter(|l| !l.trim().is_empty()).collect(),
            };
            for fen in fens {
                let board = Board::from_fen(&fen)
                    .unwrap_or_else(|e| exit_with_error(&format!("FEN parse error in {fen}: {e}")));
                print!("{}", trace_text(&board).unwrap_or_default());
            }
        }
        Cmd::Bench { depth, hash } => {
            require_networks(cli.eval);
            match bench::run(depth, hash) {
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
            }
        }
        Cmd::Challenge {
            engine1,
            engine2,
            e1_options,
            e2_options,
            time,
            inc,
            e2_time,
            e2_inc,
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
                e1_clock: GameClock {
                    base_ms: time,
                    increment_ms: inc,
                },
                e2_clock: GameClock {
                    base_ms: e2_time.unwrap_or(time),
                    increment_ms: e2_inc.unwrap_or(inc),
                },
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
        Cmd::Params => {
            println!("{:<24} {:>8} {:>8} {:>8} {:>6}", "name", "default", "min", "max", "step");
            for spec in PARAM_SPECS {
                println!("{:<24} {:>8} {:>8} {:>8} {:>6}", spec.name, spec.default, spec.min, spec.max, spec.step);
            }
        }
        Cmd::Tune {
            openings,
            engine,
            pairs,
            time,
            inc,
            concurrency,
            workers,
            seed,
            output,
            resume,
            options,
        } => {
            let workers = match workers {
                Some(spec) => spsa::parse_worker_groups(&spec).unwrap_or_else(|e| exit_with_error(&e)),
                None => vec![spsa::WorkerGroup { cpus: None, count: concurrency }],
            };
            let engine = engine
                .or_else(|| std::env::current_exe().ok().map(|p| p.display().to_string()))
                .unwrap_or_else(|| exit_with_error("cannot locate the engine binary; pass --engine"));
            let start = match resume {
                Some(path) => std::fs::read_to_string(&path)
                    .map_err(|e| format!("{path}: {e}"))
                    .and_then(|text| spsa::read_estimate(&text))
                    .unwrap_or_else(|e| exit_with_error(&e)),
                None => Vec::new(),
            };
            let openings = openings::sample_openings(&openings, 100_000, seed)
                .unwrap_or_else(|e| exit_with_error(&e));
            let mut engine_options = vec![
                ("OwnBook".to_string(), "false".to_string()),
                ("Hash".to_string(), "16".to_string()),
            ];
            engine_options.extend(parse_options(&options));
            let config = spsa::SpsaConfig {
                engine,
                engine_options,
                openings,
                pairs,
                base_time_ms: time,
                increment_ms: inc,
                workers,
                seed,
                output: output.into(),
                start,
            };
            match spsa::run(config) {
                Ok(theta) => {
                    println!("final estimate (rounded):");
                    for (spec, value) in PARAM_SPECS.iter().zip(theta) {
                        println!("  {:<24} {:>6} (default {})", spec.name, value.round() as i32, spec.default);
                    }
                }
                Err(e) => exit_with_error(&e),
            }
        }
        Cmd::Uci => uci::run_uci(cli.eval),
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

