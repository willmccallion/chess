//! One opening line: a lesson that walks through it with notes, and a drill where you play
//! your side from memory.

use super::super::view::{ACCENT, DIM, board_layout};
use super::common::{
    BAD, BoardKey, BoardPanel, GOOD, INFO, MARK_GOOD, TEXT, colored, dim, draw_board, draw_help,
    draw_status, heading, move_marks, panel, screen_layout,
};
use super::data::{CoachData, now};
use super::{Nav, Screen};
use crate::board::Board;
use crate::coach::repertoire::{CHAPTERS, Line as RepLine, start_board};
use crate::coach::store::TrainingItem;
use crate::notation::numbered;
use crate::types::{Color, Move, Square};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use std::time::{Duration, Instant};

/// The pause before the opponent's move in a drill, so you can see it happen.
const REPLY_DELAY: Duration = Duration::from_millis(500);
const DAY: u64 = 86_400;

const LESSON_KEYS: &[(&str, &str)] = &[
    ("→ l Space", "next move"),
    ("← h", "previous move"),
    ("Enter d", "drill this line"),
    ("f", "flip the board"),
    ("Esc", "back"),
];

const DRILL_KEYS: &[(&str, &str)] = &[
    ("click / drag", "play your move"),
    ("Enter", "type a move"),
    ("h", "show the move (counts as a miss)"),
    ("r", "start the drill again"),
    ("l", "go back to the lesson"),
    ("f", "flip the board"),
    ("Esc", "back"),
];

enum Mode {
    Lesson {
        cursor: usize,
    },
    Drill {
        ply: usize,
        misses: usize,
        shown: bool,
        reply_at: Option<Instant>,
    },
    Done {
        misses: usize,
        saved: Result<(), String>,
    },
}

/// Whether a move list shows the moves still to come.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Upcoming {
    Shown,
    Hidden,
}

pub struct LineScreen {
    line: &'static RepLine,
    moves: Vec<Move>,
    positions: Vec<Board>,
    mode: Mode,
    /// Lines still to drill after this one.
    queue: Vec<&'static RepLine>,
    panel: BoardPanel,
    message: Option<String>,
    help: bool,
}

impl LineScreen {
    pub fn lesson(line: &'static RepLine, queue: Vec<&'static RepLine>) -> Self {
        Self::with_mode(line, queue, Mode::Lesson { cursor: 0 })
    }

    pub fn drill(line: &'static RepLine, queue: Vec<&'static RepLine>) -> Self {
        Self::with_mode(
            line,
            queue,
            Mode::Drill {
                ply: 0,
                misses: 0,
                shown: false,
                reply_at: None,
            },
        )
    }

    fn with_mode(line: &'static RepLine, queue: Vec<&'static RepLine>, mode: Mode) -> Self {
        let moves = line.moves().unwrap_or_default();
        let mut board = start_board();
        let mut positions = vec![board.clone()];
        for &m in &moves {
            board.make_move(m);
            positions.push(board.clone());
        }
        let mut screen = Self {
            panel: BoardPanel::new(start_board(), line.side == Color::Black),
            line,
            moves,
            positions,
            mode,
            queue,
            message: None,
            help: false,
        };
        screen.show(0);
        screen
    }

    fn show(&mut self, ply: usize) {
        let board = self.positions.get(ply).cloned().unwrap_or_else(start_board);
        let last = ply.checked_sub(1).and_then(|i| self.moves.get(i).copied());
        self.panel.set_position(board, last);
    }

    fn start_drill(&mut self) {
        self.mode = Mode::Drill {
            ply: 0,
            misses: 0,
            shown: false,
            reply_at: None,
        };
        self.message = None;
        self.show(0);
    }

    fn start_lesson(&mut self) {
        self.mode = Mode::Lesson { cursor: 0 };
        self.message = None;
        self.show(0);
    }

    fn step_text(&self, ply: usize) -> Option<(String, &'static str)> {
        let step = self.line.steps.get(ply)?;
        let board = self.positions.get(ply)?;
        Some((numbered(board, step.san, true), step.note))
    }

