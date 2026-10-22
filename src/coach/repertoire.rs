//! The opening repertoire the coach teaches: lines with a note on every move.

use crate::board::Board;
use crate::notation::parse_san_line;
use crate::types::{Color, Move, START_FEN};

/// One move of a line with what it is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub san: &'static str,
    pub note: &'static str,
}

const fn step(san: &'static str, note: &'static str) -> Step {
    Step { san, note }
}

/// A line from the starting position, learnt as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    pub id: &'static str,
    pub name: &'static str,
    /// The side you play in this line.
    pub side: Color,
    pub steps: &'static [Step],
    /// What to do once the line runs out.
    pub plan: &'static str,
}

impl Line {
    /// The line's moves from the starting position.
    pub fn moves(&self) -> Result<Vec<Move>, String> {
        let sans: Vec<&str> = self.steps.iter().map(|s| s.san).collect();
        parse_san_line(&start_board(), &sans).map_err(|e| format!("{}: {e}", self.id))
    }

    /// Whether you make the move at `ply`.
    pub fn is_yours(&self, ply: usize) -> bool {
        let mover = if ply.is_multiple_of(2) {
            Color::White
        } else {
            Color::Black
        };
        mover == self.side
    }

    /// How many of the moves are yours.
    pub fn your_move_count(&self) -> usize {
        (0..self.steps.len())
            .filter(|&ply| self.is_yours(ply))
            .count()
    }
}

/// Lines grouped by the opening they belong to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chapter {
    pub name: &'static str,
    pub side: Color,
    pub intro: &'static str,
    pub lines: &'static [Line],
}

/// The starting position, without evaluation state so it is cheap to copy.
pub fn start_board() -> Board {
    Board::from_fen(START_FEN)
        .unwrap_or_else(|_| Board::empty())
        .without_eval_state()
}

pub const CHAPTERS: &[Chapter] = &[
    QUEENS_GAMBIT,
    INDIAN_DEFENCES,
    BLACK_VS_E4,
    BLACK_VS_D4,
    BLACK_VS_OTHERS,
];

pub fn all_lines() -> impl Iterator<Item = &'static Line> {
    CHAPTERS.iter().flat_map(|chapter| chapter.lines.iter())
}

pub fn line_by_id(id: &str) -> Option<&'static Line> {
    all_lines().find(|line| line.id == id)
}

/// The lines whose moves begin with `played`, for following a game along the repertoire.
pub fn lines_continuing(played: &[Move]) -> Vec<(&'static Line, Move)> {
    all_lines()
        .filter_map(|line| {
            let moves = line.moves().ok()?;
            let next = *moves.get(played.len())?;
            moves.starts_with(played).then_some((line, next))
        })
        .collect()
}

const QUEENS_GAMBIT: Chapter = Chapter {
    name: "Queen's Gambit",
    side: Color::White,
    intro: "1.d4 and 2.c4: you offer a side pawn to pull Black's d-pawn out of the centre. \
            Most of the time Black declines, and you get easy development and a long-lasting edge.",
    lines: &[QGD_EXCHANGE, SLAV, QGA, QGA_TRAP, ALBIN, CHIGORIN],
};

const INDIAN_DEFENCES: Chapter = Chapter {
    name: "Against 1...Nf6 and others",
    side: Color::White,
    intro: "When Black answers 1.d4 with 1...Nf6, keep playing 2.c4 and take the centre with \
            Nc3 and often e4. The same ideas work against 1...g6 and 1...f5.",
    lines: &[
        NIMZO,
        INDIAN_INTO_QGD,
        KINGS_INDIAN,
        GRUNFELD,
        BENONI,
        BUDAPEST,
        DUTCH,
    ],
};

const BLACK_VS_E4: Chapter = Chapter {
    name: "Answering 1.e4 with 1...e5",
    side: Color::Black,
    intro: "Meet 1.e4 with 1...e5: a pawn in the centre, open lines for your bishop and queen, \
            and natural development with ...Nc6, ...Nf6 and ...Bc5 or ...Be7.",
    lines: &[
        SCHOLARS_DEFENCE,
        ITALIAN,
        EVANS,
        RUY_LOPEZ,
        RUY_EXCHANGE,
        SCOTCH,
        FOUR_KNIGHTS,
        VIENNA,
        KINGS_GAMBIT,
        CENTER_GAME,
        PONZIANI,
    ],
};

const BLACK_VS_D4: Chapter = Chapter {
    name: "Answering 1.d4 with 1...d5",
    side: Color::Black,
    intro: "Meet 1.d4 with 1...d5 and the Queen's Gambit Declined: rock-solid, and the same \
            structures you get as White, seen from the other side.",
    lines: &[QGD_BLACK, QGD_EXCHANGE_BLACK, LONDON, COLLE, CATALAN],
};

const BLACK_VS_OTHERS: Chapter = Chapter {
    name: "Answering 1.c4 and 1.Nf3",
    side: Color::Black,
    intro: "Against 1.c4 and 1.Nf3, take the centre and develop: there is less theory to know.",
    lines: &[ENGLISH, RETI],
};

