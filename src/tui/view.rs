use super::app::{App, EngineLine, GameEnd, Mode, PieceStyle, TaskKind, TimeSetting};
use super::sprites::{self, Pixel};
use crate::game_runner::GameResult;
use crate::search::Score;
use crate::types::{Color, Piece, PieceKind};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color as Rgb, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use std::time::Instant;

const RANK_LABEL_WIDTH: u16 = 2;
const EVAL_BAR_WIDTH: u16 = 2;
const GAP: u16 = 1;
const SIDE_MIN_WIDTH: u16 = 30;
const SIDE_MAX_WIDTH: u16 = 52;
const PLAYERS_HEIGHT: u16 = 6;
const ENGINE_HEIGHT: u16 = 8;

const LIGHT_SQUARE: Rgb = Rgb::Rgb(196, 164, 120);
const DARK_SQUARE: Rgb = Rgb::Rgb(150, 111, 75);
const LAST_MOVE_LIGHT: Rgb = Rgb::Rgb(205, 196, 110);
const LAST_MOVE_DARK: Rgb = Rgb::Rgb(166, 150, 62);
const SELECTED: Rgb = Rgb::Rgb(110, 150, 190);
const CAPTURE_TARGET: Rgb = Rgb::Rgb(200, 110, 90);
const CHECK: Rgb = Rgb::Rgb(220, 70, 60);
const TARGET_DOT: Rgb = Rgb::Rgb(90, 120, 150);
const WHITE_PIECE: Rgb = Rgb::Rgb(255, 255, 255);
const BLACK_PIECE: Rgb = Rgb::Rgb(15, 15, 15);
const WHITE_SPRITE_FILL: Rgb = Rgb::Rgb(245, 245, 240);
const WHITE_SPRITE_OUTLINE: Rgb = Rgb::Rgb(35, 35, 35);
const BLACK_SPRITE_FILL: Rgb = Rgb::Rgb(45, 45, 45);
const BLACK_SPRITE_OUTLINE: Rgb = Rgb::Rgb(0, 0, 0);
/// Squares shorter than this are too coarse for pixel-art pieces.
const MIN_SPRITE_SQUARE_HEIGHT: u16 = 4;
const BAR_WHITE: Rgb = Rgb::Rgb(235, 235, 235);
const BAR_BLACK: Rgb = Rgb::Rgb(55, 55, 55);
const LABEL: Rgb = Rgb::Gray;
const DIM: Rgb = Rgb::DarkGray;
const ACCENT: Rgb = Rgb::Yellow;

/// The order promotion choices are shown in.
pub const PROMOTION_KINDS: [PieceKind; 4] =
    [PieceKind::Queen, PieceKind::Rook, PieceKind::Bishop, PieceKind::Knight];

/// Where the squares are on screen; labels sit outside this area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoardGeometry {
    pub x: u16,
    pub y: u16,
    pub square_width: u16,
    pub square_height: u16,
}

impl BoardGeometry {
    fn width(self) -> u16 {
        8 * self.square_width
    }

    fn height(self) -> u16 {
        8 * self.square_height
    }

    fn square_rect(self, square: u8, flipped: bool) -> Rect {
        let (file, rank) = (u16::from(square % 8), u16::from(square / 8));
        let (column, row) = if flipped { (7 - file, rank) } else { (file, 7 - rank) };
        Rect::new(
            self.x + column * self.square_width,
            self.y + row * self.square_height,
            self.square_width,
            self.square_height,
        )
    }

    /// The square under a screen cell.
    pub fn square_at(self, column: u16, row: u16, flipped: bool) -> Option<u8> {
        let column = column.checked_sub(self.x)? / self.square_width;
        let row = row.checked_sub(self.y)? / self.square_height;
        if column >= 8 || row >= 8 {
            return None;
        }
        let (file, rank) = if flipped { (7 - column, row) } else { (column, 7 - row) };
        Some((rank * 8 + file) as u8)
    }

    fn promotion_rects(self) -> [Rect; 4] {
        let left = self.x + 2 * self.square_width;
        let top = self.y + self.height() / 2 - self.square_height / 2;
        std::array::from_fn(|i| {
            Rect::new(left + i as u16 * self.square_width, top, self.square_width, self.square_height)
        })
    }

