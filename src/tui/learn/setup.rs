//! Choosing how the next game against the coach is played.

use super::super::view::{ACCENT, DIM};
use super::common::{TEXT, centred, dim, panel};
use super::data::CoachData;
use super::game::{CoachGame, GameSettings};
use super::{Nav, Screen};
use crate::coach::bot::Level;
use crate::coach::repertoire::{CHAPTERS, Chapter};
use crate::game_runner::GameClock;
use crate::types::Color;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use std::time::Instant;

/// Ratings below this get the blunder check by default.
const BLUNDER_CHECK_BELOW: i32 = 1000;
const FORM_WIDTH: u16 = 64;

const CLOCKS: [Option<(u64, u64)>; 5] = [
    None,
    Some((10, 5)),
    Some((15, 10)),
    Some((30, 0)),
    Some((5, 3)),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SideChoice {
    White,
    Black,
    Random,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Level,
    Side,
    Clock,
    BlunderCheck,
    Opening,
    Start,
}

const ROWS: [Row; 6] = [
    Row::Level,
    Row::Side,
    Row::Clock,
    Row::BlunderCheck,
    Row::Opening,
    Row::Start,
];

pub struct Setup {
    row: usize,
    /// None plays the level that matches your rating.
    level: Option<Level>,
    side: SideChoice,
    clock: usize,
    blunder_check: bool,
    /// Index into [`CHAPTERS`] of the opening the coach steers towards, if any.
    opening: Option<usize>,
}

impl Setup {
    pub fn new(data: &CoachData) -> Self {
        Self {
            row: ROWS.len() - 1,
            level: None,
            side: SideChoice::White,
            clock: 0,
            blunder_check: data.profile.rating.0 < BLUNDER_CHECK_BELOW,
            opening: None,
        }
    }

    fn change(&mut self, forward: bool) {
        match ROWS[self.row] {
            Row::Level => {
                self.level = match (self.level, forward) {
                    (None, true) => Some(Level::WEAKEST),
                    (None, false) => Some(Level::STRONGEST),
                    (Some(level), true) if level == Level::STRONGEST => None,
                    (Some(level), false) if level == Level::WEAKEST => None,
                    (Some(level), true) => Some(level.harder()),
                    (Some(level), false) => Some(level.easier()),
                };
            }
            Row::Side => {
                self.side = match (self.side, forward) {
                    (SideChoice::White, true) | (SideChoice::Random, false) => SideChoice::Black,
                    (SideChoice::Black, true) | (SideChoice::White, false) => SideChoice::Random,
                    (SideChoice::Random, true) | (SideChoice::Black, false) => SideChoice::White,
                };
                self.opening = None;
            }
            Row::Clock => self.clock = cycle(self.clock, CLOCKS.len(), forward),
            Row::BlunderCheck => self.blunder_check = !self.blunder_check,
            Row::Opening => {
                let choices = self.opening_choices();
                let position = self
                    .opening
                    .and_then(|o| choices.iter().position(|&c| c == o));
                let next = match position {
                    None if forward => choices.first().copied(),
                    None => choices.last().copied(),
                    Some(i) if forward => choices.get(i + 1).copied(),
                    Some(0) => None,
                    Some(i) => choices.get(i - 1).copied(),
                };
                self.opening = next;
            }
            Row::Start => {}
        }
    }

    /// Chapters you can practise with the colour chosen.
    fn opening_choices(&self) -> Vec<usize> {
        CHAPTERS
            .iter()
            .enumerate()
            .filter(|(_, chapter)| match self.side {
                SideChoice::White => chapter.side == Color::White,
                SideChoice::Black => chapter.side == Color::Black,
                SideChoice::Random => false,
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn value_text(&self, row: Row, data: &CoachData) -> String {
        match row {
            Row::Level => match self.level {
                None => format!(
                    "Your match (level {})",
                    data.profile.suggested_level().number()
                ),
                Some(level) => format!("Level {} of {}", level.number(), Level::STRONGEST.number()),
            },
            Row::Side => match self.side {
                SideChoice::White => "White".to_string(),
                SideChoice::Black => "Black".to_string(),
                SideChoice::Random => "Random".to_string(),
            },
            Row::Clock => match CLOCKS[self.clock] {
                None => "No clock".to_string(),
                Some((minutes, increment)) => format!("{minutes} min + {increment} s"),
            },
            Row::BlunderCheck => {
                if self.blunder_check {
                    "On: warns before a big mistake".to_string()
                } else {
                    "Off".to_string()
                }
            }
            Row::Opening => match self.opening.and_then(|i| CHAPTERS.get(i)) {
                None if self.side == SideChoice::Random => {
                    "Any (pick a colour to practise one)".to_string()
                }
                None => "Any".to_string(),
                Some(chapter) => format!("Practise: {}", chapter.name),
            },
            Row::Start => String::new(),
        }
    }

    fn settings(&self, data: &mut CoachData) -> GameSettings {
        let human = match self.side {
            SideChoice::White => Color::White,
            SideChoice::Black => Color::Black,
            SideChoice::Random if rand::Rng::gen_bool(&mut data.rng, 0.5) => Color::White,
            SideChoice::Random => Color::Black,
        };
        GameSettings {
            level: self.level.unwrap_or_else(|| data.profile.suggested_level()),
            human,
            clock: CLOCKS[self.clock].map(|(minutes, increment)| GameClock {
                base_ms: minutes * 60_000,
                increment_ms: increment * 1000,
            }),
            blunder_check: self.blunder_check,
            opening: self
                .opening
                .and_then(|i| CHAPTERS.get(i))
                .map(|c: &Chapter| c.name),
        }
    }
}

const fn cycle(index: usize, len: usize, forward: bool) -> usize {
    if forward {
        (index + 1) % len
    } else {
        (index + len - 1) % len
    }
}

const fn row_name(row: Row) -> &'static str {
    match row {
        Row::Level => "Opponent",
        Row::Side => "You play",
        Row::Clock => "Clock",
        Row::BlunderCheck => "Blunder check",
        Row::Opening => "Opening",
        Row::Start => "Start the game",
    }
}

fn form_area(area: Rect) -> Rect {
    centred(area, FORM_WIDTH, ROWS.len() as u16 * 2 + 3)
}

fn row_rect(area: Rect, index: usize) -> Rect {
    let form = form_area(area);
    Rect::new(
        form.x + 2,
        form.y + 2 + 2 * index as u16,
        form.width.saturating_sub(4),
        1,
    )
}

impl Screen for Setup {
    fn draw(&self, frame: &mut Frame, data: &CoachData, _now: Instant) {
        let area = frame.area();
        let form = form_area(area);
        frame.render_widget(Clear, form);
        frame.render_widget(panel("New game"), form);
        for (i, &row) in ROWS.iter().enumerate() {
            let rect = row_rect(area, i);
            if rect.bottom() > form.bottom().saturating_sub(1) {
                break;
            }
            let chosen = i == self.row;
            let line = if row == Row::Start {
                let style = if chosen {
                    Style::new()
                        .fg(ratatui::style::Color::Black)
                        .bg(ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
                };
                Line::from(Span::styled(format!("  {}  ", row_name(row)), style))
            } else {
                let arrows = if chosen {
                    ("◂ ", " ▸")
                } else {
                    ("  ", "  ")
                };
                Line::from(vec![
                    Span::styled(if chosen { "▸ " } else { "  " }, Style::new().fg(ACCENT)),
                    Span::styled(
                        format!("{:<15}", row_name(row)),
                        Style::new().fg(if chosen { TEXT } else { DIM }),
                    ),
                    Span::styled(arrows.0, Style::new().fg(ACCENT)),
                    Span::styled(self.value_text(row, data), Style::new().fg(TEXT)),
                    Span::styled(arrows.1, Style::new().fg(ACCENT)),
                ])
            };
            frame.render_widget(Paragraph::new(line), rect);
        }
        let status = Rect::new(
            area.x,
            area.bottom().saturating_sub(1),
            area.width,
            area.height.min(1),
        );
        frame.render_widget(
            Paragraph::new(dim("  ↑↓ choose · ←→ change · Enter start · Esc back")),
            status,
        );
    }

    fn key(&mut self, key: KeyEvent, data: &mut CoachData, _now: Instant) -> Nav {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Nav::Back,
            KeyCode::Up | KeyCode::Char('k') => {
                self.row = cycle(self.row, ROWS.len(), false);
                Nav::Stay
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.row = cycle(self.row, ROWS.len(), true);
                Nav::Stay
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.change(false);
                Nav::Stay
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                self.change(true);
                Nav::Stay
            }
            KeyCode::Enter => {
                let settings = self.settings(data);
                Nav::Replace(Box::new(CoachGame::new(settings, data)))
            }
            _ => Nav::Stay,
        }
    }

    fn mouse(&mut self, mouse: MouseEvent, area: Rect, data: &mut CoachData, _now: Instant) -> Nav {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Nav::Stay;
        }
        let Some(i) =
            (0..ROWS.len()).find(|&i| row_rect(area, i).contains((mouse.column, mouse.row).into()))
        else {
            return Nav::Stay;
        };
        self.row = i;
        if ROWS[i] == Row::Start {
            let settings = self.settings(data);
            return Nav::Replace(Box::new(CoachGame::new(settings, data)));
        }
        self.change(true);
        Nav::Stay
    }
}
