# Chess Engine in Rust

This is a chess engine written in Rust: a multithreaded alpha–beta search with a port of Stockfish 18's NNUE evaluation, a network of its own trained from self-play, and a coach for learning to play.

You can interact with the engine in several ways:

- Play it in your terminal with the mouse, with its evaluation shown or hidden.
- Watch it play itself.
- Connect it to a graphical chess program (Arena, Scid vs. PC, Cute Chess, etc.) and use a visual board via UCI.

---

## Features

- **Standard Chess Rules**  
  Implements all standard rules, including castling, pawn promotion, en passant, and draws by stalemate, threefold repetition, the fifty-move rule and insufficient material.

- **Search**  
  Principal variation search on up to 128 threads (see **Search** below).

- **Opening Book**  
  Over UCI, and for the coach's bot, the engine plays from a Polyglot book while it has moves for the position. The book is optional: it is looked up as `moves/book.bin` next to the executable, in the current directory and at the project root, and the UCI option `OwnBook=false` turns it off.

- **Move Generation Testing (`perft`)**  
  Includes a `perft` tool to verify the correctness of move generation by counting all legal move sequences to a given depth.

- **UCI Protocol Support**  
  Speaks the Universal Chess Interface (UCI) protocol, so it works with most modern chess GUIs.

- **NNUE Evaluation**  
  Evaluates positions with Stockfish 18's networks by default, or with the engine's own network or a small hand-written evaluation (see **NNUE** below).

- **Coach**  
  A terminal mode that reviews your games and sets puzzles, opening drills and endgames.

- **Testing and Tuning Tools**  
  Engine matches with SPRT, SPSA tuning of the search parameters, analysis compared with Stockfish, and self-play data generation for training the engine's own network.

---

## Strength

Roughly **3650–3750 Elo on the CCRL Blitz scale** (single thread). This is an estimate from local 200-game matches at 60+0.6, not an official rating:

| Opponent (CCRL Blitz) | Result | Implied rating |
| --- | --- | --- |
| Carp 3.0.1 (3526) | 143–57, +160 ± 30 | ≈3690 |
| Sirius 9.0 (3529) | 148.5–51.5, +184 ± 32 | ≈3710 |
| Stockfish 18, 1 thread (3772) | 76.5–123.5, −83 ± 21 | ≈3690 |

---

## Search

- Principal variation search with iterative deepening and aspiration windows.
- Lazy SMP on up to 128 threads (UCI `Threads`), sharing a lock-free transposition table with entry aging.
- Reverse futility pruning, razoring, null-move pruning and ProbCut before the moves are searched; late-move, futility, SEE and history pruning while they are.
- Singular extensions with double extensions and multi-cut, internal iterative reductions, and late-move reductions from a table adjusted by history.
- Quiescence search over captures with delta pruning, and over every evasion when in check.
- Move ordering by killers, counter-moves, butterfly, one- and two-ply continuation and capture histories; a pawn-structure correction history adjusts the static evaluation.
- Time management that spends less when one move takes most of the nodes and more when the score falls; pondering and `go searchmoves`.
- 49 parameters tuned with SPSA (see **Tuning search parameters** below).

---

## Building