    /// The promotion piece under a screen cell while the choice is open.
    pub fn promotion_at(self, column: u16, row: u16) -> Option<PieceKind> {
        let position = (column, row).into();
        self.promotion_rects()
            .iter()
            .position(|rect| rect.contains(position))
            .map(|i| PROMOTION_KINDS[i])
    }
}

pub struct ScreenLayout {
    /// None when the terminal is too small to draw a board.
    pub board: Option<BoardGeometry>,
    eval_bar: Option<Rect>,
    side: Rect,
    status: Rect,
}

/// Splits the screen; used both to draw and to find what a mouse click hit.
pub fn layout(area: Rect, app: &App) -> ScreenLayout {
    let status = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, area.height.min(1));
    let main = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
    let bar_width = if app.shows_engine_output() { EVAL_BAR_WIDTH + GAP } else { 0 };
    let available_width = main.width.saturating_sub(bar_width + RANK_LABEL_WIDTH + GAP + SIDE_MIN_WIDTH);
    let available_height = main.height.saturating_sub(1);
    let square_height = (available_height / 8).min((available_width / 8).saturating_sub(1) / 2);
    if square_height == 0 {
        return ScreenLayout {
            board: None,
            eval_bar: None,
            side: main,
            status,
        };
    }
    let square_width = 2 * square_height + 1;
    let board_height = 8 * square_height + 1;
    let top = main.y + (main.height - board_height) / 2;
    let board = BoardGeometry {
        x: main.x + bar_width + RANK_LABEL_WIDTH,
        y: top,
        square_width,
        square_height,
    };
    let side_x = board.x + board.width() + 2 * GAP;
    ScreenLayout {
        board: Some(board),
        eval_bar: (bar_width > 0).then(|| Rect::new(main.x, top, EVAL_BAR_WIDTH, board.height())),
        side: Rect::new(side_x, main.y, main.right().saturating_sub(side_x).min(SIDE_MAX_WIDTH), main.height),
        status,
    }
}

pub fn draw(frame: &mut Frame, app: &App, now: Instant) {
    let screen = layout(frame.area(), app);
    match screen.board {
        Some(board) => {
            draw_board(frame.buffer_mut(), app, board);
            if let Some(bar) = screen.eval_bar {
                draw_eval_bar(frame.buffer_mut(), app, bar);
            }
        }
        None => frame.render_widget(
            Paragraph::new("Make the terminal larger to see the board").style(Style::new().fg(ACCENT)),
            screen.side,
        ),
    }
    draw_side_panel(frame, app, screen.side, now);
    frame.render_widget(status_line(app), screen.status);
    if let (Some(board), Some(_)) = (screen.board, app.pending_promotion()) {
        draw_promotion_choice(frame, app, board);
    }
    if app.panels.help {
        draw_help(frame);
    }
}

fn draw_board(buf: &mut Buffer, app: &App, geometry: BoardGeometry) {
    let board = app.board();
    let targets = app.targets();
    let last_move = app.last_move();
    let checked_king = in_check_king_square(app);
    for square in 0..64u8 {
        let piece = board.piece_on[square as usize];
        let is_light = (square / 8 + square % 8) % 2 == 1;
        let background = if Some(square) == app.selected() {
            SELECTED
        } else if Some(square) == checked_king {
            CHECK
        } else if targets.contains(&square) && piece != Piece::Empty {
            CAPTURE_TARGET
        } else if last_move.is_some_and(|m| m.from == square || m.to == square) {
            if is_light { LAST_MOVE_LIGHT } else { LAST_MOVE_DARK }
        } else if is_light {
            LIGHT_SQUARE
        } else {
            DARK_SQUARE
        };
        let rect = geometry.square_rect(square, app.flipped);
        buf.set_style(rect, Style::new().bg(background));
        if piece != Piece::Empty {
            draw_piece(buf, rect, piece, app.piece_style, background);
        } else if targets.contains(&square) {
            draw_target_dot(buf, rect, background);
        }
    }
    draw_coordinates(buf, app.flipped, geometry);
}

