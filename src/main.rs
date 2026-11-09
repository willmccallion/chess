use chess::bench;
use chess::board::Board;
use chess::compare::{CompareConfig, run_compare};
use chess::eval;
use chess::game_runner::{GameClock, StartPosition};
use chess::openings;
use chess::perft::{divide, perft};
use chess::search_params::PARAM_SPECS;
use chess::sprt::SprtConfig;
use chess::spsa;
use chess::tournament::{self, EngineSpec, TournamentConfig};
use chess::tui;
use chess::types::{Color, START_FEN};
use chess::uci;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::num::NonZeroUsize;

/// The hash size SPSA's engines use unless `--option` says otherwise; many
/// games run at once.
const TUNE_HASH_OPTION: &str = "Hash=16";

#[derive(Parser)]
#[command(
    name = "chess",
    version,
    about = "Chess engine with perft/uci/play modes"
)]
struct Cli {
    /// Evaluation to use: stockfish (when built in), bootstrap or own
    #[arg(long, global = true, default_value = eval::EvalKind::DEFAULT.name())]
    eval: eval::EvalKind,
    /// Network file for --eval own (defaults to $CHESS_NNUE_OWN at build time)
    #[arg(long, global = true)]
    own_net: Option<std::path::PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    Perft(PerftArgs),
    /// Generate training positions from self-play, as `<FEN> | <score> | <result>` lines
    Datagen(DatagenArgs),
    /// Play the engine in an interactive terminal board, watch it play itself, or learn
    Play(PlayArgs),
    /// Import puzzles for learn mode from the Lichess puzzle CSV (`-` reads stdin), e.g.
    /// `zstd -dc lichess_db_puzzle.csv.zst | chess import-puzzles -`
    ImportPuzzles(ImportPuzzlesArgs),
    Eval(EvalArgs),
    /// Print the Stockfish 18 network's per-bucket contributions, as Stockfish's `eval` does
    #[cfg(feature = "stockfish-nnue")]
    EvalTrace(EvalTraceArgs),
    /// Fixed-depth search over the bench positions; prints total nodes and NPS
    Bench(BenchArgs),
    Challenge(ChallengeArgs),
    /// Analyse positions with this engine and a reference engine and compare the results
    Compare(CompareArgs),
    /// List the tunable search parameters with their defaults, ranges and SPSA steps
    Params,
    /// Tune the search parameters with SPSA self-play game pairs
    Tune(TuneArgs),
    Uci,
}

#[derive(Args)]
struct PerftArgs {
    depth: usize,
    #[arg(long)]
    fen: Option<String>,
    #[arg(long)]
    divide: bool,
}

impl PerftArgs {
    fn run(self, cli: &Cli) {
        let Self {
            depth,
            fen,
            divide: div,
        } = self;
        require_networks(cli);
        let fen_str = fen.unwrap_or_else(|| START_FEN.to_string());
        let mut b = Board::from_fen(&fen_str)
            .unwrap_or_else(|e| exit_with_error(&format!("FEN parse error: {e}")));
        if div {
            divide(&mut b, depth);
        } else {
            let n = perft(&mut b, depth);
            println!("perft({depth}) = {n}");
        }
    }
}

#[derive(Args)]
struct DatagenArgs {
    /// File to create; an existing file is never overwritten
    #[arg(long)]
    out: std::path::PathBuf,
    /// Stop after writing this many positions
    #[arg(long, default_value_t = 1_000_000)]
    positions: u64,
    /// Games played in parallel (defaults to all logical CPUs)
    #[arg(long)]
    threads: Option<usize>,
    /// Nodes after which no new iteration starts
    #[arg(long, default_value_t = 5000)]
    soft_nodes: u64,
    /// Nodes at which a search stops at once (defaults to 8x --soft-nodes)
    #[arg(long)]
    hard_nodes: Option<u64>,
    /// Random moves from the start position before the engine plays (one more in half the games)
    #[arg(long, default_value_t = 8)]
    random_plies: usize,
    /// Seed for the random openings (defaults to the clock)
    #[arg(long)]
    seed: Option<u64>,
    /// Hash table per game in MB
    #[arg(long, default_value_t = 16)]
    hash: usize,
}

