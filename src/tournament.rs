use crate::game_runner::{play_game, GameConfig, GameResult};
use crate::uci_client::UciEngine;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

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

fn format_elo(wins: usize, losses: usize, draws: usize) -> String {
    match elo_diff(wins, losses, draws) {
        Some(e) => format!("{:+.0} Elo", e),
        None if wins > losses => "+inf Elo".to_string(),
        None if losses > wins => "-inf Elo".to_string(),
        _ => "0 Elo".to_string(),
    }
}

fn spawn_and_init(
    path: &str,
    options: &[(String, String)],
) -> Result<UciEngine, String> {
    let mut engine = UciEngine::spawn(path)?;
    engine.init_uci()?;
    engine.set_options(options)?;
    engine.is_ready()?;
    Ok(engine)
}

pub fn run_tournament(config: TournamentConfig) {
    // Validate engine paths
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

    let next_game = Arc::new(AtomicUsize::new(0));
    let score = Arc::new(Mutex::new(Score {
        e1_wins: 0,
        e2_wins: 0,
        draws: 0,
        completed: 0,
    }));

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

                // Alternate colors: even games e1=white, odd games e1=black
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

                // Map result to e1/e2 perspective
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
                if e1_win {
                    s.e1_wins += 1;
                } else if e2_win {
                    s.e2_wins += 1;
                } else {
                    s.draws += 1;
                }
                s.completed += 1;

                let e1_score = s.e1_wins as f64 + s.draws as f64 * 0.5;
                let e2_score = s.e2_wins as f64 + s.draws as f64 * 0.5;
                let elo = format_elo(s.e1_wins, s.e2_wins, s.draws);

                println!(
                    "Game {:>3}/{}: {} vs {} -> {} ({}, {} plies)",
                    s.completed,
                    config.rounds,
                    white_name,
                    black_name,
                    result_str,
                    outcome.reason,
                    outcome.move_count,
                );
                println!(
                    "Score: {} {:.1} - {} {:.1} [{}] ({}/{})",
                    e1_name,
                    e1_score,
                    e2_name,
                    e2_score,
                    elo,
                    s.completed,
                    config.rounds,
                );
                println!("---");

                // If an engine crashed, try to respawn it
                if outcome.reason == crate::game_runner::GameEndReason::EngineCrash {
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

    // Final summary
    let s = score.lock().unwrap();
    let e1_score = s.e1_wins as f64 + s.draws as f64 * 0.5;
    let e2_score = s.e2_wins as f64 + s.draws as f64 * 0.5;
    let elo = format_elo(s.e1_wins, s.e2_wins, s.draws);

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

    println!("\nTournament complete!");
    println!(
        "Final: {} {:.1} - {} {:.1} [{}]",
        e1_name, e1_score, e2_name, e2_score, elo
    );
    println!(
        "W/L/D: {} / {} / {} ({} games)",
        s.e1_wins, s.e2_wins, s.draws, s.completed
    );
}
