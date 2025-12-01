use crate::board::Board;
use crate::polyglot_zobrist;
use crate::types::Move;
use crate::uci_io::format_uci;
use byteorder::{BigEndian, ReadBytesExt};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

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
    // Polyglot move encoding is different from the engine's.
    fn to_move(self) -> Option<Move> {
        use crate::types::{Move, PieceKind};

        let from_sq = ((self.raw_move >> 6) & 0x3F) as u8;
        let mut to_sq = (self.raw_move & 0x3F) as u8;
        let promo_piece = (self.raw_move >> 12) & 0x7;

        // Remap Polyglot castling encodings:
        // e1->h1 (4->7)  => e1->g1 (4->6)
        // e1->a1 (4->0)  => e1->c1 (4->2)
        // e8->h8 (60->63)=> e8->g8 (60->62)
        // e8->a8 (60->56)=> e8->c8 (60->58)
        let mut is_castle = false;
        match (from_sq, to_sq) {
            (4, 7) => {
                to_sq = 6;
                is_castle = true;
            } // white O-O
            (4, 0) => {
                to_sq = 2;
                is_castle = true;
            } // white O-O-O
            (60, 63) => {
                to_sq = 62;
                is_castle = true;
            } // black O-O
            (60, 56) => {
                to_sq = 58;
                is_castle = true;
            } // black O-O-O
            _ => {}
        }

        let promo_kind = match promo_piece {
            1 => Some(PieceKind::Knight),
            2 => Some(PieceKind::Bishop),
            3 => Some(PieceKind::Rook),
            4 => Some(PieceKind::Queen),
            _ => None,
        };

        let mut m = Move::quiet(from_sq, to_sq);
        m.promotion = promo_kind;
        m.castle = is_castle;
        Some(m)
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

static BOOK: OnceLock<Option<OpeningBook>> = OnceLock::new();

fn get_book() -> &'static Option<OpeningBook> {
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

        for path in potential_paths {
            if let Ok(book) = OpeningBook::new(path.to_str().unwrap())
                && path.exists()
            {
                println!("info string Loaded opening book from: {}", path.display());
                return Some(book);
            }
        }

        println!(
            "info string Opening book '{}' not found in any standard location.",
            book_filename
        );
        None
    })
}

pub fn get_book_move(b: &Board) -> Option<String> {
    if let Some(book) = get_book() {
        let key = polyglot_zobrist::calculate_key(b);
        let entries = book.find_entries(key);

        if entries.is_empty() {
            return None;
        }

        let total_weight: u32 = entries.iter().map(|e| e.weight as u32).sum();
        if total_weight == 0 {
            return entries.first()?.to_move().map(format_uci);
        }

        let mut rng = get_rng().lock().unwrap();
        let mut random_weight = rng.rand() as u32 % total_weight;

        for entry in entries {
            if random_weight < entry.weight as u32 {
                println!("info string Playing book move.");
                return entry.to_move().map(format_uci);
            }
            random_weight -= entry.weight as u32;
        }
    }
    None
}