const QGD_EXCHANGE: Line = Line {
    id: "w-qgd-exchange",
    name: "Declined: Exchange Variation",
    side: Color::White,
    steps: &[
        step(
            "d4",
            "Take the centre. The queen already protects this pawn.",
        ),
        step("d5", "Black takes the centre too."),
        step(
            "c4",
            "The Queen's Gambit: offer the c-pawn to tempt Black's d-pawn away from the centre.",
        ),
        step(
            "e6",
            "Black declines and keeps d5 protected: the Queen's Gambit Declined.",
        ),
        step("Nc3", "Develop and put a third piece on d5."),
        step("Nf6", "Black develops and guards d5 again."),
        step(
            "cxd5",
            "The Exchange Variation: trade now to fix Black's pawns and give yourself a clear plan.",
        ),
        step(
            "exd5",
            "Recapturing with the pawn keeps a pawn on d5. ...Nxd5 would let you take the centre with e4.",
        ),
        step(
            "Bg5",
            "Pin the knight to the queen: it cannot move without exposing the queen.",
        ),
        step("Be7", "Black blocks the pin."),
        step("e3", "Open the way for your other bishop and support d4."),
        step("O-O", "Black castles."),
        step("Bd3", "The best diagonal for this bishop, aimed at h7."),
        step("Nbd7", "Black develops towards f8 or b6."),
        step("Qc2", "Connect your rooks and add a second attacker on h7."),
        step("Re8", "The rook takes the half-open e-file."),
        step("Nf3", "Develop the last minor piece; it also eyes e5."),
        step("c6", "Black supports d5 with a pawn."),
        step(
            "O-O",
            "Castle. Now the plan is the minority attack on the queenside.",
        ),
    ],
    plan: "The minority attack: rooks to b1 and c1, then push b4 and b5 and trade on c6. Black is \
           left with a weak pawn on c6 or d5 to attack. Keep your pieces on Bd3, Qc2 and Nc3.",
};

const SLAV: Line = Line {
    id: "w-slav",
    name: "Slav Defence",
    side: Color::White,
    steps: &[
        step("d4", "Take the centre."),
        step("d5", "Black mirrors you."),
        step("c4", "The Queen's Gambit."),
        step(
            "c6",
            "The Slav: Black guards d5 with a pawn and keeps the light-squared bishop free.",
        ),
        step("Nf3", "Develop and control e5."),
        step("Nf6", "Black develops."),
        step(
            "e3",
            "Solid: support d4 and free your light-squared bishop.",
        ),
        step("Bf5", "Black's bishop comes out before ...e6 locks it in."),
        step("Nc3", "Develop and add pressure on d5."),
        step("e6", "Black builds a solid triangle of pawns."),
        step(
            "Nh4",
            "Chase the bishop: you are aiming to win the bishop pair.",
        ),
        step("Bg6", "The bishop steps back but stays on its diagonal."),
        step(
            "Nxg6",
            "Take the bishop: now you have two bishops against bishop and knight.",
        ),
        step(
            "hxg6",
            "Black recaptures towards the centre and opens the h-file.",
        ),
        step("Bd3", "Develop and aim at g6."),
        step("Nbd7", "Black develops."),
        step(
            "O-O",
            "Castle short. Black's h-file is half-open, so don't hurry with h3.",
        ),
    ],
    plan: "Your two bishops are a lasting advantage: open the position with e4 or c5 at the right \
           moment, keep the bishops, and play Qc2 and Rd1.",
};

const QGA: Line = Line {
    id: "w-qga",
    name: "Queen's Gambit Accepted",
    side: Color::White,
    steps: &[
        step("d4", "Take the centre."),
        step("d5", "Black mirrors you."),
        step("c4", "The Queen's Gambit."),
        step(
            "dxc4",
            "Black accepts. Holding on to the pawn is hard, and Black gives up the centre.",
        ),
        step(
            "e3",
            "Simple and strong: open the diagonal so your bishop wins the pawn back.",
        ),
        step("Nf6", "Black develops."),
        step("Bxc4", "Pawn back, and your bishop is on a fine diagonal."),
        step("e6", "Black opens a path for the dark-squared bishop."),
        step("Nf3", "Develop and get ready to castle."),
        step("c5", "Black strikes at your centre."),
        step("O-O", "King to safety first."),
        step("a6", "Black prepares ...b5 to chase your bishop."),
        step("a4", "Stop ...b5 for good: your bishop keeps its diagonal."),
        step("cxd4", "Black trades in the centre."),
        step(
            "exd4",
            "Recapture with the pawn: you get an isolated d-pawn, which brings active pieces.",
        ),
        step("Nc6", "Black attacks d4."),
        step("Nc3", "Develop and defend d4 indirectly."),
        step("Be7", "Black develops."),
        step("Bf4", "Develop the last minor piece to an active square."),
    ],
    plan: "The isolated d-pawn gives you open lines and outposts on e5 and c5. Put a knight on \
           e5, rooks on e1 and d1, and look for d5 to open the position or an attack on the king.",
};

const QGA_TRAP: Line = Line {
    id: "w-qga-trap",
    name: "Accepted: the ...b5 trap",
    side: Color::White,
    steps: &[
        step("d4", "Take the centre."),
        step("d5", "Black mirrors you."),
        step("c4", "The Queen's Gambit."),
        step("dxc4", "Black accepts."),
        step("e3", "Attack the pawn on c4 with your bishop."),
        step(
            "b5",
            "A common beginner's mistake: Black tries to keep the pawn.",
        ),
        step("a4", "Undermine the b5 pawn straight away."),
        step("c6", "Black holds b5 with a pawn."),
        step("axb5", "Take."),
        step(
            "cxb5",
            "The natural recapture, but it opens the long diagonal to the rook on a8.",
        ),
        step(
            "Qf3",
            "Double attack: the queen hits the rook on a8 and Black cannot keep everything.",
        ),
    ],
    plan: "Black is losing material: if the knight blocks on c6, take it with check from the \
           queen. Lesson: pawns grabbed early in the Queen's Gambit are hard to keep.",
};

