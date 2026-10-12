# Chess Engine in Rust

This is a chess engine written in Rust. It’s designed to be simple, correct, and reasonably strong, while still being easy to experiment with and extend.

You can interact with the engine in several ways:

- Play a game against it directly in your terminal.
- Let it play games against itself.
- Connect it to a graphical chess program (Arena, Scid vs. PC, Cute Chess, etc.) and use a visual board via UCI.

---

## Features

- **Standard Chess Rules**  
  Implements all standard rules, including castling, pawn promotion, en passant, threefold repetition, and the fifty-move rule.

- **Smart Move Search**  
  Uses alpha–beta search (with typical pruning and move-ordering techniques) to look ahead and find strong moves.

- **Opening Book**  
  Uses a Polyglot opening book to play many common openings confidently from move one.

- **Move Generation Testing (`perft`)**  
  Includes a `perft` tool to verify the correctness of move generation by counting all legal move sequences to a given depth.

- **UCI Protocol Support**  
  Speaks the Universal Chess Interface (UCI) protocol, so it works with most modern chess GUIs.

- **NNUE Evaluation**  
  Evaluates positions with Stockfish 18's networks (see **NNUE** below).

---

## Building

The repository ships a Nix flake. With [direnv](https://direnv.net/) and nix-direnv, run `direnv allow` once; otherwise use `nix develop`. The dev shell provides:

- the stable Rust toolchain pinned in `rust-toolchain.toml` (with clippy, rustfmt, rust-analyzer),
- the Stockfish 18 networks (`$CHESS_NNUE_BIG`, `$CHESS_NNUE_SMALL`), whose paths are baked into the binary at build time,
- Stockfish 18 (`$STOCKFISH`) for matches and comparisons,
- opening suites: `$CHESS_BOOK_UHO` (UHO Lichess, for engine-vs-engine testing) and `$CHESS_BOOK_BALANCED` (noob 3-moves),
- the Baron Polyglot book, linked to `moves/book.bin`,
- `perf` and Python with `python-chess` for the scripts in `tools/`.

Build in release mode:

```bash
cargo build --release
```

The executable is `target/release/chess`. A `profiling` profile (release plus debug info) is available for `perf`:

```bash
cargo build --profile profiling
perf record -g target/profiling/chess bench 16
```

---

## How to Use

All examples below assume you’re running commands from the project root.

### Playing a Game in the Terminal

To play against the engine, use the `play-cli` command and specify how much time (in milliseconds) the engine may think for each move:

```bash
# Start a game where the engine thinks for up to 5 seconds per move
cargo run --release -- play-cli --time 5000
```

Enter moves in UCI-style coordinate notation, for example: `e2e4`, `g1f3`, etc.

---

### Watching the Engine Play Itself

You can have the engine play a series of games against itself, which is useful for testing changes or just watching it in action:

```bash
# The engine will play 5 games against itself, thinking 1 second per move
cargo run --release -- self-play --rounds 5 --time 1000
```

---

### Using the Engine with a Chess GUI (UCI)

To use this engine with a graphical interface, start it in `uci` mode:

```bash
cargo run --release -- uci
```

Then, in your favorite chess GUI:

1. Open the engine management/settings dialog.
2. Add a **new UCI engine**.
3. When prompted for the engine executable, point to the binary (for example, `target/release/chess`).
4. Save the configuration and start a game using this engine.

---

### Testing Move Generation (`perft`)

The `perft` command is a debugging tool that counts all possible legal move sequences up to a given depth. This is an effective way to verify that the engine’s understanding of the rules is correct.

From the standard starting position:

```bash
# Calculate all possible legal moves up to a depth of 5 from the starting position
cargo run --release -- perft 5
```

From a custom FEN position:

```bash
cargo run --release -- perft 4 --fen "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1"
```

---

### Benchmarking

`bench` searches 30 fixed positions to a fixed depth and prints the total node count and speed. The node count is a signature of the search: a refactor that should not change behavior must leave it unchanged.

```bash
cargo run --release -- bench          # depth 13, 16 MB hash
cargo run --release -- bench 16 --hash 64
```

### Matches and SPRT testing

`challenge` plays two UCI engines against each other. With `--openings`, each sampled opening is played twice with colors swapped, results are reported as pentanomial pair statistics, and `--sprt` stops as soon as the test is decided.

```bash
# New build vs a saved older build, stop when +0..+5 Elo is decided
target/release/chess challenge target/release/chess versions/old \
  --e1-option OwnBook=false --e2-option OwnBook=false \
  --time 10000 --inc 100 --rounds 20000 --concurrency 8 \
  --openings "$CHESS_BOOK_UHO" --sprt 0,5

# Against Stockfish at a limited strength
target/release/chess challenge target/release/chess "$STOCKFISH" \
  --e1-option OwnBook=false --e2-option UCI_LimitStrength=true --e2-option UCI_Elo=3000 \
  --time 60000 --inc 600 --rounds 100 --concurrency 8 --openings "$CHESS_BOOK_BALANCED"
```

### Tuning search parameters (SPSA)

The search margins and reductions are runtime parameters (`chess params` lists them with their defaults, ranges and steps; UCI `setoption` accepts them by name). `chess tune` adjusts them with SPSA: each game pair perturbs every parameter up or down, plays the two versions against each other with colors swapped, and moves the estimate toward the winner. The current estimate is written to `--output` after every 20 pairs and can be continued with `--resume`.

```bash
taskset -c 0-15 chess tune --openings "$CHESS_BOOK_UHO" --pairs 10000 --time 5000 --inc 50 --concurrency 8 --output spsa.txt
```

Pin the run to one core type: the two engines of a pair should run at the same speed.

### Comparing analysis with Stockfish

`compare` analyses positions with this engine and a reference engine (Stockfish by default) and reports both scores, best moves, the score gap and static evaluations, plus a summary over all positions.

```bash
cargo run --release -- compare --fen "r1bq1rk1/ppp1nppp/4n3/3p3Q/3P4/1BP1B3/PP1N2PP/R4RK1 w - - 1 16"
cargo run --release -- compare --epd "$CHESS_BOOK_BALANCED" --count 50 --depth 14
```

### Perft against Stockfish

`tools/perft_test.py` compares `perft --divide` with Stockfish's `go perft` on a suite of tricky positions:

```bash
python3 tools/perft_test.py --stockfish "$STOCKFISH"
```

---

## NNUE

The evaluation is a port of Stockfish 18's: the big network `nn-c288c895ea92.nnue` (HalfKAv2_hm piece-square plus FullThreats attacker–target inputs, 1024 wide, 8 layer stacks chosen by piece count) and the small network `nn-37f18f62d772.nnue` (piece-square only, 128 wide), combined exactly as Stockfish does: the small net for lopsided material with a big-net recheck near equality, complexity damping, material scaling and 50-move damping. Scores are converted to centipawns with Stockfish's WDL normalisation.

The networks are read at runtime. The default paths come from the dev shell at build time; the UCI options `EvalFile` and `EvalFileSmall` override them before the first `isready`.

Accumulators are updated lazily and incrementally: each move records its piece changes and the attacker–target relations it creates and destroys, king moves refresh from a per-king-square cache, and the dense layers use AVX2 (AVX-VNNI when available) with a sparse first layer.

`chess eval-trace <fen>` prints the same per-bucket table and final evaluation as Stockfish's `eval` command, and `tools/nnue_check.py` diffs the two over random playout positions:

```bash
python3 tools/nnue_check.py --stockfish "$STOCKFISH" --epd "$CHESS_BOOK_UHO" --count 3000
```

---

## Acknowledgements

* **Opening Book**
  The opening book data is sourced from *The Baron's Polyglot Opening Book*, available via the
  [Chess Programming Wiki](https://www.chessprogramming.net/new-version-of-the-baron-v3-43-plus-the-barons-polyglot-opening-book/).

* **Stockfish**
  The networks are Stockfish's, and the network format, input features and evaluation
  follow the [Stockfish](https://github.com/official-stockfish/Stockfish) 18 source (GPLv3).