fn draw_coordinates(buf: &mut Buffer, flipped: bool, geometry: BoardGeometry) {
    let label = Style::new().fg(LABEL);
    for i in 0..8u8 {
        let rank_square = geometry.square_rect(i * 8, flipped);
        let rank_row = rank_square.y + rank_square.height / 2;
        buf.set_string(geometry.x - RANK_LABEL_WIDTH, rank_row, ((b'1' + i) as char).to_string(), label);
        let file_square = geometry.square_rect(i, flipped);
        let file_column = file_square.x + file_square.width / 2;
        buf.set_string(file_column, geometry.y + geometry.height(), ((b'a' + i) as char).to_string(), label);
    }
}

fn in_check_king_square(app: &App) -> Option<u8> {
    let board = app.board();
    let king = board.king_square(board.turn);
    (king < 64 && board.is_square_attacked(king as i32, board.turn.other())).then_some(king as u8)
}

/// A dot a third of the square across, centred, drawn in half-block pixels.
fn draw_target_dot(buf: &mut Buffer, rect: Rect, background: Rgb) {
    let (width, height) = (rect.width, 2 * rect.height);
    let dot_width = centred_length(width / 3, width);
    let dot_height = centred_length(height / 3, height);
    let (left, top) = ((width - dot_width) / 2, (height - dot_height) / 2);
    let in_dot = |x: u16, y: u16| (left..left + dot_width).contains(&x) && (top..top + dot_height).contains(&y);
    for row in 0..rect.height {
        for column in 0..rect.width {
            let paint = |y: u16| if in_dot(column, y) { TARGET_DOT } else { background };
            if let Some(cell) = buf.cell_mut((rect.x + column, rect.y + row)) {
                cell.set_symbol("▀").set_fg(paint(2 * row)).set_bg(paint(2 * row + 1));
            }
        }
    }
}

/// `length` (at least 1) adjusted to the parity of `total`, so it centres exactly.
fn centred_length(length: u16, total: u16) -> u16 {
    let length = length.max(1);
    if (total - length).is_multiple_of(2) { length } else { length + 1 }
}

/// Draws `piece` filling `rect`, whose empty parts show `background`.
fn draw_piece(buf: &mut Buffer, rect: Rect, piece: Piece, style: PieceStyle, background: Rgb) {
    let (Some(color), Some(kind)) = (piece.color(), piece.kind()) else {
        return;
    };
    if style == PieceStyle::Sprites && rect.height >= MIN_SPRITE_SQUARE_HEIGHT {
        draw_sprite(buf, rect, kind, color, background);
        return;
    }
    let style = if style == PieceStyle::Letters { PieceStyle::Letters } else { PieceStyle::Glyphs };
    if let Some((symbol, cell_style)) = piece_symbol(piece, style)
        && let Some(cell) = buf.cell_mut((rect.x + rect.width / 2, rect.y + rect.height / 2))
    {
        cell.set_symbol(symbol).set_style(cell_style);
    }
}

/// Two pixels per cell: the upper half block in the top pixel's colour over the bottom's.
fn draw_sprite(buf: &mut Buffer, rect: Rect, kind: PieceKind, color: Color, background: Rgb) {
    let (fill, outline) = match color {
        Color::White => (WHITE_SPRITE_FILL, WHITE_SPRITE_OUTLINE),
        Color::Black => (BLACK_SPRITE_FILL, BLACK_SPRITE_OUTLINE),
    };
    let paint = |pixel: Pixel| match pixel {
        Pixel::Empty => background,
        Pixel::Fill => fill,
        Pixel::Outline => outline,
    };
    let pixels = sprites::render(kind, usize::from(rect.width), 2 * usize::from(rect.height));
    for (row, pair) in pixels.chunks(2).enumerate() {
        for column in 0..rect.width {
            let top = paint(pair[0][usize::from(column)]);
            let bottom = paint(pair[1][usize::from(column)]);
            if let Some(cell) = buf.cell_mut((rect.x + column, rect.y + row as u16)) {
                cell.set_symbol("▀").set_fg(top).set_bg(bottom);
            }
        }
    }
}