const ALBIN: Line = Line {
    id: "w-albin",
    name: "Albin Counter-Gambit",
    side: Color::White,
    steps: &[
        step("d4", "Take the centre."),
        step("d5", "Black mirrors you."),
        step("c4", "The Queen's Gambit."),
        step("e5", "The Albin: Black gambits a pawn for quick play."),
        step("dxe5", "Take the pawn."),
        step(
            "d4",
            "The d4 pawn cramps you. Don't play e3? here: ...Bb4+ sets up a famous trap.",
        ),
        step(
            "Nf3",
            "Develop and attack d4, keeping your extra pawn on e5.",
        ),
        step("Nc6", "Black attacks e5."),
        step(
            "g3",
            "Prepare Bg2: the bishop will hit Black's queenside along the long diagonal.",
        ),
        step("Be6", "Black develops and eyes your c4 pawn."),
        step("Nbd2", "Develop and keep c4 protected."),
        step("Qd7", "Black prepares to castle queenside."),
        step(
            "Bg2",
            "Your bishop on the long diagonal is your best piece.",
        ),
        step("O-O-O", "Black castles long, towards your bishop."),
        step("O-O", "Castle, then attack with b4 and Qa4."),
    ],
    plan: "You are a pawn up. Castle, then attack the black king with b4, b5 and Qa4 while your \
           bishop on g2 rakes the long diagonal.",
};

const CHIGORIN: Line = Line {
    id: "w-chigorin",
    name: "Chigorin Defence",
    side: Color::White,
    steps: &[
        step("d4", "Take the centre."),
        step("d5", "Black mirrors you."),
        step("c4", "The Queen's Gambit."),
        step(
            "Nc6",
            "The Chigorin: Black develops fast and plans to give up a bishop for a knight.",
        ),
        step("Nf3", "Develop and stop ...e5."),
        step("Bg4", "Black pins the knight."),
        step(
            "cxd5",
            "Trade to open the c-file and gain time on Black's queen.",
        ),
        step("Bxf3", "Black takes before you can play e4."),
        step(
            "gxf3",
            "Recapture with the pawn: you get the two bishops and a strong centre.",
        ),
        step("Qxd5", "Black wins the pawn back."),
        step("e3", "Solid: defend d4 and open the light-squared bishop."),
        step("e5", "Black challenges your centre."),
        step("Nc3", "Develop with tempo on the queen."),
        step("Bb4", "Black pins the knight."),
        step("Bd2", "Unpin and get ready to recapture."),
        step("Bxc3", "Black gives up the second bishop."),
        step(
            "bxc3",
            "Recapture: your two bishops and centre make up for the doubled pawns.",
        ),
    ],
    plan: "You have the bishop pair. Open the position with c4 and Rb1 against b7, keep your king \
           safe on the queenside or in the centre, and use the half-open g-file.",
};

const NIMZO: Line = Line {
    id: "w-nimzo",
    name: "Nimzo-Indian",
    side: Color::White,
    steps: &[
        step("d4", "Take the centre."),
        step("Nf6", "Black develops and stops e4."),
        step("c4", "Grab more space."),
        step("e6", "Black opens the dark-squared bishop."),
        step("Nc3", "Develop and prepare e4."),
        step(
            "Bb4",
            "The Nimzo-Indian: Black pins your knight so e4 is hard to play.",
        ),
        step("e3", "Calm and solid: support d4 and develop the bishop."),
        step("O-O", "Black castles."),
        step("Bd3", "Develop towards Black's king."),
        step("d5", "Black takes the centre."),
        step("Nf3", "Develop and get ready to castle."),
        step("c5", "Black hits your centre."),
        step("O-O", "King safety first."),
        step("Nc6", "Black adds pressure on d4."),
        step("a3", "Ask the bishop to decide."),
        step("Bxc3", "Black gives up the bishop for your knight."),
        step(
            "bxc3",
            "Recapture: the bishop pair and the centre are yours.",
        ),
    ],
    plan: "With the two bishops, aim for e4 to open lines for them. Keep the c3 pawn protected \
           and use Qc2 and Re1 to support e4.",
};

const INDIAN_INTO_QGD: Line = Line {
    id: "w-indian-qgd",
    name: "1...Nf6 into the Queen's Gambit",
    side: Color::White,
    steps: &[
        step("d4", "Take the centre."),
        step("Nf6", "Black develops."),
        step("c4", "Grab more space."),
        step("e6", "Black opens the dark-squared bishop."),
        step("Nc3", "Develop."),
        step("d5", "Black plays ...d5 after all."),
        step(
            "cxd5",
            "This is the Exchange Variation of the Queen's Gambit Declined, reached in another order.",
        ),
        step("exd5", "Black keeps a pawn on d5."),
        step("Bg5", "Pin the knight, just as in the Exchange Variation."),
    ],
    plan: "The same position as the Exchange Variation: e3, Bd3, Qc2, Nf3, castle, then the \
           minority attack. Different move orders often reach the same position.",
};