impl DatagenArgs {
    fn run(self, cli: &Cli) {
        let Self {
            out,
            positions,
            threads,
            soft_nodes,
            hard_nodes,
            random_plies,
            seed,
            hash,
        } = self;
        require_networks(cli);
        let seed = seed.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64)
        });
        let config = chess::datagen::DatagenConfig {
            output: out,
            positions,
            threads: threads.unwrap_or_else(available_threads).max(1),
            nodes: chess::search::NodeLimit {
                soft: soft_nodes.max(1),
                hard: hard_nodes
                    .unwrap_or(soft_nodes.saturating_mul(8))
                    .max(soft_nodes.max(1)),
            },
            random_plies,
            seed,
            hash_mb: hash.max(1),
        };
        eprintln!(
            "datagen with the {} evaluator, seed {seed}",
            cli.eval.name()
        );
        match chess::datagen::run(&config) {
            Ok(summary) => println!(
                "{} positions from {} games in {:.0}s",
                summary.positions,
                summary.games,
                summary.elapsed.as_secs_f64()
            ),
            Err(e) => exit_with_error(&format!("datagen failed: {e}")),
        }
    }
}

#[derive(Args)]
struct PlayArgs {
    /// casual: engine evaluation, analysis and takebacks; game: no help until the game
    /// ends; watch: the engine plays both sides; learn: the coach, with lessons, puzzles,
    /// games at your level and reviews
    #[arg(long, value_enum, default_value_t = PlayMode::Casual)]
    mode: PlayMode,
    /// Where learn mode keeps your games and progress (defaults to ~/.local/share/chess/coach)
    #[arg(long)]
    coach_dir: Option<std::path::PathBuf>,
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
}

impl PlayArgs {
    fn run(self, cli: &Cli) {
        let Self {
            mode,
            coach_dir,
            side,
            clock,
            movetime,
            fen,
            threads,
            hash,
        } = self;
        require_networks(cli);
        let engine = tui::EngineConfig {
            threads: threads.max(1),
            hash_mb: hash.max(1),
        };
        let Some(board_mode) = mode.board_mode() else {
            let store = open_coach_store(coach_dir);
            if let Err(e) = tui::run_learn(engine, store) {
                exit_with_error(&format!("terminal error: {e}"));
            }
            return;
        };
        let fen_str = fen.unwrap_or_else(|| START_FEN.to_string());
        let start = Board::from_fen(&fen_str)
            .unwrap_or_else(|e| exit_with_error(&format!("FEN parse error: {e}")));
        let config = tui::PlayConfig {
            start,
            mode: board_mode,
            human: side.color(),
            time: clock.map_or(
                tui::TimeSetting::PerMove { ms: movetime },
                tui::TimeSetting::Clock,
            ),
        };
        match tui::run(config, engine) {
            Ok(record) => println!("{record}"),
            Err(e) => exit_with_error(&format!("terminal error: {e}")),
        }
    }
}

#[derive(Args)]
struct ImportPuzzlesArgs {
    path: String,
    /// Where learn mode keeps your data (defaults to ~/.local/share/chess/coach)
    #[arg(long)]
    coach_dir: Option<std::path::PathBuf>,
    /// Puzzles kept per 100-point rating band
    #[arg(long, default_value_t = 2500)]
    per_band: usize,
}

impl ImportPuzzlesArgs {
    fn run(self) {
        let Self {
            path,
            coach_dir,
            per_band,
        } = self;
        let store = open_coach_store(coach_dir);
        import_puzzles(&path, &store, per_band.max(1));
    }
}

#[derive(Args)]
struct EvalArgs {
    fen: String,
}

