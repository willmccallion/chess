//! SPSA tuning of the search parameters from self-play game pairs.
//!
//! Each pair perturbs every parameter by ±c_k, plays the two perturbed engines against
//! each other twice with colors swapped, and moves the parameters toward the winner.
//! Workers run pairs concurrently and update the shared estimate as results arrive
//! (the schedule and step sizes follow OpenBench).

use crate::game_runner::{GameClock, GameConfig, GameEndReason, GameResult, play_game};
use crate::search_params::{PARAM_SPECS, ParamSpec};
use crate::tournament::spawn_and_init_pinned;
use crate::uci_client::UciEngine;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

const ALPHA: f64 = 0.602;
const GAMMA: f64 = 0.101;
/// Stability constant `A` as a fraction of the planned iterations.
const STABILITY_FRACTION: f64 = 0.1;
/// Learning rate at the end of the run, relative to the squared perturbation.
const R_END: f64 = 0.002;
const REPORT_EVERY: usize = 20;
const MAX_MOVES: usize = 300;

/// Workers whose engines all run on one set of CPUs, so both sides of a pair run at the
/// same speed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerGroup {
    /// CPU list in taskset syntax; `None` leaves the engines unpinned.
    pub cpus: Option<String>,
    pub count: usize,
}

/// Parses `cpus:count[,cpus:count...]`, e.g. `0-15:8,16-27:10`.
pub fn parse_worker_groups(spec: &str) -> Result<Vec<WorkerGroup>, String> {
    spec.split(',')
        .map(|group| {
            let (cpus, count) = group
                .rsplit_once(':')
                .ok_or_else(|| format!("expected cpus:count, got '{group}'"))?;
            let valid_cpus =
                !cpus.is_empty() && cpus.chars().all(|c| c.is_ascii_digit() || c == '-');
            if !valid_cpus {
                return Err(format!("invalid CPU list '{cpus}'"));
            }
            let count = count
                .parse::<usize>()
                .map_err(|e| format!("'{count}': {e}"))?;
            Ok(WorkerGroup {
                cpus: Some(cpus.to_string()),
                count,
            })
        })
        .collect()
}

pub struct SpsaConfig {
    pub engine: String,
    pub engine_options: Vec<(String, String)>,
    pub openings: Vec<String>,
    /// Planned number of game pairs (SPSA iterations).
    pub pairs: usize,
    pub base_time_ms: u64,
    pub increment_ms: u64,
    pub workers: Vec<WorkerGroup>,
    pub seed: u64,
    /// Where the current estimate is written; also accepted by `resume`.
    pub output: PathBuf,
    /// Starting values by parameter name (defaults for the rest).
    pub start: Vec<(String, f64)>,
}

/// Step sizes of one SPSA iteration.
#[derive(Copy, Clone)]
struct Gains {
    /// Perturbation, in units of each parameter's `step`.
    perturbation: f64,
    /// Learning rate applied as `a_k / c_k` per unit of `step`.
    learning: f64,
}

/// OpenBench's schedule: `c_k = c / (k+1)^γ`, `a_k = a / (A+k+1)^α`, with `c` and `a`
/// chosen so the final perturbation is one `step` and the final rate `R_END * step²`.
fn gains(iteration: usize, planned: usize) -> Gains {
    let n = planned.max(1) as f64;
    let k = iteration as f64;
    let stability = STABILITY_FRACTION * n;
    let c = n.powf(GAMMA);
    let a = R_END * (stability + n).powf(ALPHA);
    let c_k = c / (k + 1.0).powf(GAMMA);
    let a_k = a / (stability + k + 1.0).powf(ALPHA);
    Gains {
        perturbation: c_k,
        learning: a_k / c_k,
    }
}

struct Estimate {
    theta: Vec<f64>,
    issued: usize,
    completed: usize,
    rng: StdRng,
    score: (i64, i64, i64),
}

/// One pair's assignment: the perturbed values and the direction of the perturbation.
struct PairJob {
    iteration: usize,
    plus: Vec<i32>,
    minus: Vec<i32>,
    signs: Vec<f64>,
    opening: String,
}

impl Estimate {
    fn next_job(&mut self, config: &SpsaConfig) -> Option<PairJob> {
        if self.issued >= config.pairs {
            return None;
        }
        let iteration = self.issued;
        self.issued += 1;
        let gains = gains(iteration, config.pairs);
        let mut job = PairJob {
            iteration,
            plus: Vec::with_capacity(PARAM_SPECS.len()),
            minus: Vec::with_capacity(PARAM_SPECS.len()),
            signs: Vec::with_capacity(PARAM_SPECS.len()),
            opening: config.openings[self.rng.gen_range(0..config.openings.len())].clone(),
        };
        for (spec, &value) in PARAM_SPECS.iter().zip(&self.theta) {
            let sign = if self.rng.gen_bool(0.5) { 1.0 } else { -1.0 };
            let shift = gains.perturbation * f64::from(spec.step) * sign;
            job.plus.push(spec.clamp((value + shift).round() as i32));
            job.minus.push(spec.clamp((value - shift).round() as i32));
            job.signs.push(sign);
        }
        Some(job)
    }