    fn your_turn_in_drill(&self) -> bool {
        matches!(self.mode, Mode::Drill { ply, .. } if ply < self.moves.len() && self.line.is_yours(ply))
    }

    fn play(&mut self, m: Move, data: &mut CoachData) {
        let Mode::Drill { ply, misses, .. } = self.mode else {
            return;
        };
        let Some(&expected) = self.moves.get(ply) else {
            return;
        };
        if m != expected {
            self.message = self
                .step_text(ply)
                .map(|(san, note)| format!("Not this time: the line plays {san}. {note}"));
            self.mode = Mode::Drill {
                ply,
                misses: misses + 1,
                shown: true,
                reply_at: None,
            };
            return;
        }
        self.mode = Mode::Drill {
            ply: ply + 1,
            misses,
            shown: false,
            reply_at: None,
        };
        self.message = None;
        self.show(ply + 1);
        if ply + 1 >= self.moves.len() {
            self.finish(data);
        }
    }

    fn finish(&mut self, data: &mut CoachData) {
        let Mode::Drill { misses, .. } = self.mode else {
            return;
        };
        let saved = data
            .record_attempt(
                TrainingItem::Opening(self.line.id.to_string()),
                misses == 0,
                None,
                None,
            )
            .map_err(|e| format!("Could not save the result: {e}"));
        self.mode = Mode::Done { misses, saved };
    }

    fn marks(&self) -> Vec<(Square, ratatui::style::Color)> {
        match self.mode {
            Mode::Drill {
                ply, shown: true, ..
            } => self
                .moves
                .get(ply)
                .map_or_else(Vec::new, |&m| move_marks(m, MARK_GOOD)),
            _ => Vec::new(),
        }
    }