The repository ships a Nix flake. With [direnv](https://direnv.net/) and nix-direnv, run `direnv allow` once; otherwise use `nix develop`. The dev shell provides:

- the stable Rust toolchain pinned in `rust-toolchain.toml` (with clippy, rustfmt, rust-analyzer),
- the Stockfish 18 networks (`$CHESS_NNUE_BIG`, `$CHESS_NNUE_SMALL`), whose paths are baked into the binary at build time,
- Stockfish 18 (`$STOCKFISH`) for matches and comparisons,
- opening suites: `$CHESS_BOOK_UHO` (UHO Lichess, for engine-vs-engine testing) and `$CHESS_BOOK_BALANCED` (noob 3-moves),
- the Baron Polyglot book, linked to `moves/book.bin`,
- `perf`, and Python with `python-chess` for `tools/perft_test.py` and `tools/nnue_check.py`.

Build in release mode, inside the dev shell (other Rust toolchains can fail to compile the AVX-VNNI code):

```bash
cargo build --release
```

The executable is `target/release/chess`. `.cargo/config.toml` builds with `-C target-cpu=native`, so the binary is for the CPU it was built on. A `profiling` profile (release plus debug info) is available for `perf`:

```bash
cargo build --profile profiling
perf record -g target/profiling/chess bench 16
```

Run the tests and lints with:

```bash
cargo test --release
cargo clippy --all-targets
```

Clippy's `pedantic` and `nursery` groups are set to deny in `Cargo.toml`.

---

## How to Use

All examples below assume you’re running commands from the project root. `chess <command> --help` lists every option of a command.

### Playing in the Terminal

`play` opens a board in the terminal. Click or drag pieces with the mouse, or press Enter and type a move (`e2e4`, `Nf3`, `O-O`). To castle, move the king two squares or onto its rook.

```bash
# Casual: the engine's evaluation, analysis of your position and takebacks are available
cargo run --release -- play

# A real game: no engine output and no takebacks until it ends, 5 minutes + 3 seconds each
cargo run --release -- play --mode game --clock 5+3 --side black

# Watch the engine play itself
cargo run --release -- play --mode watch --movetime 1000
```

Without `--clock` the engine thinks for `--movetime` milliseconds per move (default 3000). `--threads` and `--hash` set the engine's resources, and `--fen` starts from another position. Press `?` for the keys. You can hide the engine panel (`e`) and the move list (`l`), turn analysis on your turn on or off (`a`), flip the board (`f`), and switch the pieces between pictures, symbols and letters (`p`). On quitting, the game's moves and final position are printed.

---

### Learning with the coach

`play --mode learn` opens a coach that teaches openings, sets puzzles and endgame drills, plays you at your level and reviews your games:

```bash
cargo run --release -- play --mode learn
```

- **Play**: a game against the coach's bot, at the level that matches your rating or any of ten levels. The bot plays human-like moves, mistakes included. The optional blunder check stops you before a move that gives away material or allows mate, and says why. You can also have the bot steer into a chapter of your repertoire.
- **Review**: after each game the engine goes through every move, marks inaccuracies (`?!`), mistakes (`?`) and blunders (`??`), and explains each one: a piece left hanging, a threat ignored, a fork walked into, a mate or free material missed. It shows what happens after the move and what the better move leads to, with the material each line wins or loses, and how the positions differ (castling, pawn cover, pawn weaknesses, development, room for the pieces). Press `t` or `b` to play either line through on the board.
- **Puzzles**: every mistake and blunder from your games becomes a puzzle, and comes back on a spaced-repetition schedule until you get it right. Puzzles from the Lichess database fill the gaps, picked near your puzzle rating and weighted towards the themes behind your mistakes.
- **Openings**: a repertoire built on 1.d4 and 2.c4 as White, and 1...e5 and 1...d5 as Black, with a note on every move. A lesson walks through a line; a drill has you play your side from memory, and lines come back for review on a schedule.
- **Endgames**: the basic mates, king-and-pawn endings, and the Lucena and Philidor positions, against the engine's best defence.
- **Progress**: your rating and puzzle rating, accuracy by phase of the game, mistakes per game by cause with trends, opening habits, and what to work on.

Your games and practice are kept in `~/.local/share/chess/coach` (or `$XDG_DATA_HOME/chess/coach`); `--coach-dir` uses another directory. To add Lichess puzzles, download `lichess_db_puzzle.csv.zst` from [database.lichess.org](https://database.lichess.org/#puzzles) and import it once:

```bash
zstd -dc lichess_db_puzzle.csv.zst | cargo run --release -- import-puzzles -
```

This keeps a sample of about 50,000 popular puzzles rated 400 to 2400.

---

### Using the Engine with a Chess GUI (UCI)

To use this engine with a graphical interface, start it in `uci` mode (it is also the mode when no command is given):

```bash
cargo run --release -- uci
```

Then, in your favorite chess GUI:

1. Open the engine management/settings dialog.
2. Add a **new UCI engine**.
3. When prompted for the engine executable, point to the binary (for example, `target/release/chess`).
4. Save the configuration and start a game using this engine.

The engine's UCI options:

| Option | Default | Meaning |
| --- | --- | --- |
| `Hash` | 256 | Transposition table size in MB (1 to 4096) |
| `Threads` | 1 | Search threads (1 to 128) |
| `Ponder` | false | Tells the GUI it may ponder; `go ponder` and `ponderhit` work either way |
| `OwnBook` | true | Play from the opening book while it has a move |
| `CoachLevel` | 0 | 1 to 10 plays like one of the coach's levels in learn mode instead of at full strength; 0 is off |
| `Move Overhead` | 10 | Milliseconds kept back per move for GUI and network delay (0 to 5000) |
| `Evaluator`, `EvalFile`, `EvalFileSmall`, `OwnEvalFile` | | See **NNUE** below |

The search parameters listed by `chess params` are also accepted by name. The non-standard `eval` command prints the static evaluation of the current position, as does `chess eval <fen>` from the command line.

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

`challenge` plays two UCI engines against each other. With `--openings`, each sampled opening is played twice with colors swapped, results are reported as pentanomial pair statistics, and `--sprt` stops as soon as the test is decided. `--e1-name` and `--e2-name` label the engines in the output, which otherwise shows their file names.

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

### Choosing the evaluator

`--eval stockfish` (the default) uses the networks above; `--eval bootstrap` uses a small hand-written evaluation that plays the first games of our own network's training data; `--eval own` uses our own network (below). All three are also available through the UCI option `Evaluator`. Building with `--no-default-features` leaves the Stockfish code out entirely.

`chess eval-trace <fen>` prints the same per-bucket table and final evaluation as Stockfish's `eval` command, and `tools/nnue_check.py` diffs the two over random playout positions:

```bash
python3 tools/nnue_check.py --stockfish "$STOCKFISH" --epd "$CHESS_BOOK_UHO" --count 3000
```

### Our own network

`--eval own` evaluates with a network trained from the engine's own self-play games, with nothing taken from Stockfish: a `(768 → 128)×2 → 1` perspective network (piece-square inputs, SCReLU) in the quantised format [bullet](https://github.com/jw1912/bullet) saves. The architecture and quantisation constants live in `src/eval/own/network.rs`, and the trainer reads them from there. The network file is read at runtime: `--own-net <file>` on the command line, the UCI option `OwnEvalFile`, or `$CHESS_NNUE_OWN` set at build time. Networks are kept out of git.

Training happens in generations. Generation 0's games are played by the bootstrap evaluation; each later generation's games are played by the previous network. The trainer is a separate Cargo project in `trainer/` with its own dev shell, which has CUDA and `bullet-utils` (`nix develop .#trainer`, or `direnv allow` in `trainer/`). Both pin the same bullet revision.

```bash
# 1. Self-play positions as `<FEN> | <score> | <result>` lines (about 9M an hour on 28 threads)
target/release/chess --eval bootstrap datagen --out data/gen0.txt --positions 30000000

# 2. In the trainer shell: convert to bulletformat and shuffle
bullet-utils convert --from text --input data/gen0.txt --output data/gen0.data
bullet-utils shuffle --input data/gen0.data --output data/gen0-shuffled.data --mem-used-mb 16000

# 3. Train; checkpoints go to trainer/checkpoints/<net-id>-<superbatch>/quantised.bin
cd trainer && cargo run --release -- ../data/gen0-shuffled.data --net-id gen0

# 4. Test the network against the previous generation's evaluator
target/release/chess challenge target/release/chess target/release/chess \
  --e1-name gen0 --e2-name bootstrap \
  --e1-option OwnBook=false --e1-option Evaluator=own \
  --e1-option OwnEvalFile=trainer/checkpoints/gen0-40/quantised.bin \
  --e2-option OwnBook=false --e2-option Evaluator=bootstrap \
  --time 10000 --inc 100 --rounds 2000 --concurrency 8 --openings "$CHESS_BOOK_UHO"

# 5. The next generation's games, played by the new network
target/release/chess --eval own --own-net trainer/checkpoints/gen0-40/quantised.bin \
  datagen --out data/gen1.txt --positions 50000000
```

`bullet-utils interleave` merges shuffled files, so a generation can also train on earlier generations' data.

---

## Acknowledgements

* **Opening Book**
  The opening book data is sourced from *The Baron's Polyglot Opening Book*, available via the
  [Chess Programming Wiki](https://www.chessprogramming.net/new-version-of-the-baron-v3-43-plus-the-barons-polyglot-opening-book/).

* **Stockfish**
  The networks are Stockfish's, and the network format, input features and evaluation
  follow the [Stockfish](https://github.com/official-stockfish/Stockfish) 18 source (GPLv3).

## License

This program is free software, released under the GNU General Public License version 3 or (at your option) any later version. See [LICENSE](LICENSE).
