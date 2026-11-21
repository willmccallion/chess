//! The `perft` subcommand run as a process.

use std::process::{Command, Output};

fn chess(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_chess"))
        .args(args)
        .output()
        .expect("chess runs")
}

#[test]
fn perft_runs_without_loading_the_evaluator() {
    let output = chess(&[
        "--eval",
        "own",
        "--own-net",
        "/nonexistent/net.bin",
        "perft",
        "2",
    ]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout.trim(), "perft(2) = 400");
}

#[test]
fn divide_names_each_promotion_in_uci_notation() {
    let output = chess(&[
        "perft",
        "1",
        "--divide",
        "--fen",
        "4k3/1P6/8/8/8/8/8/4K3 w - - 0 1",
    ]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut promotions: Vec<&str> = stdout.lines().filter(|l| l.starts_with("b7b8")).collect();
    promotions.sort_unstable();
    assert_eq!(promotions, ["b7b8b: 1", "b7b8n: 1", "b7b8q: 1", "b7b8r: 1"]);
}