impl EvalArgs {
    fn run(self, cli: &Cli) {
        let Self { fen } = self;
        require_networks(cli);
        let mut b = Board::from_fen(&fen)
            .unwrap_or_else(|e| exit_with_error(&format!("FEN parse error: {e}")));
        println!("{}", eval::describe_evaluation(&mut b));
    }
}

#[cfg(feature = "stockfish-nnue")]
#[derive(Args)]
struct EvalTraceArgs {
    /// Position to trace; reads one FEN per line from stdin when omitted
    fen: Option<String>,
    /// Big network file (defaults to the path baked in at build time)
    #[arg(long)]
    big: Option<String>,
    /// Small network file (defaults to the path baked in at build time)
    #[arg(long)]
    small: Option<String>,
}

#[cfg(feature = "stockfish-nnue")]
impl EvalTraceArgs {
    fn run(self) {
        let Self { fen, big, small } = self;
        use eval::stockfish::{NetPaths, load, trace_text};
        let built_in = NetPaths::built_in();
        let paths = match (big, small, built_in) {
            (Some(big), Some(small), _) => NetPaths {
                big: big.into(),
                small: small.into(),
            },
            (big, small, Some(defaults)) => NetPaths {
                big: big.map_or(defaults.big, Into::into),
                small: small.map_or(defaults.small, Into::into),
            },
            _ => {
                exit_with_error("no networks: pass --big and --small or build inside the dev shell")
            }
        };
        let nets =
            load(&paths).unwrap_or_else(|e| exit_with_error(&format!("failed to load NNUE: {e}")));
        let fens: Vec<String> = match fen {
            Some(fen) => vec![fen],
            None => std::io::stdin()
                .lines()
                .map_while(Result::ok)
                .filter(|l| !l.trim().is_empty())
                .collect(),
        };
        for fen in fens {
            let board = Board::from_fen(&fen)
                .unwrap_or_else(|e| exit_with_error(&format!("FEN parse error in {fen}: {e}")));
            print!("{}", trace_text(nets, &board));
        }
    }
}

#[derive(Args)]
struct BenchArgs {
    #[arg(default_value_t = 13)]
    depth: usize,
    /// Transposition table size in MB
    #[arg(long, default_value_t = bench::BENCH_HASH_MB)]
    hash: usize,
}

impl BenchArgs {
    fn run(self, cli: &Cli) {
        let Self { depth, hash } = self;
        require_networks(cli);
        match bench::run(depth, hash) {
            Ok(result) => {
                println!("===========================");
                println!("Total time (ms) : {}", result.elapsed_ms);
                println!("Nodes searched  : {}", result.nodes);
                println!("Nodes/second    : {}", result.nps());
            }
            Err(e) => exit_with_error(&format!("bench failed: {e}")),
        }
    }
}

#[derive(Args)]
struct ChallengeArgs {
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
    /// Name shown for engine 1; defaults to its binary's file name
    #[arg(long = "e1-name")]
    e1_name: Option<String>,
    /// Name shown for engine 2; defaults to its binary's file name
    #[arg(long = "e2-name")]
    e2_name: Option<String>,
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
}

impl ChallengeArgs {
    fn run(self) {
        let Self {
            engine1,
            engine2,
            e1_options,
            e2_options,
            e1_name,
            e2_name,
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
        } = self;
        let pair_count = rounds.div_ceil(2);
        let openings = match openings {
            Some(path) => openings::sample_openings(&path, pair_count, seed)
                .unwrap_or_else(|e| exit_with_error(&e)),
            None => Vec::new(),
        };
        let sprt =
            sprt.map(|spec| SprtConfig::parse(&spec).unwrap_or_else(|e| exit_with_error(&e)));
        tournament::run_tournament(TournamentConfig {
            engine1: EngineSpec {
                name: tournament::engine_name(e1_name, &engine1),
                path: engine1,
                options: parse_options(&e1_options),
                clock: GameClock {
                    base_ms: time,
                    increment_ms: inc,
                },
            },
            engine2: EngineSpec {
                name: tournament::engine_name(e2_name, &engine2),
                path: engine2,
                options: parse_options(&e2_options),
                clock: GameClock {
                    base_ms: e2_time.unwrap_or(time),
                    increment_ms: e2_inc.unwrap_or(inc),
                },
            },
            rounds,
            concurrency,
            start: StartPosition::parse(fen.as_deref().unwrap_or(START_FEN))
                .unwrap_or_else(|e| exit_with_error(&e)),
            openings,
            sprt,
        })
        .unwrap_or_else(|e| exit_with_error(&e));
    }
}