const KINGS_INDIAN: Line = Line {
    id: "w-kings-indian",
    name: "King's Indian",
    side: Color::White,
    steps: &[
        step("d4", "Take the centre."),
        step("Nf6", "Black develops."),
        step("c4", "Grab more space."),
        step("g6", "Black will fianchetto the bishop."),
        step("Nc3", "Develop and prepare e4."),
        step("Bg7", "The bishop on the long diagonal."),
        step("e4", "Take the whole centre: Black has allowed it."),
        step("d6", "Black stops e5."),
        step("Nf3", "Develop."),
        step("O-O", "Black castles."),
        step("Be2", "The classical setup: simple and solid."),
        step("e5", "Black challenges the centre."),
        step("O-O", "Castle."),
        step("Nc6", "Black adds pressure on d4."),
        step("d5", "Close the centre with gain of time on the knight."),
        step(
            "Ne7",
            "The knight heads for the kingside, where Black will attack.",
        ),
        step(
            "b4",
            "Attack on your side: you play on the queenside with c5.",
        ),
    ],
    plan: "Black attacks your king with ...f5; you attack on the queenside with c5 and opening the \
           c-file. Move fast, and keep a knight or bishop near your king.",
};

const GRUNFELD: Line = Line {
    id: "w-grunfeld",
    name: "Grünfeld",
    side: Color::White,
    steps: &[
        step("d4", "Take the centre."),
        step("Nf6", "Black develops."),
        step("c4", "Grab more space."),
        step("g6", "Black prepares the fianchetto."),
        step("Nc3", "Develop."),
        step(
            "d5",
            "The Grünfeld: Black lets you take the centre and will attack it.",
        ),
        step("cxd5", "The Exchange Variation: take the centre at once."),
        step("Nxd5", "Black recaptures."),
        step("e4", "Gain time on the knight and build a big centre."),
        step("Nxc3", "Black trades."),
        step(
            "bxc3",
            "Recapture towards the centre: d4 is now well protected.",
        ),
        step("Bg7", "The bishop aims at your centre."),
        step("Bc4", "Develop actively."),
        step("c5", "Black attacks d4."),
        step(
            "Ne2",
            "Knight to e2 so it can't be pinned, and it guards d4.",
        ),
        step("Nc6", "Black piles up on d4."),
        step("Be3", "Defend d4 again."),
        step("O-O", "Black castles."),
        step("O-O", "Castle and hold the centre."),
    ],
    plan: "Hold your big centre with Rc1 and Qd2, then look for d5 to gain space. Black's play \
           is all against d4, so keep it well protected.",
};

const BENONI: Line = Line {
    id: "w-benoni",
    name: "Benoni",
    side: Color::White,
    steps: &[
        step("d4", "Take the centre."),
        step("Nf6", "Black develops."),
        step("c4", "Grab more space."),
        step("c5", "The Benoni: Black challenges d4 at once."),
        step("d5", "Push past: you gain space."),
        step("e6", "Black attacks your d5 pawn."),
        step("Nc3", "Defend d5."),
        step("exd5", "Black trades."),
        step(
            "cxd5",
            "Recapture with the pawn: you have more space in the centre.",
        ),
        step("d6", "Black blocks your d-pawn and watches e5."),
        step("e4", "Build the centre."),
        step("g6", "The bishop goes to g7."),
        step("Nf3", "Develop."),
        step("Bg7", "The fianchetto."),
        step("Be2", "Develop simply."),
        step("O-O", "Black castles."),
        step("O-O", "Castle; your extra space is a lasting advantage."),
    ],
    plan: "Your plan is the e5 push to open the centre while Black's pieces are cramped. Stop \
           Black's ...b5 with a4 and keep your centre protected.",
};

const BUDAPEST: Line = Line {
    id: "w-budapest",
    name: "Budapest Gambit",
    side: Color::White,
    steps: &[
        step("d4", "Take the centre."),
        step("Nf6", "Black develops."),
        step("c4", "Grab more space."),
        step("e5", "The Budapest: a gambit."),
        step("dxe5", "Take the pawn."),
        step("Ng4", "Black attacks e5 to win it back."),
        step("Bf4", "Defend e5."),
        step("Nc6", "Black attacks e5 again."),
        step("Nf3", "Defend e5 a third time."),
        step("Bb4+", "Black checks."),
        step("Nbd2", "Block with a piece that develops."),
        step("Qe7", "Black attacks e5 a fourth time."),
        step(
            "e3",
            "Free your bishop: e5 will fall, but you develop smoothly.",
        ),
        step("Ngxe5", "Black wins the pawn back."),
        step("Nxe5", "Trade."),
        step("Nxe5", "Black recaptures."),
        step(
            "Be2",
            "Develop and castle next; your position is comfortable.",
        ),
    ],
    plan: "Castle, play a3 to question the bishop, and use your extra central pawn and space on \
           the queenside.",
};

