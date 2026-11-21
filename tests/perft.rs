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
