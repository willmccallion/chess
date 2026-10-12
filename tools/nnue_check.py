#!/usr/bin/env python3
"""Compare the engine's Stockfish 18 network trace with Stockfish's own `eval` output.

Positions come from random playouts of EPD openings, so they cover every material
count, both sides to move, kings on every file, promotions and castling rights.
"""
import argparse
import random
import re
import subprocess
import sys

import chess

ROW = re.compile(r"^\|\s+(\d)\s+\|(.*)\|(.*)\|(.*)\|")
NNUE_LINE = re.compile(r"^NNUE evaluation\s+(\S+)")
FINAL_LINE = re.compile(r"^Final evaluation\s+(\S+)")


def sample_positions(epd_path, count, seed):
    rng = random.Random(seed)
    with open(epd_path) as f:
        openings = [" ".join(line.split()[:4]) + " 0 1" for line in f if line.strip()]
    positions = []
    while len(positions) < count:
        board = chess.Board(rng.choice(openings))
        for _ in range(rng.randrange(0, 160)):
            moves = list(board.legal_moves)
            if not moves:
                break
            board.push(rng.choice(moves))
            if not board.is_check() and rng.random() < 0.08:
                positions.append(board.fen())
                if len(positions) >= count:
                    break
    return positions


def parse_trace(text):
    """Bucket rows, the big-net evaluation and the final evaluation of one trace, as strings."""
    rows = []
    nnue = None
    final = None
    for line in text.splitlines():
        m = ROW.match(line)
        if m:
            rows.append(tuple(cell.replace(" ", "") for cell in m.groups()[1:]))
        m = NNUE_LINE.match(line)
        if m:
            nnue = m.group(1)
        m = FINAL_LINE.match(line)
        if m:
            final = m.group(1)
    return rows, nnue, final


def stockfish_traces(stockfish, fens):
    commands = "".join(f"position fen {fen}\neval\n" for fen in fens) + "quit\n"
    out = subprocess.run([stockfish], input=commands, capture_output=True, text=True, check=True).stdout
    chunks = out.split("NNUE network contributions")[1:]
    return [parse_trace(chunk) for chunk in chunks]


def engine_traces(engine, fens):
    out = subprocess.run([engine, "eval-trace"], input="\n".join(fens) + "\n",
                         capture_output=True, text=True, check=True).stdout
    chunks = out.split("NNUE network contributions")[1:]
    return [parse_trace(chunk) for chunk in chunks]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--engine", default="target/release/chess")
    parser.add_argument("--stockfish", required=True)
    parser.add_argument("--epd", required=True)
    parser.add_argument("--count", type=int, default=2000)
    parser.add_argument("--seed", type=int, default=1)
    args = parser.parse_args()

    fens = sample_positions(args.epd, args.count, args.seed)
    ours = engine_traces(args.engine, fens)
    theirs = stockfish_traces(args.stockfish, fens)
    if len(ours) != len(fens) or len(theirs) != len(fens):
        sys.exit(f"trace count mismatch: {len(ours)} ours, {len(theirs)} stockfish, {len(fens)} fens")

    mismatches = 0
    for fen, a, b in zip(fens, ours, theirs):
        if a != b or len(a[0]) != 8:
            mismatches += 1
            if mismatches <= 5:
                print(f"MISMATCH {fen}\n  ours:      {a}\n  stockfish: {b}")
    print(f"{len(fens) - mismatches}/{len(fens)} positions match")
    sys.exit(1 if mismatches else 0)


if __name__ == "__main__":
    main()