fn piece_symbol(piece: Piece, style: PieceStyle) -> Option<(&'static str, Style)> {
    let color = piece.color()?;
    let kind = piece.kind()?;
    let symbol = match (style, kind) {
        (PieceStyle::Glyphs | PieceStyle::Sprites, PieceKind::King) => "♚",
        (PieceStyle::Glyphs | PieceStyle::Sprites, PieceKind::Queen) => "♛",
        (PieceStyle::Glyphs | PieceStyle::Sprites, PieceKind::Rook) => "♜",
        (PieceStyle::Glyphs | PieceStyle::Sprites, PieceKind::Bishop) => "♝",
        (PieceStyle::Glyphs | PieceStyle::Sprites, PieceKind::Knight) => "♞",
        (PieceStyle::Glyphs | PieceStyle::Sprites, PieceKind::Pawn) => "♟",
        (PieceStyle::Letters, PieceKind::King) => "K",
        (PieceStyle::Letters, PieceKind::Queen) => "Q",
        (PieceStyle::Letters, PieceKind::Rook) => "R",
        (PieceStyle::Letters, PieceKind::Bishop) => "B",
        (PieceStyle::Letters, PieceKind::Knight) => "N",
        (PieceStyle::Letters, PieceKind::Pawn) => "P",
    };
    let foreground = if color == Color::White { WHITE_PIECE } else { BLACK_PIECE };
    Some((symbol, Style::new().fg(foreground).add_modifier(Modifier::BOLD)))
}

/// White's share of the bar, from the expected score of the evaluation.
fn white_share(score: Score) -> f64 {
    match score {
        Score::MateIn(moves) => {
            if moves >= 0 {
                1.0
            } else {
                0.0
            }
        }
        Score::Centipawns(cp) => 1.0 / (1.0 + (-0.003_682_08 * f64::from(cp)).exp()),
    }
}

fn draw_eval_bar(buf: &mut Buffer, app: &App, bar: Rect) {
    let share = app.engine_line().map_or(0.5, |line| white_share(line.score));
    let white_rows = (share * f64::from(bar.height)).round() as u16;
    let (white, black) = if app.flipped {
        (Rect::new(bar.x, bar.y, bar.width, white_rows), Rect::new(bar.x, bar.y + white_rows, bar.width, bar.height - white_rows))
    } else {
        let black_rows = bar.height - white_rows;
        (Rect::new(bar.x, bar.y + black_rows, bar.width, white_rows), Rect::new(bar.x, bar.y, bar.width, black_rows))
    };
    buf.set_style(white, Style::new().bg(BAR_WHITE));
    buf.set_style(black, Style::new().bg(BAR_BLACK));
}

fn draw_side_panel(frame: &mut Frame, app: &App, area: Rect, now: Instant) {
    let players_height = PLAYERS_HEIGHT.min(area.height);
    let players = Rect::new(area.x, area.y, area.width, players_height);
    frame.render_widget(players_panel(app, now), players);
    let mut top = players.bottom();
    if app.panels.eval {
        let height = ENGINE_HEIGHT.min(area.bottom() - top);
        frame.render_widget(engine_panel(app), Rect::new(area.x, top, area.width, height));
        top += height;
    }
    if app.panels.moves && top < area.bottom() {
        let moves = Rect::new(area.x, top, area.width, area.bottom() - top);
        frame.render_widget(moves_panel(app, moves.height.saturating_sub(2)), moves);
    }
}

fn player_name(app: &App, color: Color) -> &'static str {
    if app.mode() != Mode::Watch && color == app.human() { "You" } else { "Engine" }
}

