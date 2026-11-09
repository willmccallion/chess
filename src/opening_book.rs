use crate::board::Board;
use crate::polyglot_zobrist;
use crate::types::{Move, MoveList, Piece, PieceKind, Square};
use byteorder::{BigEndian, ReadBytesExt};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};

// A simple random number generator
struct Rng(u64);
impl Rng {
    fn new() -> Self {
        Self(0x1234_5678_9ABC_DEF0)
    }
    fn rand(&mut self) -> u64 {
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
    pub _learn: u32,
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
        let castles = b.piece_on[from.index()] == Piece::from_kind(PieceKind::King, b.turn)
            && b.piece_on[to.index()] == Piece::from_kind(PieceKind::Rook, b.turn);
        let to = match (castles, to > from) {
            (false, _) => Some(to),
            (true, true) => from.offset(2),
            (true, false) => from.offset(-2),
        }?;
        legal
            .iter()
            .find(|m| m.from == from && m.to == to && m.promotion == promotion)
            .copied()
    }
}

pub struct OpeningBook {
    entries: Vec<BookEntry>,
}

impl OpeningBook {
    fn new(path: &str) -> Result<Self, std::io::Error> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut buffer = [0; 16];

        while let Ok(()) = reader.read_exact(&mut buffer) {
            let key = (&buffer[0..8]).read_u64::<BigEndian>()?;
            let raw_move = (&buffer[8..10]).read_u16::<BigEndian>()?;
            let weight = (&buffer[10..12]).read_u16::<BigEndian>()?;
            let learn = (&buffer[12..16]).read_u32::<BigEndian>()?;

            entries.push(BookEntry {
                key,
                raw_move,
                weight,
                _learn: learn,
            });
        }

        Ok(OpeningBook { entries })
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

fn load_book() -> &'static LoadedBook {
    BOOK.get_or_init(|| {
        let book_filename = "moves/book.bin";
        let mut potential_paths: Vec<PathBuf> = Vec::new();

        if let Ok(mut exe_path) = std::env::current_exe() {
            exe_path.pop(); // Remove the executable name to get the directory
            potential_paths.push(exe_path.join(book_filename));
        }

        if let Ok(cwd) = std::env::current_dir() {
            potential_paths.push(cwd.join(book_filename));
        }

        potential_paths.push(PathBuf::from("/home/will/projects/chess/moves/book.bin"));

        if let Ok(exe_path) = std::env::current_exe()
            && exe_path.to_string_lossy().contains("target")
            && let Some(target_pos) = exe_path.to_string_lossy().find("target")
        {
            let project_root = PathBuf::from(&exe_path.to_string_lossy()[..target_pos]);
            potential_paths.push(project_root.join(book_filename));
        }

        for path in &potential_paths {
            if let Some(book) = path.to_str().and_then(|p| OpeningBook::new(p).ok())
                && path.exists()
            {
                return Ok((book, path.clone()));
            }
        }
        Err(potential_paths)
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
            _learn: 0,
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
    fn the_roll_picks_the_move_whose_weight_covers_it() {
        let moves = [1, 3].map(|weight| WeightedMove {
            m: Move::quiet(Square::new(12), Square::new(28)),
            weight,
        });

        let picked = [0, 1, 3].map(|roll| pick_by_weight(&moves, roll).map(|w| w.weight));

        assert_eq!(picked, [Some(1), Some(3), Some(3)]);
    }
}
