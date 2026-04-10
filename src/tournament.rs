use crate::game_runner::{play_game, GameConfig, GameEndReason, GameResult};
use crate::uci_client::UciEngine;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

pub struct TournamentConfig {
    pub engine1_path: String,
    pub engine2_path: String,
    pub e1_options: Vec<(String, String)>,
    pub e2_options: Vec<(String, String)>,
    pub base_time_ms: u64,
    pub increment_ms: u64,
    pub rounds: usize,
    pub concurrency: usize,
    pub start_fen: Option<String>,
}

struct Score {
    e1_wins: usize,
    e2_wins: usize,
    draws: usize,
    completed: usize,

    // As white / as black (from e1's perspective)
    e1_wins_as_white: usize,
    e1_wins_as_black: usize,
    e2_wins_as_white: usize,
    e2_wins_as_black: usize,
    draws_e1_white: usize,
    draws_e1_black: usize,

    // Termination reasons
    checkmates: usize,
    stalemates: usize,
    repetitions: usize,
    fifty_move: usize,
    flag_falls: usize,
    move_limits: usize,
    illegal_moves: usize,
    crashes: usize,

    // Game length tracking
    total_plies: usize,
    shortest_game: usize,
    longest_game: usize,
}

impl Score {
    fn new() -> Self {
        Self {
            e1_wins: 0,
            e2_wins: 0,
            draws: 0,
            completed: 0,
            e1_wins_as_white: 0,
            e1_wins_as_black: 0,
            e2_wins_as_white: 0,
            e2_wins_as_black: 0,
            draws_e1_white: 0,
            draws_e1_black: 0,
            checkmates: 0,
            stalemates: 0,
            repetitions: 0,
            fifty_move: 0,
            flag_falls: 0,
            move_limits: 0,
            illegal_moves: 0,
            crashes: 0,
            total_plies: 0,
            shortest_game: usize::MAX,
            longest_game: 0,
        }
    }
}

pub fn parse_option(s: &str) -> Result<(String, String), String> {
    let (name, value) = s
        .split_once('=')
        .ok_or_else(|| format!("Invalid option '{}', expected Name=Value", s))?;
    Ok((name.trim().to_string(), value.trim().to_string()))
}

fn elo_diff(wins: usize, losses: usize, draws: usize) -> Option<f64> {
    let total = (wins + losses + draws) as f64;
    if total == 0.0 {
        return None;
    }
    let score = (wins as f64 + draws as f64 * 0.5) / total;
    if score <= 0.0 || score >= 1.0 {
        return None;
    }
    Some(-400.0 * (1.0 / score - 1.0).log10())
}

fn elo_error_95(wins: usize, losses: usize, draws: usize) -> Option<f64> {
    let total = (wins + losses + draws) as f64;
    if total < 2.0 {
        return None;
    }
    let score = (wins as f64 + draws as f64 * 0.5) / total;
    if score <= 0.0 || score >= 1.0 {
        return None;
    }
    let se = (score * (1.0 - score) / total).sqrt();
    let lo = (score - 1.96 * se).max(0.001);
    let hi = (score + 1.96 * se).min(0.999);
    let elo_lo = -400.0 * (1.0 / lo - 1.0).log10();
    let elo_hi = -400.0 * (1.0 / hi - 1.0).log10();
    Some((elo_hi - elo_lo) / 2.0)
}

fn format_elo(wins: usize, losses: usize, draws: usize) -> String {
    match elo_diff(wins, losses, draws) {
        Some(e) => format!("{:+.0}", e),
        None if wins > losses => "+inf".to_string(),
        None if losses > wins => "-inf".to_string(),
        _ => "0".to_string(),
    }
}

fn format_pct(n: usize, total: usize) -> String {
    if total == 0 {
        "  -  ".to_string()
    } else {
        format!("{:5.1}%", n as f64 / total as f64 * 100.0)
    }
}