#[derive(Args)]
struct CompareArgs {
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
}

impl CompareArgs {
    fn run(self) {
        let Self {
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
        } = self;
        let fens = match (fen.is_empty(), epd) {
            (false, _) => fen,
            (true, Some(path)) => openings::sample_openings(&path, count, seed)
                .unwrap_or_else(|e| exit_with_error(&e))
                .iter()
                .map(|position| position.fen().to_string())
                .collect(),
            (true, None) => bench::BENCH_FENS.iter().map(|f| f.to_string()).collect(),
        };
        let engine_go =
            movetime.map_or_else(|| format!("depth {depth}"), |ms| format!("movetime {ms}"));
        let reference_go = ref_depth.map_or_else(|| engine_go.clone(), |d| format!("depth {d}"));
        let engine_path = engine.unwrap_or_else(own_binary);
        let reference_path = reference
            .or_else(|| std::env::var("STOCKFISH").ok())
            .unwrap_or_else(|| "stockfish".to_string());
        let config = CompareConfig {
            engine_path,
            reference_path,
            engine_options: options_without_book(&e1_options),
            reference_options: parse_options(&e2_options),
            fens,
            engine_go,
            reference_go,
        };
        if let Err(e) = run_compare(&config) {
            exit_with_error(&e);
        }
    }
}

#[derive(Args)]
struct TuneArgs {
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
}