fn players_panel(app: &App, now: Instant) -> Paragraph<'static> {
    let board = app.board();
    let player_line = |color: Color| {
        let to_move = board.turn == color && app.outcome().is_none();
        let marker = if to_move { "▶ " } else { "  " };
        let side = if color == Color::White { "White" } else { "Black" };
        let mut spans = vec![
            Span::styled(marker, Style::new().fg(ACCENT)),
            Span::raw(format!("{side:<6} {:<7}", player_name(app, color))),
        ];
        if let Some(clocks) = app.clocks() {
            let ms = clocks.remaining_ms(color, board.turn, now).max(0);
            let style = if to_move { Style::new().fg(ACCENT).add_modifier(Modifier::BOLD) } else { Style::new() };
            spans.push(Span::styled(format_clock(ms), style));
        }
        Line::from(spans)
    };
    let lines = vec![
        player_line(Color::White),
        player_line(Color::Black),
        Line::styled(mode_text(app), Style::new().fg(DIM)),
        game_state_line(app),
    ];
    Paragraph::new(lines).block(Block::bordered().title(" chess "))
}

fn mode_text(app: &App) -> String {
    let mode = match app.mode() {
        Mode::Casual => "Casual",
        Mode::Game => "Game (no help)",
        Mode::Watch => "Watching",
    };
    let time = match app.time_setting() {
        TimeSetting::PerMove { ms } => format!("engine {:.1}s/move", ms as f64 / 1000.0),
        TimeSetting::Clock(clock) => format!(
            "{}+{}",
            format_minutes(clock.base_ms),
            clock.increment_ms as f64 / 1000.0
        ),
    };
    format!("{mode} · {time}")
}

fn format_minutes(ms: u64) -> String {
    let minutes = ms as f64 / 60_000.0;
    if minutes.fract() == 0.0 { format!("{minutes:.0}") } else { format!("{minutes:.1}") }
}

fn game_state_line(app: &App) -> Line<'static> {
    if let Some(end) = app.outcome() {
        return Line::styled(result_text(end), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD));
    }
    let text = if app.is_paused() {
        "Paused"
    } else if app.pending_promotion().is_some() {
        "Choose a promotion piece"
    } else if app.is_engine_turn() {
        "Engine is thinking…"
    } else {
        "Your move"
    };
    Line::raw(text)
}

fn result_text(end: GameEnd) -> String {
    let winner = match end.result {
        GameResult::WhiteWin => "White wins",
        GameResult::BlackWin => "Black wins",
        GameResult::Draw => "Draw",
    };
    format!("{winner} by {}", end.reason)
}

fn engine_panel(app: &App) -> Paragraph<'static> {
    let block = Block::bordered().title(" Engine ");
    if !app.assistance_allowed() {
        return Paragraph::new(Line::styled("Hidden until the game ends", Style::new().fg(DIM))).block(block);
    }
    let Some(line) = app.engine_line() else {
        let idle = if app.engine_task().is_some() {
            "Searching…"
        } else if app.mode() == Mode::Watch {
            "Idle"
        } else {
            "Analysis is off (a to turn on)"
        };
        return Paragraph::new(Line::styled(idle, Style::new().fg(DIM))).block(block);
    };
    Paragraph::new(engine_lines(line)).wrap(Wrap { trim: true }).block(block)
}

fn engine_lines(line: &EngineLine) -> Vec<Line<'static>> {
    let source = match line.source {
        TaskKind::Think => "thinking",
        TaskKind::Analyse => "analysis",
    };
    vec![
        Line::from(vec![
            Span::styled(format_score(line.score), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {source}"), Style::new().fg(DIM)),
        ]),
        Line::styled(
            format!(
                "depth {}/{}  {} nodes  {} nps",
                line.depth,
                line.seldepth,
                format_count(line.nodes),
                format_count(line.nps)
            ),
            Style::new().fg(DIM),
        ),
        Line::raw(line.pv_san.clone()),
    ]
}

fn moves_panel(app: &App, visible_rows: u16) -> Paragraph<'static> {
    let start = app.start_position();
    let mut rows = Vec::new();
    let mut number = start.fullmove_number;
    let mut moves = app.history().iter().map(|(_, san)| san.as_str());
    if start.turn == Color::Black
        && let Some(first) = moves.next()
    {
        rows.push(format!("{number:>3}. {:<8} {first}", "..."));
        number += 1;
    }
    let rest: Vec<&str> = moves.collect();
    for pair in rest.chunks(2) {
        let black = pair.get(1).copied().unwrap_or("");
        rows.push(format!("{number:>3}. {:<8} {black}", pair[0]));
        number += 1;
    }
    let skip = rows.len().saturating_sub(usize::from(visible_rows));
    let lines: Vec<Line> = rows.into_iter().skip(skip).map(Line::raw).collect();
    Paragraph::new(lines).block(Block::bordered().title(" Moves "))
}

