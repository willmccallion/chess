#!/usr/bin/env python3
# -*- coding: utf-8 -*-

import argparse
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from typing import Dict, Iterable, List, Optional, Tuple

try:
    import chess
except ImportError:
    chess = None

class Colors:
    GREEN = '\033[92m'
    RED = '\033[91m'
    YELLOW = '\033[93m'
    CYAN = '\033[96m'
    BOLD = '\033[1m'
    DIM = '\033[2m'
    END = '\033[0m'

DEFAULT_ENGINE = "./target/release/chess"
DEFAULT_STOCKFISH = "/bin/stockfish"
DEFAULT_TIMEOUT = 300  # seconds per call
DEFAULT_DEPTHS = [1, 2, 3, 4, 5]
SF_SAFE_OPTIONS = {
    "Threads": "1",
    "Hash": "16",
    "SyzygyPath": "",
    "SyzygyProbeDepth": "0",
    "UCI_Chess960": "false",
    "MultiPV": "1",
}

@dataclass
class Position:
    name: str
    fen: str
    max_depth: int  # max depth to test for this fen

def fmt_int(n: Optional[int]) -> str:
    if n is None:
        return "?"
    return f"{n:,}"

def tail(s: str, nlines: int = 20) -> str:
    lines = s.strip().splitlines()
    return "\n".join(lines[-nlines:]) if lines else ""

def print_header(engine_path: str, sf_path: str, positions: int, depths: Iterable[int]):
    dlist = ",".join(str(d) for d in depths)
    print(f"{Colors.BOLD}Starting Chess Engine Perft Test Suite (Stockfish baseline){Colors.END}")
    print(f" Your engine : {engine_path}")
    print(f" Stockfish   : {sf_path}")
    print(f" Positions   : {positions}")
    print(f" Depths      : {dlist}\n")

_sf_move_line = re.compile(r"^[a-h][1-8][a-h][1-8][nbrqNBRQ]?:\s*([0-9]+)\s*$")
_sf_nodes_line = re.compile(r"^Nodes searched:\s*([0-9]+)\s*$", re.IGNORECASE)
_any_number = re.compile(r"([0-9]+)")

def parse_stockfish_perft(stdout: str) -> Optional[int]:
    """
    Parse Stockfish 'go perft N' output.
    Accept either:
      - 'Nodes searched: N'
      - Sum of per-move lines: 'e2e4: 20'
    If neither appears, return None (do NOT guess).
    """
    total = 0
    seen_move_lines = False

    for raw in stdout.splitlines():
        line = raw.strip()

        m2 = _sf_nodes_line.match(line)
        if m2:
            try:
                return int(m2.group(1))
            except ValueError:
                return None

        m = _sf_move_line.match(line)
        if m:
            seen_move_lines = True
            total += int(m.group(1))

    if seen_move_lines:
        return total if total >= 0 else None

    return None

# Your engine parser: last integer in last line by default
def parse_engine_perft(stdout: str) -> Optional[int]:
    lines = [ln for ln in stdout.strip().splitlines() if ln.strip()]
    if not lines:
        return None
    last = lines[-1]
    nums = _any_number.findall(last)
    if nums:
        return int(nums[-1])
    nums_all = [int(x) for x in _any_number.findall(stdout)]
    return max(nums_all) if nums_all else None

_ep_square = re.compile(r"^[a-h][36]$")  # only ranks 3 or 6 make sense for ep target

def plausible_fen(fen: str) -> bool:
    try:
        parts = fen.split()
        if len(parts) < 4:
            return False
        board, stm, castling, ep = parts[:4]
        # Must contain exactly one white king and one black king
        if 'K' not in board or 'k' not in board:
            return False
        if stm not in ('w', 'b'):
            return False
        # EP square must be "-" or like "e3"/"e6"
        if ep != '-' and not _ep_square.match(ep):
            return False
        # Positions where the side not to move is in check are illegal; engines may
        # legitimately reject them, so they say nothing about move generation.
        if chess is not None and not chess.Board(fen).is_valid():
            return False
        return True
    # If anything odd happens, treat as implausible
    except Exception:
        return False

_invalid_fen_pat = re.compile(
    r"(invalid\s+(fen|position)|no\s+king|illegal\s+position|error\s+in\s+fen)",
    re.IGNORECASE
)