const DUTCH: Line = Line {
    id: "w-dutch",
    name: "Dutch Defence",
    side: Color::White,
    steps: &[
        step("d4", "Take the centre."),
        step("f5", "The Dutch: Black controls e4 with a pawn."),
        step("c4", "Grab space, as always."),
        step("Nf6", "Black develops."),
        step("g3", "Fianchetto: your bishop on g2 will fight for e4."),
        step("e6", "Black opens the bishop."),
        step("Bg2", "The bishop aims at e4 and the long diagonal."),
        step("Be7", "Black develops."),
        step("Nf3", "Develop."),
        step("O-O", "Black castles."),
        step("O-O", "Castle."),
        step("d6", "Black prepares ...e5."),
        step("Nc3", "Develop and add another attacker on e4."),
    ],
    plan: "Play for the e4 break with Qc2 and Re1. Black's ...f5 weakened e6 and the king, so \
           opening the centre favours you.",
};

const SCHOLARS_DEFENCE: Line = Line {
    id: "b-scholars",
    name: "Stopping the Scholar's Mate",
    side: Color::Black,
    steps: &[
        step("e4", "White takes the centre."),
        step("e5", "Take your share."),
        step("Qh5", "An early queen attack: it hits e5 and eyes f7."),
        step("Nc6", "Defend e5 with a developing move."),
        step("Bc4", "Now White threatens Qxf7, checkmate."),
        step("g6", "Block the queen's line to f7, gaining time."),
        step("Qf3", "The queen tries again on f7."),
        step(
            "Nf6",
            "Block the f-file with a knight. Develop and defend at the same time.",
        ),
        step("Ne2", "White develops."),
        step("Bg7", "Your bishop now has a great diagonal."),
        step("Nbc3", "White develops."),
        step(
            "d6",
            "Open the other bishop and support e5. You are ahead in development.",
        ),
    ],
    plan: "White's queen came out too early and you developed with gain of time. Castle, then \
           look for ...Nd4 to chase the queen and ...Bg4 to harass it again.",
};

const ITALIAN: Line = Line {
    id: "b-italian",
    name: "Italian Game",
    side: Color::Black,
    steps: &[
        step("e4", "White takes the centre."),
        step("e5", "Take your share."),
        step("Nf3", "White attacks e5."),
        step("Nc6", "Defend with development."),
        step("Bc4", "The Italian: the bishop aims at f7."),
        step("Bc5", "Mirror it: your bishop aims at f2."),
        step("c3", "White prepares d4."),
        step("Nf6", "Develop and hit e4."),
        step("d3", "White defends e4 and keeps things quiet."),
        step("d6", "Support e5 and open your light-squared bishop."),
        step("O-O", "White castles."),
        step("O-O", "Castle too."),
        step("Re1", "White's rook supports e4."),
        step(
            "a6",
            "Make room for the bishop on a7 so d4 can't chase it with gain of time.",
        ),
        step("Bb3", "White tucks the bishop away from ...Na5 and ...d5."),
        step("Ba7", "Your bishop is safe and still aims at f2."),
    ],
    plan: "Play ...h6 against Bg5, ...Re8 and ...Be6 to trade White's strong bishop. Look for \
           ...d5 when it is well supported.",
};

const EVANS: Line = Line {
    id: "b-evans",
    name: "Evans Gambit",
    side: Color::Black,
    steps: &[
        step("e4", "White takes the centre."),
        step("e5", "Take your share."),
        step("Nf3", "White attacks e5."),
        step("Nc6", "Defend with development."),
        step("Bc4", "The Italian."),
        step("Bc5", "Mirror it."),
        step("b4", "The Evans Gambit: a pawn to gain time for c3 and d4."),
        step("Bxb4", "Take the pawn."),
        step("c3", "White chases the bishop and prepares d4."),
        step(
            "Be7",
            "A safe retreat that keeps your king's side well covered.",
        ),
        step("d4", "White opens the centre."),
        step("Na5", "Hit the strong bishop on c4."),
        step("Be2", "The bishop steps back."),
        step("d6", "Hold e5 and open your light-squared bishop."),
        step("dxe5", "White opens the d-file."),
        step("dxe5", "Recapture."),
        step("Qa4+", "White checks to win the pawn back."),
        step("c6", "Block with a pawn."),
        step("Nxe5", "White takes back the pawn."),
        step(
            "Nf6",
            "Develop and get ready to castle: material is level and your position is solid.",
        ),
    ],
    plan: "Castle, play ...Qc7 and ...Be6, and bring the knight on a5 back via b7 or c4. White's \
           pawns are scattered, so trade pieces and aim for a good endgame.",
};

const RUY_LOPEZ: Line = Line {
    id: "b-ruy",
    name: "Ruy Lopez",
    side: Color::Black,
    steps: &[
        step("e4", "White takes the centre."),
        step("e5", "Take your share."),
        step("Nf3", "White attacks e5."),
        step("Nc6", "Defend with development."),
        step(
            "Bb5",
            "The Ruy Lopez: White pressures the knight that defends e5.",
        ),
        step("a6", "Ask the bishop at once."),
        step("Ba4", "The bishop keeps the pin."),
        step("Nf6", "Develop and attack e4."),
        step("O-O", "White castles; e4 is indirectly defended."),
        step("Be7", "Develop and prepare to castle."),
        step("Re1", "White defends e4."),
        step("b5", "Free yourself from the pin."),
        step("Bb3", "The bishop keeps aiming at f7."),
        step("d6", "Support e5 and open your bishop."),
        step("c3", "White prepares d4."),
        step("O-O", "Castle."),
        step("h3", "White stops ...Bg4."),
        step("Na5", "Chase the bishop off its diagonal."),
        step("Bc2", "The bishop retreats."),
        step("c5", "Grab space on the queenside."),
    ],
    plan: "The Chigorin setup: ...Qc7 and ...Nc6 to pressure d4, and ...Re8 and ...Bf8 for the \
           centre. Play on the queenside with ...c4 or ...cxd4 at the right moment.",
};

