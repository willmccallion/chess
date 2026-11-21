use crate::board::Board;
use crate::polyglot_zobrist;
use crate::types::{Move, MoveList, Piece, PieceKind, Square};
use byteorder::{BigEndian, ReadBytesExt};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

// A simple random number generator
struct Rng(u64);
impl Rng {
    const fn new() -> Self {
        Self(0x1234_5678_9ABC_DEF0)
    }
    const fn rand(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

// Use a thread-safe Mutex for the global RNG
static BOOK_RNG: OnceLock<Mutex<Rng>> = OnceLock::new();
fn get_rng() -> &'static Mutex<Rng> {
    BOOK_RNG.get_or_init(|| Mutex::new(Rng::new()))
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BookEntry {
    pub key: u64,
    pub raw_move: u16,
    pub weight: u16,
}

impl BookEntry {
    /// The legal move in `b` this entry names, if `b` has one: Polyglot
    /// writes castling as the king moving onto its own rook.
    fn legal_move(self, b: &Board, legal: &[Move]) -> Option<Move> {
        let from = Square::new(((self.raw_move >> 6) & 0x3F) as u8);
        let to = Square::new((self.raw_move & 0x3F) as u8);
        let promotion = match (self.raw_move >> 12) & 0x7 {
            1 => Some(PieceKind::Knight),
            2 => Some(PieceKind::Bishop),
            3 => Some(PieceKind::Rook),
            4 => Some(PieceKind::Queen),
            _ => None,
        };
        let castles = b.piece_on()[from.index()] == Piece::from_kind(PieceKind::King, b.turn())
            && b.piece_on()[to.index()] == Piece::from_kind(PieceKind::Rook, b.turn());
        let to = match (castles, to > from) {
            (false, _) => Some(to),
            (true, true) => from.offset(2),
            (true, false) => from.offset(-2),
        }?;
        legal
            .iter()
            .find(|m| m.from == from && m.to == to && m.promotion() == promotion)
            .copied()
    }
}

pub struct OpeningBook {
    entries: Vec<BookEntry>,
}

impl OpeningBook {
    fn new(path: &Path) -> Result<Self, std::io::Error> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut buffer = [0; 16];

        while matches!(reader.read_exact(&mut buffer), Ok(())) {
            let key = (&buffer[0..8]).read_u64::<BigEndian>()?;
            let raw_move = (&buffer[8..10]).read_u16::<BigEndian>()?;
            let weight = (&buffer[10..12]).read_u16::<BigEndian>()?;
            // The last four bytes, Polyglot's learn field, are not used.
            entries.push(BookEntry {
                key,
                raw_move,
                weight,
            });
        }

        Ok(Self { entries })
    }

    fn find_entries(&self, key: u64) -> &[BookEntry] {
        match self.entries.binary_search_by_key(&key, |e| e.key) {
            Ok(mut index) => {
                while index > 0 && self.entries[index - 1].key == key {
                    index -= 1;
                }
                let start = index;
                while index < self.entries.len() && self.entries[index].key == key {
                    index += 1;
                }
                &self.entries[start..index]
            }
            Err(_) => &[],
        }
    }
}

/// The book and the file it came from, or every path that was tried.
type LoadedBook = Result<(OpeningBook, PathBuf), Vec<PathBuf>>;

static BOOK: OnceLock<LoadedBook> = OnceLock::new();
static LOAD_REPORTED: OnceLock<()> = OnceLock::new();

const BOOK_FILENAME: &str = "moves/book.bin";

/// Where the book is looked for, in order: next to the executable, in the
/// working directory, and at the project root of an executable under `target/`.
fn book_paths(exe: Option<&Path>, cwd: Option<&Path>) -> Vec<PathBuf> {
    let exe_dir = exe.and_then(Path::parent);
    let project_root = exe
        .and_then(|e| e.ancestors().find(|a| a.ends_with("target")))
        .and_then(Path::parent);
    [exe_dir, cwd, project_root]
        .into_iter()
        .flatten()
        .map(|dir| dir.join(BOOK_FILENAME))
        .collect()
}

/// The first of `paths` that holds a readable book.
fn load_first_book(paths: Vec<PathBuf>) -> LoadedBook {
    for path in &paths {
        if let Ok(book) = OpeningBook::new(path) {
            return Ok((book, path.clone()));
        }
    }
    Err(paths)
}

fn load_book() -> &'static LoadedBook {
    BOOK.get_or_init(|| {
        let exe = std::env::current_exe().ok();
        let cwd = std::env::current_dir().ok();
        load_first_book(book_paths(exe.as_deref(), cwd.as_deref()))
    })
}