def run_engine_perft(
    engine_path: str,
    fen: str,
    depth: int,
    timeout_sec: int,
) -> Tuple[Optional[int], str, Optional[str], Optional[int]]:
    cmd = [engine_path, "perft", str(depth), "--fen", fen]
    try:
        res = subprocess.run(
            cmd, capture_output=True, text=True, check=False, timeout=timeout_sec
        )
    except FileNotFoundError:
        return None, "", f"Engine not found: {engine_path}", 127
    except subprocess.TimeoutExpired:
        return None, "", "Timeout", 124

    if res.returncode != 0:
        return None, res.stdout or "", f"Non-zero exit ({res.returncode}). Stderr: {res.stderr.strip() if res.stderr else ''}", res.returncode

    nodes = parse_engine_perft(res.stdout)
    if nodes is None:
        return None, res.stdout, "Could not parse node count from your engine output.", 0

    return nodes, res.stdout, None, 0


def run_stockfish_perft_once(
    sf_path: str,
    fen: str,
    depth: int,
    timeout_sec: int,
    extra_opts: Optional[Dict[str, str]] = None,
) -> Tuple[Optional[int], str, Optional[str], int]:
    opts = dict(SF_SAFE_OPTIONS)
    if extra_opts:
        opts.update(extra_opts)

    lines = ["uci"]
    for k, v in opts.items():
        lines.append(f"setoption name {k} value {v}")
    lines += [
        "isready",
        "ucinewgame",
        f"position fen {fen}",
        f"go perft {depth}",
        "quit",
        "",
    ]
    script = "\n".join(lines)

    try:
        res = subprocess.run(
            [sf_path],
            input=script,
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout_sec,
        )
    except FileNotFoundError:
        return None, "", f"Stockfish not found: {sf_path}", 127
    except subprocess.TimeoutExpired:
        return None, "", "Timeout", 124

    combined = (res.stdout or "") + "\n" + (res.stderr or "")
    if _invalid_fen_pat.search(combined):
        return None, res.stdout or "", "Stockfish rejected FEN (invalid/illegal).", res.returncode

    if res.returncode != 0:
        parsed = parse_stockfish_perft(res.stdout or "")
        if parsed is not None:
            return parsed, res.stdout, None, res.returncode
        return None, res.stdout or "", f"Non-zero exit ({res.returncode}). Stderr: {res.stderr.strip() if res.stderr else ''}", res.returncode

    nodes = parse_stockfish_perft(res.stdout)
    if nodes is None:
        return None, res.stdout, "Could not parse node count from Stockfish output.", 0

    return nodes, res.stdout, None, 0


def run_stockfish_perft_hardened(
    sf_path: str,
    fen: str,
    depth: int,
    timeout_sec: int,
    verbose: bool = False,
) -> Tuple[Optional[int], str, Optional[str]]:
    nodes, out, err, rc = run_stockfish_perft_once(sf_path, fen, depth, timeout_sec, {})
    if nodes is not None:
        return nodes, out, None
    if rc == 124:
        return None, out, err
    nodes2, out2, err2, _ = run_stockfish_perft_once(sf_path, fen, depth, timeout_sec, {"Hash": "8", "Threads": "1"})
    if nodes2 is not None:
        return nodes2, out2, None
    if verbose and out2:
        return None, out2, err2 or err
    return None, out, err2 or err