const RUY_EXCHANGE: Line = Line {
    id: "b-ruy-exchange",
    name: "Ruy Lopez: Exchange",
    side: Color::Black,
    steps: &[
        step("e4", "White takes the centre."),
        step("e5", "Take your share."),
        step("Nf3", "White attacks e5."),
        step("Nc6", "Defend with development."),
        step("Bb5", "The Ruy Lopez."),
        step("a6", "Ask the bishop."),
        step("Bxc6", "The Exchange Variation: White gives up the bishop."),
        step(
            "dxc6",
            "Recapture towards the centre, opening your queen and bishop.",
        ),
        step("O-O", "White castles; Nxe5 is now a threat."),
        step("f6", "Protect e5 solidly."),
        step("d4", "White opens the centre."),
        step("exd4", "Trade."),
        step("Nxd4", "White recaptures."),
        step("c5", "Chase the knight."),
        step("Nb3", "The knight retreats."),
        step(
            "Qxd1",
            "Trade queens: your two bishops shine in the endgame.",
        ),
        step("Rxd1", "White recaptures."),
        step("Bg4", "Develop with gain of time on the rook."),
    ],
    plan: "Your two bishops are worth more than White's healthier pawns. Keep the position open, \
           castle long, and use the bishops.",
};

const SCOTCH: Line = Line {
    id: "b-scotch",
    name: "Scotch Game",
    side: Color::Black,
    steps: &[
        step("e4", "White takes the centre."),
        step("e5", "Take your share."),
        step("Nf3", "White attacks e5."),
        step("Nc6", "Defend with development."),
        step("d4", "The Scotch: White opens the centre at once."),
        step("exd4", "Take."),
        step("Nxd4", "White recaptures."),
        step("Bc5", "Develop and attack the knight."),
        step("Be3", "White defends the knight."),
        step("Qf6", "Add pressure on d4."),
        step("c3", "White defends d4 with a pawn."),
        step(
            "Nge7",
            "Develop the knight to e7 so the f-file stays open for your queen.",
        ),
        step("Bc4", "White develops."),
        step("O-O", "Castle."),
        step("O-O", "White castles."),
        step(
            "Bb6",
            "Step out of the way of any Nxc6 tricks and keep the pressure on d4.",
        ),
    ],
    plan: "Play ...d6, ...Ne5 or ...Ng6, and ...Be6 to trade White's strong bishop. Your pieces \
           are active and the centre is balanced.",
};

const FOUR_KNIGHTS: Line = Line {
    id: "b-four-knights",
    name: "Four Knights",
    side: Color::Black,
    steps: &[
        step("e4", "White takes the centre."),
        step("e5", "Take your share."),
        step("Nf3", "White attacks e5."),
        step("Nc6", "Defend with development."),
        step("Nc3", "White develops."),
        step("Nf6", "The Four Knights."),
        step("Bb5", "White pins your knight."),
        step("Bb4", "Copy: pin White's knight."),
        step("O-O", "White castles."),
        step("O-O", "Castle."),
        step("d3", "White supports e4."),
        step("d6", "Support e5 and open your bishop."),
    ],
    plan: "A symmetrical, balanced position. Watch for Bg5 pins: ...Bxc3 and ...Qe7 are typical. \
           Develop the last bishop to g4 or e6.",
};

const VIENNA: Line = Line {
    id: "b-vienna",
    name: "Vienna Game",
    side: Color::Black,
    steps: &[
        step("e4", "White takes the centre."),
        step("e5", "Take your share."),
        step("Nc3", "The Vienna: White prepares f4."),
        step("Nf6", "Develop and hit e4."),
        step("f4", "White's gambit-style f4."),
        step(
            "d5",
            "Strike in the centre: the best answer to a wing attack.",
        ),
        step("fxe5", "White takes."),
        step("Nxe4", "Your knight takes a great central square."),
        step("Nf3", "White develops."),
        step("Be7", "Develop and prepare to castle."),
    ],
    plan: "Castle, then play ...c5 and ...Nc6 against White's centre. Your knight on e4 is strong; \
           trade it only for something good.",
};

const KINGS_GAMBIT: Line = Line {
    id: "b-kings-gambit",
    name: "King's Gambit",
    side: Color::Black,
    steps: &[
        step("e4", "White takes the centre."),
        step("e5", "Take your share."),
        step(
            "f4",
            "The King's Gambit: White offers a pawn to open the f-file.",
        ),
        step("exf4", "Accept the pawn."),
        step("Nf3", "White stops ...Qh4+."),
        step("d5", "Hit back in the centre and open your pieces."),
        step("exd5", "White takes."),
        step("Nf6", "Develop and attack d5."),
    ],
    plan: "Win back d5 with ...Nxd5, develop quickly with ...Bd6 and ...O-O, and keep the f4 \
           pawn if it is safe. White's king is the weaker one.",
};