    fn chapter_name(&self) -> &'static str {
        CHAPTERS
            .iter()
            .find(|c| c.lines.iter().any(|l| l.id == self.line.id))
            .map_or("", |c| c.name)
    }

    /// The line's moves up to `current`, then the rest as `upcoming` says: a
    /// drill hides them so the line is played from memory.
    fn move_list(&self, current: usize, upcoming: Upcoming) -> Line<'static> {
        let mut spans = Vec::new();
        for (ply, step) in self.line.steps.iter().enumerate() {
            if upcoming == Upcoming::Hidden && ply >= current {
                spans.push(Span::styled("…", Style::new().fg(DIM)));
                break;
            }
            if ply.is_multiple_of(2) {
                spans.push(Span::styled(
                    format!("{}.", ply / 2 + 1),
                    Style::new().fg(DIM),
                ));
            }
            let style = if ply + 1 == current {
                Style::new()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else if ply < current {
                Style::new().fg(TEXT)
            } else {
                Style::new().fg(DIM)
            };
            spans.push(Span::styled(step.san.to_string(), style));
            spans.push(Span::raw(" "));
        }
        Line::from(spans)
    }

    fn side_lines(&self, data: &CoachData) -> Vec<Line<'static>> {
        let side = if self.line.side == Color::White {
            "White"
        } else {
            "Black"
        };
        let mut lines = vec![
            heading(self.line.name),
            dim(format!("{} · you play {side}", self.chapter_name())),
            Line::raw(""),
        ];
        lines.extend(match &self.mode {
            Mode::Lesson { cursor } => self.lesson_lines(*cursor),
            Mode::Drill { ply, misses, .. } => self.drill_lines(*ply, *misses),
            Mode::Done { misses, saved } => self.done_lines(*misses, saved, data),
        });
        lines
    }

    /// The moves up to `cursor`, the last one's note, and at the end the plan.
    fn lesson_lines(&self, cursor: usize) -> Vec<Line<'static>> {
        let mut lines = vec![self.move_list(cursor, Upcoming::Shown), Line::raw("")];
        match cursor.checked_sub(1).and_then(|ply| self.step_text(ply)) {
            Some((san, note)) => {
                let yours = self.line.is_yours(cursor - 1);
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{san}  "),
                        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        if yours { "your move" } else { "their move" },
                        Style::new().fg(DIM),
                    ),
                ]));
                lines.push(Line::styled(note.to_string(), Style::new().fg(TEXT)));
            }
            None => lines.push(dim("Press → to go through the line move by move.")),
        }
        if cursor >= self.moves.len() {
            lines.extend(self.plan_lines());
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Enter: drill this line from memory",
                Style::new().fg(ACCENT),
            ));
        }
        lines
    }

    /// The moves played so far, whose turn it is, and the tally.
    fn drill_lines(&self, ply: usize, misses: usize) -> Vec<Line<'static>> {
        let yours_left = (ply..self.moves.len())
            .filter(|&p| self.line.is_yours(p))
            .count();
        let text = if self.your_turn_in_drill() {
            "Your move: play the line from memory."
        } else {
            "Their move…"
        };
        let mut lines = vec![
            self.move_list(ply, Upcoming::Hidden),
            Line::raw(""),
            heading(text),
            dim(format!(
                "{yours_left} of your moves to go · {misses} missed"
            )),
        ];
        if let Some(message) = &self.message {
            lines.push(Line::raw(""));
            lines.push(colored(message.clone(), BAD));
        }
        lines
    }

    /// How the drill went, when the line comes back, and the plan.
    fn done_lines(
        &self,
        misses: usize,
        saved: &Result<(), String>,
        data: &CoachData,
    ) -> Vec<Line<'static>> {
        let mut lines = vec![
            self.move_list(self.moves.len(), Upcoming::Shown),
            Line::raw(""),
        ];
        if misses == 0 {
            lines.push(Line::styled(
                "Perfect! Every move from memory.",
                Style::new().fg(GOOD).add_modifier(Modifier::BOLD),
            ));
        } else {
            lines.push(colored(
                format!(
                    "Done, with {misses} miss{}.",
                    if misses == 1 { "" } else { "es" }
                ),
                BAD,
            ));
            lines.push(dim("It will come back soon so you can try again."));
        }
        if let Some(schedule) = data.schedule(&TrainingItem::Opening(self.line.id.to_string())) {
            let wait = schedule.due.0.saturating_sub(now().0);
            let when = if wait >= DAY {
                format!("in {} days", wait.div_ceil(DAY))
            } else {
                "later today".to_string()
            };
            lines.push(colored(format!("Next review {when}."), INFO));
        }
        if let Err(e) = saved {
            lines.push(colored(e.clone(), BAD));
        }
        lines.extend(self.plan_lines());
        lines.push(Line::raw(""));
        let next = if self.queue.is_empty() {
            "Enter: back"
        } else {
            "Enter: next line"
        };
        lines.push(Line::styled(
            format!("{next}   r: drill again   l: lesson"),
            Style::new().fg(ACCENT),
        ));
        lines
    }

    /// The plan the line leads to, under a blank line.
    fn plan_lines(&self) -> [Line<'static>; 3] {
        [
            Line::raw(""),
            heading("The plan from here"),
            Line::styled(self.line.plan.to_string(), Style::new().fg(TEXT)),
        ]
    }

    /// Plays or rejects a move the board makes of `key` in a drill; false
    /// when the board does not take the key.
    fn board_takes(&mut self, key: KeyEvent, data: &mut CoachData) -> bool {
        match self.panel.key(key, self.your_turn_in_drill()) {
            BoardKey::Move(m) => self.play(m, data),
            BoardKey::Invalid(text) => self.message = Some(text),
            BoardKey::Handled => {}
            BoardKey::NotHandled => return false,
        }
        true
    }

    /// Steps through the lesson, or starts the drill.
    fn lesson_key(&mut self, key: KeyEvent) {
        let Mode::Lesson { cursor } = &mut self.mode else {
            return;
        };
        let shown = match key.code {
            KeyCode::Right | KeyCode::Char('l' | ' ') => (*cursor + 1).min(self.moves.len()),
            KeyCode::Left | KeyCode::Char('h') => cursor.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('d') => return self.start_drill(),
            _ => return,
        };
        *cursor = shown;
        self.show(shown);
    }

    /// Shows the next move of the line as a hint, or restarts.
    fn drill_key(&mut self, key: KeyEvent) {
        let Mode::Drill {
            ply, misses, shown, ..
        } = &mut self.mode
        else {
            return;
        };
        match key.code {
            KeyCode::Char('h') if self.line.is_yours(*ply) && !*shown => {
                *misses += 1;
                *shown = true;
                let ply = *ply;
                self.message = self
                    .step_text(ply)
                    .map(|(san, note)| format!("The line plays {san}. {note}"));
            }
            KeyCode::Char('r') => self.start_drill(),
            KeyCode::Char('l') => self.start_lesson(),
            _ => {}
        }
    }

    /// Moves on to the next queued line, or goes over this one again.
    fn done_key(&mut self, key: KeyEvent) -> Nav {
        match key.code {
            KeyCode::Enter if self.queue.is_empty() => return Nav::Back,
            KeyCode::Enter => {
                let next = self.queue.remove(0);
                let rest = std::mem::take(&mut self.queue);
                return Nav::Replace(Box::new(Self::drill(next, rest)));
            }
            KeyCode::Char('r') => self.start_drill(),
            KeyCode::Char('l') => self.start_lesson(),
            _ => {}
        }
        Nav::Stay
    }
}