    /// `result` is the plus engine's wins minus losses over the pair.
    fn apply(&mut self, job: &PairJob, result: i32, planned: usize) {
        let gains = gains(job.iteration, planned);
        for ((value, spec), sign) in self.theta.iter_mut().zip(PARAM_SPECS).zip(&job.signs) {
            let step = f64::from(spec.step);
            *value += gains.learning * step * f64::from(result) * sign;
            *value = value.clamp(f64::from(spec.min), f64::from(spec.max));
        }
        self.completed += 1;
    }
}

fn format_estimate(theta: &[f64]) -> String {
    let mut out = String::new();
    for (spec, value) in PARAM_SPECS.iter().zip(theta) {
        let _ = writeln!(out, "{} {:.3}", spec.name, value);
    }
    out
}

/// Starting values from a file written by a previous run (`Name value` per line).
pub fn read_estimate(text: &str) -> Result<Vec<(String, f64)>, String> {
    let mut values = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.split_whitespace();
        let (Some(name), Some(value), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!("expected 'Name value', got '{line}'"));
        };
        if ParamSpec::find(name).is_none() {
            return Err(format!("unknown parameter '{name}'"));
        }
        let value = value.parse::<f64>().map_err(|e| format!("{name}: {e}"))?;
        values.push((name.to_string(), value));
    }
    Ok(values)
}

fn set_params(engine: &mut UciEngine, values: &[i32]) -> Result<(), String> {
    let options: Vec<(String, String)> = PARAM_SPECS
        .iter()
        .zip(values)
        .map(|(spec, value)| (spec.name.to_string(), value.to_string()))
        .collect();
    engine.set_options(&options)?;
    engine.is_ready()
}

/// Points of the engine playing `color_is_white`, as +1 / 0 / -1.
fn signed_points(result: GameResult, plays_white: bool) -> i32 {
    match (result, plays_white) {
        (GameResult::Draw, _) => 0,
        (GameResult::WhiteWin, true) | (GameResult::BlackWin, false) => 1,
        _ => -1,
    }
}

/// Plays the pair; `None` if an engine crashed, so the pair says nothing about the values.
fn play_pair(
    plus: &mut UciEngine,
    minus: &mut UciEngine,
    job: &PairJob,
    config: &SpsaConfig,
) -> Result<Option<(i32, [GameResult; 2])>, String> {
    set_params(plus, &job.plus)?;
    set_params(minus, &job.minus)?;
    let clock = GameClock {
        base_ms: config.base_time_ms,
        increment_ms: config.increment_ms,
    };
    let game = GameConfig {
        white_clock: clock,
        black_clock: clock,
        max_moves: MAX_MOVES,
        start_fen: Some(job.opening.clone()),
    };
    let first = play_game(plus, minus, &game);
    let second = play_game(minus, plus, &game);
    if first.reason == GameEndReason::EngineCrash || second.reason == GameEndReason::EngineCrash {
        return Ok(None);
    }
    let result = signed_points(first.result, true) + signed_points(second.result, false);
    Ok(Some((result, [first.result, second.result])))
}

fn tally(score: &mut (i64, i64, i64), result: GameResult, plus_is_white: bool) {
    match signed_points(result, plus_is_white) {
        1 => score.0 += 1,
        -1 => score.1 += 1,
        _ => score.2 += 1,
    }
}

fn report(estimate: &Estimate, config: &SpsaConfig, started: Instant) {
    let elapsed = started.elapsed().as_secs_f64();
    let (wins, losses, draws) = estimate.score;
    println!(
        "pairs {}/{}  plus-vs-minus W/L/D {wins}/{losses}/{draws}  {:.1} pairs/min",
        estimate.completed,
        config.pairs,
        estimate.completed as f64 / elapsed * 60.0
    );
    let text = format_estimate(&estimate.theta);
    if let Err(e) = fs::write(&config.output, &text) {
        eprintln!("cannot write {}: {e}", config.output.display());
    }
}

fn worker(
    config: &SpsaConfig,
    cpus: Option<&str>,
    estimate: &Mutex<Estimate>,
    started: Instant,
) -> Result<(), String> {
    let spawn = || spawn_and_init_pinned(&config.engine, &config.engine_options, cpus);
    let mut plus = spawn()?;
    let mut minus = spawn()?;
    loop {
        let job = {
            let mut state = estimate.lock().map_err(|_| "estimate lock poisoned")?;
            state.next_job(config)
        };
        let Some(job) = job else {
            return Ok(());
        };
        match play_pair(&mut plus, &mut minus, &job, config)? {
            Some((result, games)) => {
                let mut state = estimate.lock().map_err(|_| "estimate lock poisoned")?;
                state.apply(&job, result, config.pairs);
                tally(&mut state.score, games[0], true);
                tally(&mut state.score, games[1], false);
                if state.completed % REPORT_EVERY == 0 {
                    report(&state, config, started);
                }
            }
            None => {
                eprintln!("engine crashed; restarting both and discarding the pair");
                plus = spawn()?;
                minus = spawn()?;
            }
        }
    }
}