fn status_line(app: &App) -> Paragraph<'static> {
    if let Some(typed) = &app.typed {
        return Paragraph::new(Line::from(vec![
            Span::styled("Move: ", Style::new().fg(ACCENT)),
            Span::raw(format!("{typed}█")),
            Span::styled("   Enter to play · Esc to cancel", Style::new().fg(DIM)),
        ]));
    }
    if let Some(message) = &app.message {
        return Paragraph::new(Line::styled(message.clone(), Style::new().fg(ACCENT)));
    }
    let hints = match app.mode() {
        Mode::Casual => "click or drag to move · Enter type a move · u undo · a analysis · e engine · l moves · f flip · ? help · q quit",
        Mode::Game => "click or drag to move · Enter type a move · f flip · n new game · s swap sides · ? help · q quit",
        Mode::Watch => "space pause · e engine · l moves · f flip · n new game · ? help · q quit",
    };
    Paragraph::new(Line::styled(hints, Style::new().fg(DIM)))
}

fn draw_promotion_choice(frame: &mut Frame, app: &App, geometry: BoardGeometry) {
    let rects = geometry.promotion_rects();
    let outline = Rect::new(rects[0].x - 1, rects[0].y - 1, 4 * geometry.square_width + 2, geometry.square_height + 2);
    frame.render_widget(Clear, outline);
    frame.render_widget(Block::bordered().title(" Promote "), outline);
    let color = app.board().turn;
    let buf = frame.buffer_mut();
    for (rect, kind) in rects.into_iter().zip(PROMOTION_KINDS) {
        buf.set_style(rect, Style::new().bg(LIGHT_SQUARE));
        draw_piece(buf, rect, Piece::from_kind(kind, color), app.piece_style, LIGHT_SQUARE);
    }
}

const HELP: &[(&str, &str)] = &[
    ("click / drag", "select and move a piece"),
    ("Enter", "type a move (e2e4, Nf3, O-O)"),
    ("q r b n", "choose a promotion piece"),
    ("u", "take back your last move (casual)"),
    ("a", "analyse on your turn (casual)"),
    ("e", "show or hide the engine panel"),
    ("l", "show or hide the move list"),
    ("f", "flip the board"),
    ("p", "pieces as pictures, symbols or letters"),
    ("space", "pause (watching)"),
    ("n", "new game"),
    ("s", "swap sides and start again"),
    ("Esc", "cancel / close"),
    ("q", "quit"),
];

fn draw_help(frame: &mut Frame) {
    let area = frame.area();
    let width = 52.min(area.width);
    let height = (HELP.len() as u16 + 2).min(area.height);
    let popup = Rect::new(area.x + (area.width - width) / 2, area.y + (area.height - height) / 2, width, height);
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(key, action)| {
            Line::from(vec![Span::styled(format!("{key:>13}  "), Style::new().fg(ACCENT)), Span::raw(*action)])
        })
        .collect();
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(Block::bordered().title(" Keys ")), popup);
}

fn format_score(score: Score) -> String {
    match score {
        Score::Centipawns(cp) => format!("{:+.2}", f64::from(cp) / 100.0),
        Score::MateIn(moves) if moves >= 0 => format!("#{moves}"),
        Score::MateIn(moves) => format!("#-{}", -moves),
    }
}

fn format_count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

