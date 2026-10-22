//! Your past games against the coach, newest first, to pick one to review.

use super::super::view::{ACCENT, DIM};
use super::common::{BAD, GOOD, TEXT, centred, dim, panel};
use super::data::CoachData;
use super::review::ReviewScreen;
use super::{Nav, Screen};
use crate::coach::profile::ReviewedGame;
use crate::coach::rating::Outcome;
use crate::coach::review::summarise;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::time::Instant;

const WIDTH: u16 = 80;

#[derive(Default)]
pub struct GamesList {
    selected: usize,
}

pub const fn outcome_word(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Win => "Won",
        Outcome::Draw => "Drew",
        Outcome::Loss => "Lost",
    }
}

fn newest_first(data: &CoachData) -> Vec<&ReviewedGame> {
    data.games.iter().rev().collect()
}

fn game_line(game: &ReviewedGame, chosen: bool) -> Line<'static> {
    let record = &game.record;
    let outcome = record.outcome();
    let color = match outcome {
        Outcome::Win => GOOD,
        Outcome::Draw => TEXT,
        Outcome::Loss => BAD,
    };
    let summary = summarise(&game.reviews, record.human);
    let analysed = if record.is_fully_analysed() {
        format!(
            "accuracy {:>3}  ·  ?? {}  ? {}",
            summary
                .accuracy
                .map_or_else(|| "–".to_string(), |a| format!("{a:.0}%")),
            summary.blunders,
            summary.mistakes
        )
    } else {
        "not reviewed yet".to_string()
    };
    let side = if record.human == crate::types::Color::White {
        "White"
    } else {
        "Black"
    };
    Line::from(vec![
        Span::styled(if chosen { "▸ " } else { "  " }, Style::new().fg(ACCENT)),
        Span::styled(
            format!("{:<5}", outcome_word(outcome)),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {side:<6} level {:<3}", record.level.number()),
            Style::new().fg(TEXT),
        ),
        Span::styled(
            format!("{:>3} moves  ", record.moves.len().div_ceil(2)),
            Style::new().fg(DIM),
        ),
        Span::styled(analysed, Style::new().fg(if chosen { TEXT } else { DIM })),
    ])
}

fn list_area(area: Rect) -> Rect {
    centred(area, WIDTH, area.height.saturating_sub(2))
}

impl Screen for GamesList {
    fn draw(&self, frame: &mut Frame, data: &CoachData, _now: Instant) {
        let area = frame.area();
        let list = list_area(area);
        let games = newest_first(data);
        let rows = usize::from(list.height.saturating_sub(2));
        let skip = (self.selected + 1).saturating_sub(rows);
        let lines: Vec<Line> = if games.is_empty() {
            vec![dim(
                "No games yet. Play one from the menu, then come back here to review it.",
            )]
        } else {
            games
                .iter()
                .enumerate()
                .skip(skip)
                .map(|(i, g)| game_line(g, i == self.selected))
                .collect()
        };
        frame.render_widget(Paragraph::new(lines).block(panel("Your games")), list);
        let status = Rect::new(
            area.x,
            area.bottom().saturating_sub(1),
            area.width,
            area.height.min(1),
        );
        frame.render_widget(
            Paragraph::new(dim("  ↑↓ choose · Enter review · Esc back")),
            status,
        );
    }

    fn key(&mut self, key: KeyEvent, data: &mut CoachData, _now: Instant) -> Nav {
        let count = data.games.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Nav::Back,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                Nav::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1));
                Nav::Stay
            }
            KeyCode::Enter | KeyCode::Right => self.open(data),
            _ => Nav::Stay,
        }
    }

    fn mouse(&mut self, mouse: MouseEvent, area: Rect, data: &mut CoachData, _now: Instant) -> Nav {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Nav::Stay;
        }
        let list = list_area(area);
        let rows = usize::from(list.height.saturating_sub(2));
        let skip = (self.selected + 1).saturating_sub(rows);
        let Some(row) = mouse.row.checked_sub(list.y + 1) else {
            return Nav::Stay;
        };
        let index = skip + usize::from(row);
        if index < data.games.len() && mouse.row < list.bottom().saturating_sub(1) {
            self.selected = index;
            return self.open(data);
        }
        Nav::Stay
    }
}

impl GamesList {
    fn open(&self, data: &CoachData) -> Nav {
        match newest_first(data).get(self.selected) {
            Some(game) => Nav::Push(Box::new(ReviewScreen::new(game.record.id, data))),
            None => Nav::Stay,
        }
    }
}