fn spawn_and_init(path: &str, options: &[(String, String)]) -> Result<UciEngine, String> {
    let mut engine = UciEngine::spawn(path)?;
    engine.init_uci()?;
    if !options.iter().any(|(k, _)| k.eq_ignore_ascii_case("Threads")) {
        engine.set_options(&[("Threads".to_string(), "1".to_string())])?;
    }
    engine.set_options(options)?;
    engine.is_ready()?;
    Ok(engine)
}

fn print_summary(s: &Score, e1_name: &str, e2_name: &str, elapsed: f64) {
    let total = s.completed;
    let e1_score = s.e1_wins as f64 + s.draws as f64 * 0.5;
    let e2_score = s.e2_wins as f64 + s.draws as f64 * 0.5;
    let elo = format_elo(s.e1_wins, s.e2_wins, s.draws);
    let err = elo_error_95(s.e1_wins, s.e2_wins, s.draws);
    let draw_rate = if total > 0 {
        s.draws as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    let avg_len = if total > 0 {
        s.total_plies as f64 / total as f64
    } else {
        0.0
    };

    // Width for engine name columns
    let w = e1_name.len().max(e2_name.len()).max(6);

    println!();
    println!("=================================================================");
    println!("  TOURNAMENT RESULTS");
    println!("=================================================================");
    println!();

    // Score line
    let elo_str = match err {
        Some(e) => format!("{} +/- {:.0} Elo", elo, e),
        None => format!("{} Elo", elo),
    };
    println!(
        "  Score: {:.1} - {:.1}  [{elo_str}]",
        e1_score, e2_score
    );
    println!(
        "  Games: {}    Draw rate: {:.1}%",
        total, draw_rate
    );
    if elapsed > 0.0 {
        let gps = total as f64 / elapsed;
        println!(
            "  Time:  {:.1}s ({:.1} games/sec)",
            elapsed, gps
        );
    }
    println!();

    // Head-to-head table
    println!("  +-{0:-<w$}-+------+------+------+-------+-------+",
        "", w = w);
    println!(
        "  | {0:<w$} | {1:>4} | {2:>4} | {3:>4} | {4:>5} | {5:>5} |",
        "Engine", "W", "L", "D", "Score", "Pts", w = w
    );
    println!("  +-{0:-<w$}-+------+------+------+-------+-------+",
        "", w = w);
    println!(
        "  | {name:<w$} | {wins:>4} | {losses:>4} | {draws:>4} | {pct} | {pts:>5.1} |",
        name = e1_name,
        wins = s.e1_wins,
        losses = s.e2_wins,
        draws = s.draws,
        pct = format_pct(s.e1_wins * 2 + s.draws, total * 2),
        pts = e1_score,
        w = w,
    );
    println!(
        "  | {name:<w$} | {wins:>4} | {losses:>4} | {draws:>4} | {pct} | {pts:>5.1} |",
        name = e2_name,
        wins = s.e2_wins,
        losses = s.e1_wins,
        draws = s.draws,
        pct = format_pct(s.e2_wins * 2 + s.draws, total * 2),
        pts = e2_score,
        w = w,
    );
    println!("  +-{0:-<w$}-+------+------+------+-------+-------+",
        "", w = w);
    println!();

    // White/Black breakdown
    let e1_as_white = s.e1_wins_as_white + s.e2_wins_as_white + s.draws_e1_white;
    let e1_as_black = s.e1_wins_as_black + s.e2_wins_as_black + s.draws_e1_black;

    println!("  White/Black Breakdown (from {}'s perspective):", e1_name);
    println!("  +-----------+------+------+------+-------+");
    println!("  | Color     | {0:>4} | {1:>4} | {2:>4} | {3:>5} |", "W", "L", "D", "Score");
    println!("  +-----------+------+------+------+-------+");
    if e1_as_white > 0 {
        println!(
            "  | As white  | {:>4} | {:>4} | {:>4} | {} |",
            s.e1_wins_as_white,
            s.e2_wins_as_white,
            s.draws_e1_white,
            format_pct(s.e1_wins_as_white * 2 + s.draws_e1_white, e1_as_white * 2),
        );
    }
    if e1_as_black > 0 {
        println!(
            "  | As black  | {:>4} | {:>4} | {:>4} | {} |",
            s.e1_wins_as_black,
            s.e2_wins_as_black,
            s.draws_e1_black,
            format_pct(s.e1_wins_as_black * 2 + s.draws_e1_black, e1_as_black * 2),
        );
    }
    println!("  +-----------+------+------+------+-------+");
    println!();

    // Termination stats
    println!("  Terminations:");
    let reasons: Vec<(&str, usize)> = vec![
        ("Checkmate", s.checkmates),
        ("Stalemate", s.stalemates),
        ("Repetition", s.repetitions),
        ("50-move rule", s.fifty_move),
        ("Flag fall", s.flag_falls),
        ("Move limit", s.move_limits),
        ("Illegal move", s.illegal_moves),
        ("Engine crash", s.crashes),
    ];
    for (name, count) in &reasons {
        if *count > 0 {
            println!(
                "    {:<14} {:>4}  ({})",
                name,
                count,
                format_pct(*count, total),
            );
        }
    }
    println!();

    // Game length stats
    println!("  Game Length:");
    println!("    Average:  {:.0} plies ({:.0} moves)", avg_len, avg_len / 2.0);
    if s.shortest_game != usize::MAX {
        println!(
            "    Shortest: {} plies ({} moves)",
            s.shortest_game,
            s.shortest_game / 2
        );
    }
    println!(
        "    Longest:  {} plies ({} moves)",
        s.longest_game,
        s.longest_game / 2
    );
    println!();
    println!("=================================================================");
}

pub fn run_tournament(config: TournamentConfig) {
    if !std::path::Path::new(&config.engine1_path).exists() {
        eprintln!("Error: engine1 path '{}' not found", config.engine1_path);
        std::process::exit(1);
    }
    if !std::path::Path::new(&config.engine2_path).exists() {
        eprintln!("Error: engine2 path '{}' not found", config.engine2_path);
        std::process::exit(1);
    }

    let e1_name = config
        .engine1_path
        .rsplit('/')
        .next()
        .unwrap_or(&config.engine1_path);
    let e2_name = config
        .engine2_path
        .rsplit('/')
        .next()
        .unwrap_or(&config.engine2_path);

    println!("Tournament: {} vs {}", e1_name, e2_name);
    println!(
        "Time control: {}ms + {}ms/move",
        config.base_time_ms, config.increment_ms
    );
    println!(
        "Rounds: {}, Concurrency: {}",
        config.rounds, config.concurrency
    );
    if let Some(ref fen) = config.start_fen {
        println!("Start FEN: {}", fen);
    }
    println!("---");

    let start_time = Instant::now();
    let next_game = Arc::new(AtomicUsize::new(0));
    let score = Arc::new(Mutex::new(Score::new()));
    let config = Arc::new(config);
    let mut handles = Vec::new();

    for _ in 0..config.concurrency {
        let next_game = Arc::clone(&next_game);
        let score = Arc::clone(&score);
        let config = Arc::clone(&config);

        let handle = thread::spawn(move || {
            let mut e1 = match spawn_and_init(&config.engine1_path, &config.e1_options) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("Failed to start engine1: {}", e);
                    return;
                }
            };
            let mut e2 = match spawn_and_init(&config.engine2_path, &config.e2_options) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("Failed to start engine2: {}", e);
                    return;
                }
            };

            let e1_name = config
                .engine1_path
                .rsplit('/')
                .next()
                .unwrap_or(&config.engine1_path);
            let e2_name = config
                .engine2_path
                .rsplit('/')
                .next()
                .unwrap_or(&config.engine2_path);

            loop {
                let game_idx = next_game.fetch_add(1, Ordering::Relaxed);
                if game_idx >= config.rounds {
                    break;
                }

                let game_config = GameConfig {
                    base_time_ms: config.base_time_ms,
                    increment_ms: config.increment_ms,
                    max_moves: 300,
                    start_fen: config.start_fen.clone(),
                };

                let e1_is_white = game_idx % 2 == 0;
                let (white, black) = if e1_is_white {
                    (&mut e1, &mut e2)
                } else {
                    (&mut e2, &mut e1)
                };

                let (white_name, black_name) = if e1_is_white {
                    (e1_name, e2_name)
                } else {
                    (e2_name, e1_name)
                };

                let outcome = play_game(white, black, &game_config);

                let (e1_win, e2_win) = match outcome.result {
                    GameResult::WhiteWin => (e1_is_white, !e1_is_white),
                    GameResult::BlackWin => (!e1_is_white, e1_is_white),
                    GameResult::Draw => (false, false),
                };

                let result_str = match outcome.result {
                    GameResult::WhiteWin => "1-0",
                    GameResult::BlackWin => "0-1",
                    GameResult::Draw => "1/2-1/2",
                };

                let mut s = score.lock().unwrap();

                // Basic score
                if e1_win {
                    s.e1_wins += 1;
                } else if e2_win {
                    s.e2_wins += 1;
                } else {
                    s.draws += 1;
                }
                s.completed += 1;

                // White/Black breakdown
                if e1_is_white {
                    if e1_win {
                        s.e1_wins_as_white += 1;
                    } else if e2_win {
                        s.e2_wins_as_white += 1;
                    } else {
                        s.draws_e1_white += 1;
                    }
                } else {
                    if e1_win {
                        s.e1_wins_as_black += 1;
                    } else if e2_win {
                        s.e2_wins_as_black += 1;
                    } else {
                        s.draws_e1_black += 1;
                    }
                }

                // Termination reason
                match outcome.reason {
                    GameEndReason::Checkmate => s.checkmates += 1,
                    GameEndReason::Stalemate => s.stalemates += 1,
                    GameEndReason::Repetition => s.repetitions += 1,
                    GameEndReason::FiftyMoveRule => s.fifty_move += 1,
                    GameEndReason::FlagFall => s.flag_falls += 1,
                    GameEndReason::MoveLimitExceeded => s.move_limits += 1,
                    GameEndReason::IllegalMove => s.illegal_moves += 1,
                    GameEndReason::EngineCrash => s.crashes += 1,
                }

                // Game length
                s.total_plies += outcome.move_count;
                if outcome.move_count < s.shortest_game {
                    s.shortest_game = outcome.move_count;
                }
                if outcome.move_count > s.longest_game {
                    s.longest_game = outcome.move_count;
                }

                let e1_score = s.e1_wins as f64 + s.draws as f64 * 0.5;
                let e2_score = s.e2_wins as f64 + s.draws as f64 * 0.5;
                let elo = format_elo(s.e1_wins, s.e2_wins, s.draws);

                println!(
                    "Game {:>3}/{}: {} vs {} -> {} ({}, {} plies)  [{} {:.1} - {:.1} {}] [{}]",
                    s.completed,
                    config.rounds,
                    white_name,
                    black_name,
                    result_str,
                    outcome.reason,
                    outcome.move_count,
                    e1_name,
                    e1_score,
                    e2_score,
                    e2_name,
                    elo,
                );

                // If an engine crashed, try to respawn it
                if outcome.reason == GameEndReason::EngineCrash {
                    if let Ok(new_e1) =
                        spawn_and_init(&config.engine1_path, &config.e1_options)
                    {
                        e1 = new_e1;
                    }
                    if let Ok(new_e2) =
                        spawn_and_init(&config.engine2_path, &config.e2_options)
                    {
                        e2 = new_e2;
                    }
                }
            }

            e1.quit();
            e2.quit();
        });

        handles.push(handle);
    }

    for h in handles {
        let _ = h.join();
    }

    let elapsed = start_time.elapsed().as_secs_f64();

    let s = score.lock().unwrap();
    let e1_name = config
        .engine1_path
        .rsplit('/')
        .next()
        .unwrap_or(&config.engine1_path);
    let e2_name = config
        .engine2_path
        .rsplit('/')
        .next()
        .unwrap_or(&config.engine2_path);

    print_summary(&s, e1_name, e2_name, elapsed);
}