const CENTER_GAME: Line = Line {
    id: "b-center-game",
    name: "Center Game",
    side: Color::Black,
    steps: &[
        step("e4", "White takes the centre."),
        step("e5", "Take your share."),
        step("d4", "The Center Game: White opens the centre at once."),
        step("exd4", "Take."),
        step("Qxd4", "White recaptures with the queen, early."),
        step("Nc6", "Develop with gain of time on the queen."),
        step("Qe3", "The queen steps away."),
        step("Nf6", "Develop and hit e4."),
        step("Nc3", "White defends e4."),
        step("Bb4", "Pin the knight that defends e4."),
        step("Bd2", "White unpins."),
        step("O-O", "Castle; your development is faster."),
        step("O-O-O", "White castles long."),
        step(
            "Re8",
            "Put pressure on the e-file against e4 and the queen.",
        ),
    ],
    plan: "You are ahead in development. Attack e4, and push ...d5 or ...b5 against White's king \
           on the queenside.",
};

const PONZIANI: Line = Line {
    id: "b-ponziani",
    name: "Ponziani",
    side: Color::Black,
    steps: &[
        step("e4", "White takes the centre."),
        step("e5", "Take your share."),
        step("Nf3", "White attacks e5."),
        step("Nc6", "Defend with development."),
        step("c3", "The Ponziani: White prepares d4."),
        step(
            "d5",
            "Strike in the centre: c3 took away the knight's best square, so White is slower.",
        ),
        step("Qa4", "White pins your knight and eyes e5."),
        step("Bd7", "Unpin with a developing move."),
        step("exd5", "White takes, attacking your knight."),
        step(
            "Nd4",
            "Counterattack: the knight jumps forward and hits the queen's defender on f3.",
        ),
        step("Qd1", "The queen goes home."),
        step("Nxf3+", "Trade knights: you are ahead in development."),
        step("Qxf3", "White recaptures."),
        step("Nf6", "Develop and get ready to win back d5."),
    ],
    plan: "Play ...Bd6 and ...O-O, then win back d5 or push ...e4 to gain space. Your pieces come \
           out faster than White's.",
};

const QGD_BLACK: Line = Line {
    id: "b-qgd",
    name: "Queen's Gambit Declined",
    side: Color::Black,
    steps: &[
        step("d4", "White takes the centre."),
        step("d5", "Take your share."),
        step("c4", "The Queen's Gambit."),
        step("e6", "Decline: keep d5 protected and open your bishop."),
        step("Nc3", "White adds pressure on d5."),
        step("Nf6", "Develop and defend d5."),
        step("Bg5", "White pins your knight."),
        step("Be7", "Break the pin."),
        step("e3", "White develops."),
        step("O-O", "Castle early: your king is safe."),
        step("Nf3", "White develops."),
        step("Nbd7", "Develop and support ...e5 or ...c5 later."),
        step("Rc1", "White's rook goes to the c-file."),
        step("c6", "Solidify d5."),
        step("Bd3", "White develops the last bishop."),
        step(
            "dxc4",
            "Take now, because White's bishop has just moved and must move again.",
        ),
        step("Bxc4", "White recaptures."),
        step(
            "Nd5",
            "Capablanca's freeing idea: trade pieces to relieve your cramped position.",
        ),
        step("Bxe7", "White trades."),
        step("Qxe7", "Recapture."),
        step("O-O", "White castles."),
        step("Nxc3", "Trade another pair."),
        step("Rxc3", "White recaptures."),
        step(
            "e5",
            "Free your light-squared bishop: Black's problems are solved.",
        ),
    ],
    plan: "After ...e5 your bishop on c8 comes to life. Trade on d4 or keep the tension, put rooks \
           on the d- and e-files, and the position is equal.",
};

const QGD_EXCHANGE_BLACK: Line = Line {
    id: "b-qgd-exchange",
    name: "Declined: Exchange Variation",
    side: Color::Black,
    steps: &[
        step("d4", "White takes the centre."),
        step("d5", "Take your share."),
        step("c4", "The Queen's Gambit."),
        step("e6", "Decline."),
        step("Nc3", "White develops."),
        step("Nf6", "Develop."),
        step("cxd5", "The Exchange Variation."),
        step(
            "exd5",
            "Recapture with the pawn, keeping one in the centre.",
        ),
        step("Bg5", "White pins your knight."),
        step("c6", "Support d5 and give your queen a path to b6 or a5."),
        step("e3", "White develops."),
        step("Be7", "Break the pin."),
        step("Bd3", "White develops."),
        step("O-O", "Castle."),
        step("Qc2", "White aims at h7."),
        step("Nbd7", "Develop towards f8, where the knight guards h7."),
    ],
    plan: "White will try the minority attack with b4 and b5. Meet it calmly, and use your own \
           chances on the kingside with ...Re8, ...Nf8 and ...Ne4.",
};

const LONDON: Line = Line {
    id: "b-london",
    name: "London System",
    side: Color::Black,
    steps: &[
        step("d4", "White takes the centre."),
        step("d5", "Take your share."),
        step(
            "Bf4",
            "The London System: White develops the bishop before e3.",
        ),
        step("Nf6", "Develop."),
        step("e3", "White builds a solid triangle."),
        step(
            "c5",
            "Challenge d4 at once: the best way to meet the London.",
        ),
        step("c3", "White supports d4."),
        step("Nc6", "Add pressure on d4."),
        step("Nd2", "White develops."),
        step("e6", "Open your bishop."),
        step("Ngf3", "White develops."),
        step("Bd6", "Offer to trade White's best bishop."),
        step("Bg3", "White keeps the bishop."),
        step("O-O", "Castle."),
        step("Bd3", "White develops."),
        step("b6", "Prepare ...Bb7 for your last piece."),
    ],
    plan: "Play ...Bb7, ...Qe7 and look for ...e5 to free your position. If White takes on d6, \
           recapture with the queen.",
};