/// The book for UCI play, announcing on stdout once where it was loaded from.
fn get_book() -> Option<&'static OpeningBook> {
    let loaded = load_book();
    LOAD_REPORTED.get_or_init(|| match loaded {
        Ok((_, path)) => println!("info string Loaded opening book from: {}", path.display()),
        Err(_) => println!(
            "info string Opening book 'moves/book.bin' not found in any standard location."
        ),
    });
    loaded.as_ref().ok().map(|(book, _)| book)
}

/// A book move with its relative weight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeightedMove {
    pub m: Move,
    pub weight: u16,
}

/// The book's legal moves for `b` with their weights, without printing
/// anything; empty when there is no book.
pub fn legal_book_moves(b: &Board) -> Vec<WeightedMove> {
    load_book()
        .as_ref()
        .map_or_else(|_| Vec::new(), |(book, _)| legal_moves_in(book, b))
}

fn legal_moves_in(book: &OpeningBook, b: &Board) -> Vec<WeightedMove> {
    let mut legal = MoveList::new();
    b.generate_legal_moves(&mut legal);
    let key = polyglot_zobrist::calculate_key(b);
    book.find_entries(key)
        .iter()
        .filter_map(|e| {
            let m = e.legal_move(b, legal.as_slice())?;
            Some(WeightedMove {
                m,
                weight: e.weight,
            })
        })
        .collect()
}

/// A book move for UCI play, chosen by weight among the legal ones.
pub fn get_book_move(b: &Board) -> Option<Move> {
    let moves = legal_moves_in(get_book()?, b);
    let total_weight: u32 = moves.iter().map(|w| u32::from(w.weight)).sum();
    let chosen = if total_weight == 0 {
        moves.first()?
    } else {
        let roll = get_rng()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .rand() as u32;
        pick_by_weight(&moves, roll % total_weight)?
    };
    println!("info string Playing book move.");
    Some(chosen.m)
}

/// The move whose share of the summed weights covers `roll`.
fn pick_by_weight(moves: &[WeightedMove], mut roll: u32) -> Option<&WeightedMove> {
    for w in moves {
        let weight = u32::from(w.weight);
        if roll < weight {
            return Some(w);
        }
        roll -= weight;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uci_io::format_uci;

    fn decoded(fen: &str, from: u16, to: u16) -> Option<String> {
        let board = Board::from_fen(fen).unwrap();
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);
        let entry = BookEntry {
            key: 0,
            raw_move: (from << 6) | to,
            weight: 1,
        };
        entry.legal_move(&board, legal.as_slice()).map(format_uci)
    }

    #[test]
    fn a_rook_move_from_e1_to_h1_stays_a_rook_move() {
        assert_eq!(
            decoded("7k/8/8/8/8/8/K7/4R3 w - - 0 1", 4, 7),
            Some("e1h1".into())
        );
    }

    #[test]
    fn a_king_onto_its_own_rook_is_castling() {
        let castled = decoded("4k3/8/8/8/8/8/8/4K2R w K - 0 1", 4, 7);

        assert_eq!(castled, Some("e1g1".into()));
    }

    #[test]
    fn an_illegal_book_move_decodes_to_none() {
        let fen = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

        assert_eq!(decoded(fen, 8, 32), None);
    }

    #[test]
    fn the_book_is_looked_for_next_to_the_executable_then_in_the_cwd_then_at_the_project_root() {
        let exe = Path::new("/build/chess/target/release/chess");

        let paths = book_paths(Some(exe), Some(Path::new("/games")));

        assert_eq!(
            paths,
            [
                "/build/chess/target/release/moves/book.bin",
                "/games/moves/book.bin",
                "/build/chess/moves/book.bin",
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn the_first_book_found_in_lookup_order_is_loaded() {
        let dir = std::env::temp_dir().join(format!("chess-book-test-{}", std::process::id()));
        let root = dir.join("project");
        let cwd = dir.join("cwd");
        for book_dir in [&root, &cwd] {
            std::fs::create_dir_all(book_dir.join("moves")).unwrap();
            std::fs::write(book_dir.join(BOOK_FILENAME), []).unwrap();
        }
        let paths = book_paths(Some(&root.join("target/release/chess")), Some(&cwd));

        let first = load_first_book(paths.clone()).map(|(_, path)| path);
        std::fs::remove_file(cwd.join(BOOK_FILENAME)).unwrap();
        let without_cwd_book = load_first_book(paths).map(|(_, path)| path);
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(first, Ok(cwd.join(BOOK_FILENAME)));
        assert_eq!(without_cwd_book, Ok(root.join(BOOK_FILENAME)));
    }

    #[test]
    fn the_roll_picks_the_move_whose_weight_covers_it() {
        let moves = [1, 3].map(|weight| WeightedMove {
            m: Move::quiet(Square::new(12), Square::new(28)),
            weight,
        });

        let picked = [0, 1, 3].map(|roll| pick_by_weight(&moves, roll).map(|w| w.weight));

        assert_eq!(picked, [Some(1), Some(3), Some(3)]);
    }
}