BASE_FENS = [
    # Classics
    ("Initial Position", "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1", 5),
    ("Kiwipete", "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1", 4),
    ("Pinned Pieces & Checks", "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1", 5),
    ("Promotions/Checks/Stalemates", "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1", 4),
    ("EP & Checks", "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8", 4),
    ("Many Captures & Checks", "r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10", 4),
    ("EP & Castling (TalkChess)", "r3k2r/1b4b1/8/8/8/8/7P/R3K2R w KQkq - 0 1", 4),
    ("Bare Kings (mate/stalemate variant A)", "8/8/1k6/8/8/8/8/K7 w - - 0 1", 5),
    ("Illegal EP (self-check)", "8/k7/3p4/p2P1p2/P2P1P2/8/8/K7 w - - 0 1", 5),
    ("EP gives Check", "8/5k2/8/2pP4/8/8/8/K7 w - - 0 1", 5),
    ("Castling Through Check (illegal)", "4k3/8/8/8/8/8/r7/R3K2R w KQ - 0 1", 4),

    # Opening-ish
    ("Italian-ish", "r1bqkbnr/pppp1ppp/2n5/4p3/3PP3/5N2/PPP2PPP/RNBQKB1R w KQkq - 2 3", 4),
    ("Giuoco Pianissimo", "r1bqk2r/ppppbppp/2n2n2/4p3/2B1P3/2NP1N2/PPP2PPP/R1BQ1RK1 w kq - 6 6", 4),
    ("QGD-ish", "rnbqk2r/pp1pbppp/2p2n2/3p4/3P4/2N1PN2/PPPQ1PPP/R3KB1R w KQkq - 4 6", 4),
    ("Queen vs castling trap", "r3k2r/pppq1ppp/2npbn2/4p3/2P1P3/1PN2N2/PB1P1PPP/R2QK2R w KQkq - 4 8", 4),
    ("Queen on d5 vs rooks", "r3k2r/ppp2ppp/8/3q4/8/8/PPP2PPP/R3K2R w KQkq - 0 1", 4),

    # Tricky minimal material (VALID only)
    ("K+N vs k", "4k3/8/8/8/8/8/6N1/7K w - - 0 1", 4),
    ("K vs N (other side)", "8/8/8/3N4/8/8/8/K6k w - - 0 1", 4),
]

def toggle_side(fen: str) -> str:
    parts = fen.split()
    parts[1] = "b" if parts[1] == "w" else "w"
    return " ".join(parts)

def strip_castling(fen: str) -> str:
    parts = fen.split()
    parts[2] = "-"
    return " ".join(parts)

def generate_suite(base: List[Tuple[str, str, int]], include_depth3_mirror: bool = True) -> List[Position]:
    positions: List[Position] = []

    for name, fen, maxd in base:
        variants = [
            (f"{name}", fen, maxd),
            (f"{name} (STM toggled)", toggle_side(fen), maxd),
            (f"{name} (no castle)", strip_castling(fen), maxd),
            (f"{name} (STM toggled, no castle)", strip_castling(toggle_side(fen)), maxd),
        ]
        for vn, vf, vd in variants:
            if plausible_fen(vf):
                positions.append(Position(vn, vf, vd))

    if include_depth3_mirror:
        for name, fen, maxd in base:
            c = min(maxd, 3)
            for vn, vf in [
                (f"{name} (d≤3)", fen),
                (f"{name} (STM toggled) (d≤3)", toggle_side(fen)),
                (f"{name} (no castle) (d≤3)", strip_castling(fen)),
                (f"{name} (STM toggled, no castle) (d≤3)", strip_castling(toggle_side(fen))),
            ]:
                if plausible_fen(vf):
                    positions.append(Position(vn, vf, c))

    return positions

