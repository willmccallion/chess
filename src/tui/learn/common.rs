//! Pieces every learn-mode screen uses: an interactive board, colours and small widgets.

use super::super::app::PieceStyle;
use super::super::input::MoveInput;
use super::super::view::{self, ACCENT, BoardGeometry, BoardScene, DIM, PROMOTION_KINDS, ScreenLayout};
use crate::board::Board;
use crate::coach::judgement::MoveClass;
use crate::notation::parse_typed_move;
use crate::types::{Move, MoveList, PieceKind};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color as Rgb, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

pub const GOOD: Rgb = Rgb::Rgb(120, 200, 120);
pub const BAD: Rgb = Rgb::Rgb(235, 100, 90);
pub const WARN: Rgb = Rgb::Rgb(240, 170, 80);
pub const INFO: Rgb = Rgb::Rgb(120, 170, 230);
pub const TEXT: Rgb = Rgb::Rgb(225, 225, 225);

/// Square tints for pointing at moves on the board.
pub const MARK_GOOD: Rgb = Rgb::Rgb(120, 170, 95);
pub const MARK_BAD: Rgb = Rgb::Rgb(200, 95, 80);
pub const MARK_WARN: Rgb = Rgb::Rgb(214, 150, 70);
pub const MARK_HINT: Rgb = Rgb::Rgb(100, 140, 190);

pub const fn class_color(class: MoveClass) -> Rgb {
    match class {
        MoveClass::Book => INFO,
        MoveClass::Best | MoveClass::Good => GOOD,
        MoveClass::Inaccuracy => Rgb::Rgb(230, 210, 100),
        MoveClass::Mistake => WARN,
        MoveClass::Blunder => BAD,
    }
}

pub const fn class_mark(class: MoveClass) -> Option<Rgb> {
    match class {
        MoveClass::Book | MoveClass::Best | MoveClass::Good => None,
        MoveClass::Inaccuracy => Some(Rgb::Rgb(200, 185, 90)),
        MoveClass::Mistake => Some(MARK_WARN),
        MoveClass::Blunder => Some(MARK_BAD),
    }
}

/// The squares a move goes from and to, tinted `color`.
pub fn move_marks(m: Move, color: Rgb) -> Vec<(u8, Rgb)> {
    vec![(m.from, color), (m.to, color)]
}

/// A board you can move pieces on by mouse or by typing.
pub struct BoardPanel {
    board: Board,
    legal: MoveList,
    input: MoveInput,
    pub flipped: bool,
    pub last_move: Option<Move>,
    /// The move being typed, when move entry is open.
    pub typed: Option<String>,
}

/// What a key did on a board.
pub enum BoardKey {
    NotHandled,
    Handled,
    Move(Move),
    Invalid(String),
}

impl BoardPanel {
    pub fn new(board: Board, flipped: bool) -> Self {
        let mut panel = Self {
            board: Board::empty(),
            legal: MoveList::new(),
            input: MoveInput::default(),
            flipped,
            last_move: None,
            typed: None,
        };
        panel.set_position(board, None);
        panel
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    pub fn legal(&self) -> &[Move] {
        self.legal.as_slice()
    }

    pub fn set_position(&mut self, board: Board, last_move: Option<Move>) {
        self.board = board;
        self.legal = MoveList::new();
        self.board.generate_legal_moves(&mut self.legal);
        self.last_move = last_move;
        self.input.clear();
    }

    /// Plays `m` on the panel's board.
    pub fn play(&mut self, m: Move) {
        let mut next = self.board.clone();
        next.make_move(m);
        self.set_position(next, Some(m));
    }

    pub fn scene(&self, piece_style: PieceStyle, marks: Vec<(u8, Rgb)>) -> BoardScene<'_> {
        BoardScene {
            board: &self.board,
            flipped: self.flipped,
            piece_style,
            selected: self.input.selected(),
            targets: self.input.targets(self.legal.as_slice()),
            last_move: self.last_move,
            marks,
            promotion_open: self.input.pending_promotion().is_some(),
        }
    }