impl Screen for LineScreen {
    fn draw(&self, frame: &mut Frame, data: &CoachData, _now: Instant) {
        let layout = screen_layout(frame.area());
        draw_board(
            frame,
            &layout,
            &self.panel.scene(data.piece_style, self.marks()),
        );
        let title = match self.mode {
            Mode::Lesson { .. } => "Lesson",
            Mode::Drill { .. } | Mode::Done { .. } => "Drill",
        };
        frame.render_widget(
            Paragraph::new(self.side_lines(data))
                .wrap(Wrap { trim: false })
                .block(panel(title)),
            layout.side,
        );
        let hints = match self.mode {
            Mode::Lesson { .. } => "←→ step through · Enter drill it · f flip · ? help · Esc back",
            Mode::Drill { .. } => {
                "play your moves · h show move · r restart · l lesson · ? help · Esc back"
            }
            Mode::Done { .. } => "Enter continue · r drill again · l lesson · Esc back",
        };
        draw_status(
            frame,
            layout.status,
            self.panel.typed.as_deref(),
            None,
            hints,
        );
        if self.help {
            let keys = if matches!(self.mode, Mode::Lesson { .. }) {
                LESSON_KEYS
            } else {
                DRILL_KEYS
            };
            draw_help(frame, keys);
        }
    }

    fn key(&mut self, key: KeyEvent, data: &mut CoachData, _now: Instant) -> Nav {
        if self.help {
            self.help = false;
            return Nav::Stay;
        }
        if matches!(self.mode, Mode::Drill { .. }) && self.board_takes(key, data) {
            return Nav::Stay;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Nav::Back,
            KeyCode::Char('f') => self.panel.flipped = !self.panel.flipped,
            KeyCode::Char('p') => data.piece_style = data.piece_style.next(),
            KeyCode::Char('?') => self.help = true,
            _ => {}
        }
        match self.mode {
            Mode::Lesson { .. } => self.lesson_key(key),
            Mode::Drill { .. } => self.drill_key(key),
            Mode::Done { .. } => return self.done_key(key),
        }
        Nav::Stay
    }

    fn mouse(&mut self, mouse: MouseEvent, area: Rect, data: &mut CoachData, _now: Instant) -> Nav {
        let geometry = board_layout(area, false).board;
        if let Some(m) = self.panel.mouse(mouse, geometry, self.your_turn_in_drill()) {
            self.play(m, data);
        }
        Nav::Stay
    }

    fn tick(&mut self, data: &mut CoachData, now: Instant) -> Nav {
        let Mode::Drill { ply, reply_at, .. } = &mut self.mode else {
            return Nav::Stay;
        };
        if *ply >= self.moves.len() || self.line.is_yours(*ply) {
            return Nav::Stay;
        }
        match *reply_at {
            None => *reply_at = Some(now + REPLY_DELAY),
            Some(at) if now >= at => {
                *ply += 1;
                *reply_at = None;
                let ply = *ply;
                self.show(ply);
                if ply >= self.moves.len() {
                    self.finish(data);
                }
            }
            Some(_) => {}
        }
        Nav::Stay
    }
}