/// Runs the tuning session and returns the final estimate.
pub fn run(config: SpsaConfig) -> Result<Vec<f64>, String> {
    if config.openings.is_empty() {
        return Err("no openings".to_string());
    }
    let mut theta: Vec<f64> = PARAM_SPECS
        .iter()
        .map(|spec| f64::from(spec.default))
        .collect();
    for (name, value) in &config.start {
        if let Some(index) = PARAM_SPECS
            .iter()
            .position(|spec| spec.name.eq_ignore_ascii_case(name))
        {
            theta[index] = *value;
        }
    }
    let estimate = Mutex::new(Estimate {
        theta,
        issued: 0,
        completed: 0,
        rng: StdRng::seed_from_u64(config.seed),
        score: (0, 0, 0),
    });
    let config = Arc::new(config);
    let estimate = Arc::new(estimate);
    let started = Instant::now();

    let total_workers: usize = config.workers.iter().map(|group| group.count).sum();
    if total_workers == 0 {
        return Err("no workers".to_string());
    }
    println!(
        "SPSA: {} parameters, {} pairs at {}+{}ms, {} workers, writing {}",
        PARAM_SPECS.len(),
        config.pairs,
        config.base_time_ms,
        config.increment_ms,
        total_workers,
        config.output.display()
    );
    let handles: Vec<_> = config
        .workers
        .iter()
        .flat_map(|group| std::iter::repeat_n(group.cpus.clone(), group.count))
        .map(|cpus| {
            let (config, estimate) = (Arc::clone(&config), Arc::clone(&estimate));
            thread::spawn(move || worker(&config, cpus.as_deref(), &estimate, started))
        })
        .collect();
    for handle in handles {
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("worker stopped: {e}"),
            Err(_) => eprintln!("worker panicked"),
        }
    }

    let state = estimate.lock().map_err(|_| "estimate lock poisoned")?;
    report(&state, &config, started);
    Ok(state.theta.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_groups_parse_cpu_lists_and_counts() {
        let groups = parse_worker_groups("0-15:8,16-27:10").unwrap();

        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0],
            WorkerGroup {
                cpus: Some("0-15".into()),
                count: 8
            }
        );
        assert_eq!(groups[1].count, 10);
        assert!(parse_worker_groups("0-15").is_err());
        assert!(parse_worker_groups("a-b:3").is_err());
    }

    #[test]
    fn schedule_ends_at_one_step_and_the_final_rate() {
        let planned = 10_000;

        let last = gains(planned - 1, planned);

        assert!(
            (last.perturbation - 1.0).abs() < 1e-9,
            "{}",
            last.perturbation
        );
        assert!((last.learning - R_END).abs() < 1e-9, "{}", last.learning);
        assert!(gains(0, planned).perturbation > last.perturbation);
    }

    #[test]
    fn estimate_round_trips_through_its_file_format() {
        let theta: Vec<f64> = PARAM_SPECS
            .iter()
            .map(|s| f64::from(s.default) + 0.25)
            .collect();

        let parsed = read_estimate(&format_estimate(&theta)).unwrap();

        assert_eq!(parsed.len(), PARAM_SPECS.len());
        for ((name, value), (spec, expected)) in parsed.iter().zip(PARAM_SPECS.iter().zip(&theta)) {
            assert_eq!(name, spec.name);
            assert!((value - expected).abs() < 1e-9);
        }
    }

    #[test]
    fn a_won_pair_moves_values_toward_the_plus_side() {
        let config = SpsaConfig {
            engine: String::new(),
            engine_options: Vec::new(),
            openings: vec![crate::types::START_FEN.to_string()],
            pairs: 100,
            base_time_ms: 0,
            increment_ms: 0,
            workers: Vec::new(),
            seed: 3,
            output: PathBuf::new(),
            start: Vec::new(),
        };
        let mut estimate = Estimate {
            theta: PARAM_SPECS.iter().map(|s| f64::from(s.default)).collect(),
            issued: 0,
            completed: 0,
            rng: StdRng::seed_from_u64(3),
            score: (0, 0, 0),
        };
        let before = estimate.theta.clone();

        let job = estimate.next_job(&config).unwrap();
        estimate.apply(&job, 2, config.pairs);

        for (i, spec) in PARAM_SPECS.iter().enumerate() {
            let moved = estimate.theta[i] - before[i];
            let toward_plus = f64::from(job.plus[i]) - before[i];
            if toward_plus.abs() > 0.0
                && (before[i] > f64::from(spec.min) && before[i] < f64::from(spec.max))
            {
                assert!(
                    moved * toward_plus > 0.0,
                    "{} moved {moved} plus {toward_plus}",
                    spec.name
                );
            }
        }
    }
}