impl TuneArgs {
    fn run(self) {
        let Self {
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
        } = self;
        let workers = match workers {
            Some(spec) => spsa::parse_worker_groups(&spec).unwrap_or_else(|e| exit_with_error(&e)),
            None => vec![spsa::WorkerGroup {
                cpus: None,
                count: concurrency,
            }],
        };
        let engine = engine.unwrap_or_else(own_binary);
        let start = match resume {
            Some(path) => std::fs::read_to_string(&path)
                .map_err(|e| format!("{path}: {e}"))
                .and_then(|text| spsa::read_estimate(&text))
                .unwrap_or_else(|e| exit_with_error(&e)),
            None => Vec::new(),
        };
        let openings = openings::sample_openings(&openings, 100_000, seed)
            .unwrap_or_else(|e| exit_with_error(&e));
        let tune_options: Vec<String> = std::iter::once(TUNE_HASH_OPTION.to_string())
            .chain(options)
            .collect();
        let config = spsa::SpsaConfig {
            engine,
            engine_options: options_without_book(&tune_options),
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
                    println!(
                        "  {:<24} {:>6} (default {})",
                        spec.name,
                        value.round() as i32,
                        spec.default
                    );
                }
            }
            Err(e) => exit_with_error(&e),
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum PlayMode {
    Casual,
    Game,
    Watch,
    Learn,
}

impl PlayMode {
    /// The board mode, or None for learn mode, which has its own screens.
    const fn board_mode(self) -> Option<tui::Mode> {
        match self {
            Self::Casual => Some(tui::Mode::Casual),
            Self::Game => Some(tui::Mode::Game),
            Self::Watch => Some(tui::Mode::Watch),
            Self::Learn => None,
        }
    }
}

/// Opens learn mode's data directory, or exits saying why it can't.
fn open_coach_store(dir: Option<std::path::PathBuf>) -> chess::coach::store::Store {
    let dir = dir
        .or_else(chess::coach::store::Store::default_dir)
        .unwrap_or_else(|| exit_with_error("no data directory: set HOME or pass --coach-dir"));
    chess::coach::store::Store::open(dir.clone())
        .unwrap_or_else(|e| exit_with_error(&format!("cannot open {}: {e}", dir.display())))
}

fn import_puzzles(path: &str, store: &chess::coach::store::Store, per_band: usize) {
    use chess::coach::store::{ImportFilter, sample_lichess_csv};
    use std::io::BufReader;
    let filter = ImportFilter {
        per_band,
        ..ImportFilter::default()
    };
    let mut rng = rand::thread_rng();
    let sampled = if path == "-" {
        sample_lichess_csv(std::io::stdin().lock(), filter, &mut rng)
    } else {
        let file = std::fs::File::open(path)
            .unwrap_or_else(|e| exit_with_error(&format!("cannot open {path}: {e}")));
        sample_lichess_csv(BufReader::new(file), filter, &mut rng)
    };
    let puzzles = sampled.unwrap_or_else(|e| exit_with_error(&format!("reading {path}: {e}")));
    if puzzles.is_empty() {
        exit_with_error(
            "no puzzles found: expected the Lichess puzzle CSV, decompressed (zstd -dc)",
        );
    }
    store
        .save_lichess_puzzles(&puzzles)
        .unwrap_or_else(|e| exit_with_error(&format!("saving puzzles: {e}")));
    println!(
        "Imported {} puzzles into {}",
        puzzles.len(),
        store.dir().display()
    );
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

impl Cli {
    /// The evaluator chosen on the command line, with its network files.
    fn eval_options(&self) -> eval::EvalOptions {
        let options = eval::EvalOptions::with_kind(self.eval);
        match &self.own_net {
            Some(path) => options.with_own_net(path.clone()),
            None => options,
        }
    }
}

/// Loads the chosen evaluator, or exits: nothing can be evaluated without it.
fn require_networks(cli: &Cli) {
    if let Err(e) = cli.eval_options().load() {
        exit_with_error(&format!(
            "failed to load the {} evaluator: {e}",
            cli.eval.name()
        ));
    }
}

/// The threads this process may run at once, or one if that is unknown.
fn available_threads() -> usize {
    std::thread::available_parallelism().map_or(1, NonZeroUsize::get)
}

fn main() {
    let mut cli = Cli::parse();
    match cli.cmd.take().unwrap_or(Cmd::Uci) {
        Cmd::Perft(args) => args.run(&cli),
        Cmd::Datagen(args) => args.run(&cli),
        Cmd::Play(args) => args.run(&cli),
        Cmd::ImportPuzzles(args) => args.run(),
        Cmd::Eval(args) => args.run(&cli),
        #[cfg(feature = "stockfish-nnue")]
        Cmd::EvalTrace(args) => args.run(),
        Cmd::Bench(args) => args.run(&cli),
        Cmd::Challenge(args) => args.run(),
        Cmd::Compare(args) => args.run(),
        Cmd::Params => print_params(),
        Cmd::Tune(args) => args.run(),
        Cmd::Uci => uci::run_uci(cli.eval_options()).unwrap_or_else(|e| exit_with_error(&e)),
    }
}

fn print_params() {
    println!(
        "{:<24} {:>8} {:>8} {:>8} {:>6}",
        "name", "default", "min", "max", "step"
    );
    for spec in PARAM_SPECS {
        println!(
            "{:<24} {:>8} {:>8} {:>8} {:>6}",
            spec.name, spec.default, spec.min, spec.max, spec.step
        );
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

/// Turns the engine's own book off, then sets `extra`: comparisons and tuning
/// games measure the search, not the book.
fn options_without_book(extra: &[String]) -> Vec<(String, String)> {
    let mut options = vec![("OwnBook".to_string(), "false".to_string())];
    options.extend(parse_options(extra));
    options
}

/// The path of this executable, which plays when no engine is named.
fn own_binary() -> String {
    std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|e| {
            exit_with_error(&format!("cannot locate this binary ({e}); pass --engine"))
        })
}