def run_suite(
    engine_path: str,
    sf_path: str,
    positions: List[Position],
    depths: List[int],
    timeout_sec: int,
    strict_baseline: bool,
    verbose: bool,
    limit: Optional[int],
) -> None:
    if limit is not None:
        positions = positions[:limit]

    total_cases = sum(len([d for d in depths if d <= p.max_depth]) for p in positions)

    print_header(engine_path, sf_path, len(positions), depths)
    print(f"{Colors.DIM} Total cases : {total_cases}{Colors.END}\n")

    passed = failed = skipped = 0

    for i, pos in enumerate(positions, 1):
        print(f"{Colors.CYAN}Testing Position {i}/{len(positions)}: {pos.name}{Colors.END}")
        for depth in depths:
            if depth > pos.max_depth:
                continue

            test_name = f'FEN: "{pos.fen}" at Depth: {depth}'

            your_nodes, your_out, your_err, your_rc = run_engine_perft(engine_path, pos.fen, depth, timeout_sec)
            if your_nodes is None:
                print(f" {Colors.BOLD}{Colors.RED}FAILED{Colors.END}: {test_name}")
                if your_err:
                    print(f"  └─ Reason: Could not parse your engine. {your_err}")
                else:
                    print(f"  └─ Reason: Your engine failed.")
                if verbose and (your_out or your_err):
                    if your_out:
                        print(f"  └─ Your engine output (tail):\n{tail(your_out)}")
                    if your_err and your_rc not in (0, None):
                        print(f"  └─ Your engine stderr:\n{your_err}")
                failed += 1
                continue

            sf_nodes, sf_out, sf_err = run_stockfish_perft_hardened(sf_path, pos.fen, depth, timeout_sec, verbose=verbose)

            if sf_nodes is None:
                if strict_baseline:
                    print(f" {Colors.BOLD}{Colors.RED}FAILED{Colors.END}: {test_name}")
                    print(f"  └─ Reason: Stockfish baseline unavailable. {sf_err or ''}".rstrip())
                    if verbose and (sf_out or sf_err):
                        print(f"  └─ Stockfish output (tail):\n{tail(sf_out)}")
                    failed += 1
                else:
                    print(f" {Colors.BOLD}{Colors.YELLOW}SKIPPED{Colors.END}: {test_name}")
                    print(f"  └─ Reason: Stockfish baseline unavailable. {sf_err or ''}".rstrip())
                    if verbose and (sf_out or sf_err):
                        print(f"  └─ Stockfish output (tail):\n{tail(sf_out)}")
                    skipped += 1
                continue

            if your_nodes == sf_nodes:
                print(f" {Colors.BOLD}{Colors.GREEN}PASSED{Colors.END}: {test_name} -> {fmt_int(your_nodes)}")
                passed += 1
            else:
                print(f" {Colors.BOLD}{Colors.RED}FAILED{Colors.END}: {test_name}")
                print(f"  └─ Expected (SF): {fmt_int(sf_nodes)}, Got: {fmt_int(your_nodes)}")
                if verbose:
                    if your_out:
                        print(f"  └─ Your engine output (tail):\n{tail(your_out)}")
                    if sf_out:
                        print(f"  └─ Stockfish output (tail):\n{tail(sf_out)}")
                failed += 1

        print()

    print("\n" + "=" * 56)
    print("Suite Finished")
    print(f" Cases   : {total_cases}")
    ps = f"{Colors.BOLD}{Colors.GREEN}{passed} PASSED{Colors.END}"
    fs = f"{Colors.BOLD}{Colors.RED}{failed} FAILED{Colors.END}"
    if skipped:
        sk = f"{Colors.BOLD}{Colors.YELLOW}{skipped} SKIPPED{Colors.END}"
        print(f" {ps}\n {fs}\n {sk}")
    else:
        print(f" {ps}\n {fs}")
    print("=" * 56 + "\n")

    if failed > 0:
        sys.exit(1)

def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="Compare your engine's perft with Stockfish perft (as baseline).",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    p.add_argument("--engine", default=os.environ.get("ENGINE_PATH", DEFAULT_ENGINE),
                   help="Path to your engine executable")
    p.add_argument("--stockfish", default=os.environ.get("STOCKFISH_PATH", DEFAULT_STOCKFISH),
                   help="Path to Stockfish executable")
    p.add_argument("--timeout", type=int, default=int(os.environ.get("PERFT_TIMEOUT", DEFAULT_TIMEOUT)),
                   help="Timeout per perft call (seconds)")
    p.add_argument("--depths", default=",".join(str(d) for d in DEFAULT_DEPTHS),
                   help="Comma-separated list of depths to test, e.g. 1,2,3,4")
    p.add_argument("--strict-baseline", action="store_true",
                   help="If set, a failing Stockfish perft counts as a test failure. "
                        "By default, such cases are marked SKIPPED.")
    p.add_argument("--no-mirrors", action="store_true",
                   help="Do not include the extra '(d≤3)' variants.")
    p.add_argument("--limit", type=int, default=None,
                   help="Limit number of positions (for quick runs).")
    p.add_argument("--verbose", action="store_true",
                   help="Show tail of engine and Stockfish outputs on failures.")
    return p.parse_args()

def main():
    args = parse_args()
    try:
        depths = [int(x) for x in args.depths.split(",") if x.strip()]
    except ValueError:
        print("Error: --depths must be a comma-separated list of integers, e.g. 1,2,3,4")
        sys.exit(2)

    positions = generate_suite(BASE_FENS, include_depth3_mirror=not args.no_mirrors)

    run_suite(
        engine_path=args.engine,
        sf_path=args.stockfish,
        positions=positions,
        depths=depths,
        timeout_sec=args.timeout,
        strict_baseline=args.strict_baseline,
        verbose=args.verbose,
        limit=args.limit,
    )

if __name__ == "__main__":
    main()