const COLLE: Line = Line {
    id: "b-colle",
    name: "Colle and 2.Nf3",
    side: Color::Black,
    steps: &[
        step("d4", "White takes the centre."),
        step("d5", "Take your share."),
        step("Nf3", "White develops."),
        step("Nf6", "Develop."),
        step("e3", "White plays the quiet Colle setup."),
        step("e6", "Open your bishop."),
        step("Bd3", "White aims at your king."),
        step("c5", "Challenge White's centre."),
        step("c3", "White supports d4."),
        step("Nc6", "Add pressure on d4."),
        step("Nbd2", "White prepares e4."),
        step("Bd6", "Develop and watch e5."),
        step("O-O", "White castles."),
        step("O-O", "Castle."),
    ],
    plan: "When White plays e4, take on e4 or d4 to keep the centre open. Develop the last bishop \
           with ...b6 and ...Bb7, or ...Qc7 and ...e5.",
};

const CATALAN: Line = Line {
    id: "b-catalan",
    name: "Catalan",
    side: Color::Black,
    steps: &[
        step("d4", "White takes the centre."),
        step("d5", "Take your share."),
        step("c4", "The Queen's Gambit."),
        step("e6", "Decline."),
        step("Nf3", "White develops."),
        step("Nf6", "Develop."),
        step(
            "g3",
            "The Catalan: White's bishop will aim down the long diagonal.",
        ),
        step("Be7", "Develop and get ready to castle."),
        step("Bg2", "The fianchetto."),
        step("O-O", "Castle."),
        step("O-O", "White castles."),
        step("dxc4", "Take the pawn now that White has castled."),
        step("Qc2", "White wants the pawn back."),
        step("a6", "Prepare ...b5 to keep the pawn or gain time."),
    ],
    plan: "If White takes back on c4, play ...b5 and ...Bb7 to fight White's bishop on the long \
           diagonal. Get ...c5 in to free your position.",
};

const ENGLISH: Line = Line {
    id: "b-english",
    name: "English Opening",
    side: Color::Black,
    steps: &[
        step("c4", "The English: White controls d5 from the side."),
        step("e5", "Take the centre."),
        step("Nc3", "White develops."),
        step("Nf6", "Develop."),
        step("Nf3", "White attacks e5."),
        step("Nc6", "Defend with development."),
        step("g3", "White plans the fianchetto."),
        step("d5", "Grab the centre while you can."),
        step("cxd5", "White takes."),
        step("Nxd5", "Recapture."),
        step("Bg2", "White's bishop hits d5."),
        step("Nb6", "Step back to safety."),
        step("O-O", "White castles."),
        step("Be7", "Develop and castle next."),
    ],
    plan: "Castle, play ...Be6 and ...f6 to hold e5, and watch the long diagonal. This is a Sicilian \
           Dragon with colours reversed.",
};

const RETI: Line = Line {
    id: "b-reti",
    name: "Réti and 1.Nf3",
    side: Color::Black,
    steps: &[
        step("Nf3", "White develops."),
        step("d5", "Take the centre."),
        step("g3", "White plans a fianchetto."),
        step("Nf6", "Develop."),
        step("Bg2", "White's bishop on the long diagonal."),
        step("c6", "Blunt the bishop's diagonal."),
        step("O-O", "White castles."),
        step(
            "Bg4",
            "Develop the bishop outside the pawn chain before ...e6.",
        ),
        step("d3", "White prepares e4."),
        step("Nbd7", "Develop and support ...e5."),
    ],
    plan: "Play ...e6, ...Bd6 or ...Be7, and castle. A solid setup where White's bishop bites on \
           your c6 and d5 pawns.",
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_line_is_legal_from_the_start() {
        for line in all_lines() {
            assert!(line.moves().is_ok(), "{:?}", line.moves());
        }
    }

    #[test]
    fn line_ids_are_unique() {
        let ids: HashSet<&str> = all_lines().map(|line| line.id).collect();

        assert_eq!(ids.len(), all_lines().count());
    }

    #[test]
    fn lines_end_on_your_own_move_and_have_notes() {
        for line in all_lines() {
            assert!(
                line.is_yours(line.steps.len() - 1),
                "{} ends on the opponent's move",
                line.id
            );
            assert!(
                line.steps.iter().all(|s| !s.note.is_empty()),
                "{} has a move without a note",
                line.id
            );
        }
    }

    #[test]
    fn chapter_lines_are_played_from_the_chapters_side() {
        for chapter in CHAPTERS {
            assert!(
                chapter.lines.iter().all(|line| line.side == chapter.side),
                "{}",
                chapter.name
            );
        }
    }

    #[test]
    fn games_are_followed_along_every_matching_line() {
        let start = start_board();
        let played = parse_san_line(&start, &["d4", "d5", "c4"]).unwrap();

        let continuing = lines_continuing(&played);

        let ids: HashSet<&str> = continuing.iter().map(|(line, _)| line.id).collect();
        assert!(ids.contains("w-qgd-exchange") && ids.contains("w-slav") && ids.contains("b-qgd"));
        assert!(!ids.contains("w-nimzo"));
    }
}