fn format_clock(ms: i64) -> String {
    let tenths = ms / 100;
    let seconds = tenths / 10;
    if seconds < 20 {
        format!("{}:{:02}.{}", seconds / 60, seconds % 60, tenths % 10)
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;
    use crate::tui::app::PlayConfig;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    const GEOMETRY: BoardGeometry = BoardGeometry {
        x: 10,
        y: 2,
        square_width: 5,
        square_height: 2,
    };

    #[test]
    fn every_square_is_found_where_it_is_drawn() {
        for flipped in [false, true] {
            for square in 0..64u8 {
                let rect = GEOMETRY.square_rect(square, flipped);
                let corners = [(rect.x, rect.y), (rect.right() - 1, rect.bottom() - 1)];
                for (column, row) in corners {
                    assert_eq!(GEOMETRY.square_at(column, row, flipped), Some(square));
                }
            }
        }
    }

    #[test]
    fn a1_is_bottom_left_unless_flipped() {
        let bottom_left = (GEOMETRY.x, GEOMETRY.y + GEOMETRY.height() - 1);

        assert_eq!(GEOMETRY.square_at(bottom_left.0, bottom_left.1, false), Some(0));
        assert_eq!(GEOMETRY.square_at(bottom_left.0, bottom_left.1, true), Some(63));
    }

    #[test]
    fn cells_outside_the_board_are_no_square() {
        assert_eq!(GEOMETRY.square_at(GEOMETRY.x - 1, GEOMETRY.y, false), None);
        assert_eq!(GEOMETRY.square_at(GEOMETRY.x + GEOMETRY.width(), GEOMETRY.y, false), None);
        assert_eq!(GEOMETRY.square_at(GEOMETRY.x, GEOMETRY.y + GEOMETRY.height(), false), None);
    }

    #[test]
    fn promotion_choices_are_hit_where_they_are_drawn() {
        for (rect, kind) in GEOMETRY.promotion_rects().into_iter().zip(PROMOTION_KINDS) {
            assert_eq!(GEOMETRY.promotion_at(rect.x, rect.y), Some(kind));
        }
        assert_eq!(GEOMETRY.promotion_at(GEOMETRY.x, GEOMETRY.y), None);
    }

    #[test]
    fn scores_and_clocks_read_naturally() {
        assert_eq!(format_score(Score::Centipawns(35)), "+0.35");
        assert_eq!(format_score(Score::Centipawns(-120)), "-1.20");
        assert_eq!(format_score(Score::MateIn(3)), "#3");
        assert_eq!(format_score(Score::MateIn(-2)), "#-2");
        assert_eq!(format_clock(305_000), "5:05");
        assert_eq!(format_clock(9_400), "0:09.4");
    }

    #[test]
    fn the_bar_favours_the_side_ahead() {
        assert!((white_share(Score::Centipawns(0)) - 0.5).abs() < 1e-9);
        assert!(white_share(Score::Centipawns(300)) > 0.7);
        assert_eq!(white_share(Score::MateIn(-1)), 0.0);
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app, Instant::now())).unwrap();
        let buffer = terminal.backend().buffer();
        buffer.content().iter().map(|cell| cell.symbol()).collect()
    }

    fn app_with(mode: Mode, fen: &str) -> App {
        let config = PlayConfig {
            start: Board::from_fen(fen).unwrap(),
            mode,
            human: Color::White,
            time: TimeSetting::Clock(crate::game_runner::GameClock { base_ms: 60_000, increment_ms: 0 }),
        };
        App::new(config, Instant::now())
    }

    #[test]
    fn draws_at_any_terminal_size() {
        let mut promoting = app_with(Mode::Casual, "8/4P3/8/8/8/8/k7/4K3 w - - 0 1");
        promoting.press_square(52, Instant::now());
        promoting.release_square(60, Instant::now());
        let mut help = app_with(Mode::Watch, crate::types::START_FEN);
        help.panels.help = true;
        let apps = [app_with(Mode::Game, crate::types::START_FEN), promoting, help];
        for app in &apps {
            for (width, height) in [(1, 1), (20, 6), (40, 12), (80, 24), (120, 40), (250, 80)] {
                render(app, width, height);
            }
        }
    }

    #[test]
    fn game_mode_shows_no_eval_bar_or_engine_output() {
        let app = app_with(Mode::Game, crate::types::START_FEN);

        let screen = render(&app, 120, 40);

        assert!(screen.contains("Hidden until the game ends"));
        assert!(layout(Rect::new(0, 0, 120, 40), &app).eval_bar.is_none());
    }
}