    /// Handles a mouse event when `interactive`; returns a move you completed.
    pub fn mouse(&mut self, mouse: MouseEvent, geometry: Option<BoardGeometry>, interactive: bool) -> Option<Move> {
        let geometry = geometry?;
        if !interactive {
            self.input.clear();
            return None;
        }
        let square = geometry.square_at(mouse.column, mouse.row, self.flipped);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.input.pending_promotion().is_some() {
                    return match geometry.promotion_at(mouse.column, mouse.row) {
                        Some(kind) => self.input.choose_promotion(self.legal.as_slice(), kind),
                        None => {
                            self.input.clear();
                            None
                        }
                    };
                }
                match square {
                    Some(square) => self.input.press(&self.board, self.legal.as_slice(), square),
                    None => {
                        self.input.clear();
                        None
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => match square {
                Some(square) => self.input.release(&self.board, self.legal.as_slice(), square),
                None => {
                    self.input.release_off_board();
                    None
                }
            },
            _ => None,
        }
    }

    /// Move typing and promotion choices; `Enter` opens typing when `interactive`.
    pub fn key(&mut self, key: KeyEvent, interactive: bool) -> BoardKey {
        if let Some(typed) = self.typed.as_mut() {
            match key.code {
                KeyCode::Enter => {
                    let text = self.typed.take().unwrap_or_default();
                    return match parse_typed_move(&self.board, self.legal.as_slice(), &text) {
                        Some(m) if interactive => BoardKey::Move(m),
                        Some(_) => BoardKey::Invalid("Wait for your turn".into()),
                        None => BoardKey::Invalid(format!("'{}' is not a legal move here", text.trim())),
                    };
                }
                KeyCode::Esc => self.typed = None,
                KeyCode::Backspace => {
                    typed.pop();
                }
                KeyCode::Char(c) if !c.is_control() => typed.push(c),
                _ => {}
            }
            return BoardKey::Handled;
        }
        if self.input.pending_promotion().is_some() {
            let kind = key.code.as_char().and_then(promotion_kind);
            return match kind {
                Some(kind) => self
                    .input
                    .choose_promotion(self.legal.as_slice(), kind)
                    .map_or(BoardKey::Handled, BoardKey::Move),
                None => {
                    self.input.clear();
                    BoardKey::Handled
                }
            };
        }
        if key.code == KeyCode::Enter && interactive {
            self.typed = Some(String::new());
            return BoardKey::Handled;
        }
        BoardKey::NotHandled
    }
}

fn promotion_kind(c: char) -> Option<PieceKind> {
    PROMOTION_KINDS.into_iter().find(|kind| kind.to_char_upper() == c.to_ascii_uppercase())
}

/// Board on the left, side panel on the right, one status line at the bottom.
pub fn screen_layout(area: Rect) -> ScreenLayout {
    view::board_layout(area, false)
}

/// Draws the board part of a screen, or a note that the terminal is too small.
pub fn draw_board(frame: &mut Frame, layout: &ScreenLayout, scene: &BoardScene) {
    match layout.board {
        Some(geometry) => {
            view::draw_board(frame.buffer_mut(), scene, geometry);
            if scene.promotion_open {
                view::draw_promotion_choice(frame, scene.board.turn, scene.piece_style, geometry);
            }
        }
        None => frame.render_widget(
            Paragraph::new("Make the terminal larger to see the board").style(Style::new().fg(ACCENT)),
            layout.side,
        ),
    }
}

/// The bottom line: what you are typing, a message, or the keys you can press.
pub fn draw_status(frame: &mut Frame, area: Rect, typed: Option<&str>, message: Option<&str>, hints: &str) {
    let line = if let Some(typed) = typed {
        Line::from(vec![
            Span::styled("Move: ", Style::new().fg(ACCENT)),
            Span::raw(format!("{typed}█")),
            Span::styled("   Enter to play · Esc to cancel", Style::new().fg(DIM)),
        ])
    } else if let Some(message) = message {
        Line::styled(message.to_string(), Style::new().fg(ACCENT))
    } else {
        Line::styled(hints.to_string(), Style::new().fg(DIM))
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// A rounded panel with a title.
pub fn panel(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(DIM))
        .title(Span::styled(format!(" {title} "), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)))
}

pub fn heading(text: impl Into<String>) -> Line<'static> {
    Line::styled(text.into(), Style::new().fg(TEXT).add_modifier(Modifier::BOLD))
}

pub fn dim(text: impl Into<String>) -> Line<'static> {
    Line::styled(text.into(), Style::new().fg(DIM))
}

pub fn colored(text: impl Into<String>, color: Rgb) -> Line<'static> {
    Line::styled(text.into(), Style::new().fg(color))
}

pub fn label_value(label: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<12}"), Style::new().fg(DIM)),
        Span::styled(value.into(), Style::new().fg(TEXT)),
    ])
}

/// Values from 0 to 100 as a row of block characters, squeezed into `width` cells.
pub fn sparkline(values: &[f64], width: usize) -> String {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let cells = values.len().min(width);
    (0..cells)
        .map(|i| {
            let start = i * values.len() / cells;
            let end = ((i + 1) * values.len() / cells).max(start + 1);
            let mean = values[start..end].iter().sum::<f64>() / (end - start) as f64;
            let level = ((mean.clamp(0.0, 100.0) / 100.0) * 7.0).round() as usize;
            BLOCKS[level.min(7)]
        })
        .collect()
}

/// A horizontal bar `width` cells long, filled to `fraction`.
pub fn bar(fraction: f64, width: usize) -> String {
    let filled = (fraction.clamp(0.0, 1.0) * width as f64).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// A rectangle of at most `width` by `height`, centred in `area`.
pub fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(area.x + (area.width - width) / 2, area.y + (area.height - height) / 2, width, height)
}

/// Key help: the same `?` popup as play mode, with this screen's keys.
pub fn draw_help(frame: &mut Frame, keys: &[(&str, &str)]) {
    view::draw_key_help(frame, keys);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sparkline_rises_with_its_values() {
        assert_eq!(sparkline(&[0.0, 50.0, 100.0], 10), "▁▅█");
    }

    #[test]
    fn a_long_sparkline_is_squeezed_into_its_width() {
        let values: Vec<f64> = (0..100).map(f64::from).collect();

        assert_eq!(sparkline(&values, 10).chars().count(), 10);
    }

    #[test]
    fn bars_fill_in_proportion() {
        assert_eq!(bar(0.5, 4), "██░░");
        assert_eq!(bar(2.0, 3), "███");
    }
}
