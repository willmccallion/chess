//! The repertoire by chapter, with where you stand on each line.

use super::common::{GOOD, INFO, TEXT, WARN, centred, dim, panel};
use super::data::{CoachData, now};
use super::line::LineScreen;
use super::super::view::{ACCENT, DIM};
use super::{Nav, Screen};
use crate::coach::profile::{LineStatus, line_status};
use crate::coach::repertoire::{CHAPTERS, Chapter, Line as RepLine};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use std::time::Instant;

const WIDTH: u16 = 84;
const INTRO_HEIGHT: u16 = 5;

/// A row of the list: a chapter heading or one of its lines.
#[derive(Clone, Copy)]
enum Row {
    Chapter(&'static Chapter),
    Line(&'static Chapter, &'static RepLine),
}

pub struct Openings {
    rows: Vec<Row>,
    selected: usize,
}

impl Openings {
    pub fn new(data: &CoachData) -> Self {
        let rows: Vec<Row> = CHAPTERS
            .iter()
            .flat_map(|chapter| {
                std::iter::once(Row::Chapter(chapter)).chain(chapter.lines.iter().map(move |line| Row::Line(chapter, line)))
            })
            .collect();
        let now = now();
        let first_due_or_new = rows
            .iter()
            .position(|row| matches!(row, Row::Line(_, line) if line_status(line, &data.schedules, now) == LineStatus::Due))
            .or_else(|| {
                rows.iter().position(
                    |row| matches!(row, Row::Line(_, line) if line_status(line, &data.schedules, now) == LineStatus::New),
                )
            })
            .unwrap_or(1);
        Self { rows, selected: first_due_or_new }
    }

    fn line_indices(&self) -> Vec<usize> {
        self.rows.iter().enumerate().filter(|(_, r)| matches!(r, Row::Line(..))).map(|(i, _)| i).collect()
    }

    fn step(&mut self, forward: bool) {
        let lines = self.line_indices();
        let position = lines.iter().position(|&i| i == self.selected).unwrap_or(0);
        let next = if forward { (position + 1).min(lines.len() - 1) } else { position.saturating_sub(1) };
        self.selected = lines[next];
    }

    fn selected_line(&self) -> Option<&'static RepLine> {
        match self.rows.get(self.selected) {
            Some(Row::Line(_, line)) => Some(line),
            _ => None,
        }
    }

    fn selected_chapter(&self) -> Option<&'static Chapter> {
        match self.rows.get(self.selected) {
            Some(Row::Line(chapter, _) | Row::Chapter(chapter)) => Some(chapter),
            None => None,
        }
    }

    /// Opens the selected line: a lesson if it's new, a drill otherwise.
    fn open(&self, data: &CoachData, force_lesson: bool) -> Nav {
        let Some(line) = self.selected_line() else {
            return Nav::Stay;
        };
        let new = line_status(line, &data.schedules, now()) == LineStatus::New;
        if new || force_lesson {
            Nav::Push(Box::new(LineScreen::lesson(line, Vec::new())))
        } else {
            Nav::Push(Box::new(LineScreen::drill(line, Vec::new())))
        }
    }

    /// Drills every due line, one after another.
    fn review_due(data: &CoachData) -> Nav {
        let now = now();
        let mut due: Vec<&'static RepLine> = CHAPTERS
            .iter()
            .flat_map(|c| c.lines.iter())
            .filter(|l| line_status(l, &data.schedules, now) == LineStatus::Due)
            .collect();
        if due.is_empty() {
            return Nav::Stay;
        }
        let first = due.remove(0);
        Nav::Push(Box::new(LineScreen::drill(first, due)))
    }
}

fn status_span(status: LineStatus) -> Span<'static> {
    match status {
        LineStatus::New => Span::styled("new     ", Style::new().fg(INFO)),
        LineStatus::Due => Span::styled("review  ", Style::new().fg(WARN).add_modifier(Modifier::BOLD)),
        LineStatus::Learning => Span::styled("learning", Style::new().fg(DIM)),
        LineStatus::Learned => Span::styled("learned ", Style::new().fg(GOOD)),
    }
}

fn layout(area: Rect) -> (Rect, Rect) {
    let page = centred(area, WIDTH, area.height.saturating_sub(1));
    let intro = Rect::new(page.x, page.y, page.width, INTRO_HEIGHT.min(page.height));
    let list = Rect::new(page.x, page.y + intro.height, page.width, page.height.saturating_sub(intro.height));
    (intro, list)
}

fn visible_skip(selected: usize, rows: usize) -> usize {
    (selected + 2).saturating_sub(rows)
}

impl Screen for Openings {
    fn draw(&self, frame: &mut Frame, data: &CoachData, _now: Instant) {
        let area = frame.area();
        let (intro_area, list_area) = layout(area);
        let intro = self.selected_chapter().map_or("", |c| c.intro);
        let chapter_name = self.selected_chapter().map_or("Openings", |c| c.name);
        frame.render_widget(
            Paragraph::new(Line::styled(intro.to_string(), Style::new().fg(TEXT)))
                .wrap(Wrap { trim: true })
                .block(panel(chapter_name)),
            intro_area,
        );
        let now = now();
        let rows = usize::from(list_area.height.saturating_sub(2));
        let skip = visible_skip(self.selected, rows);
        let lines: Vec<Line> = self
            .rows
            .iter()
            .enumerate()
            .skip(skip)
            .take(rows)
            .map(|(i, row)| match row {
                Row::Chapter(chapter) => {
                    let side = if chapter.side == crate::types::Color::White { "as White" } else { "as Black" };
                    Line::from(vec![
                        Span::styled(chapter.name, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                        Span::styled(format!("  {side}"), Style::new().fg(DIM)),
                    ])
                }
                Row::Line(_, line) => {
                    let chosen = i == self.selected;
                    Line::from(vec![
                        Span::styled(if chosen { "  ▸ " } else { "    " }, Style::new().fg(ACCENT)),
                        status_span(line_status(line, &data.schedules, now)),
                        Span::raw("  "),
                        Span::styled(
                            line.name,
                            if chosen { Style::new().fg(TEXT).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT) },
                        ),
                        Span::styled(format!("  {} moves", line.your_move_count()), Style::new().fg(DIM)),
                    ])
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).block(panel("Lines")), list_area);
        let status = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, area.height.min(1));
        frame.render_widget(
            Paragraph::new(dim("  ↑↓ choose · Enter learn or drill · l lesson · d drill · r review all due · Esc back")),
            status,
        );
    }

    fn key(&mut self, key: KeyEvent, data: &mut CoachData, _now: Instant) -> Nav {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Nav::Back,
            KeyCode::Up | KeyCode::Char('k') => {
                self.step(false);
                Nav::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.step(true);
                Nav::Stay
            }
            KeyCode::Enter | KeyCode::Right => self.open(data, false),
            KeyCode::Char('l') => self.open(data, true),
            KeyCode::Char('d') => match self.selected_line() {
                Some(line) => Nav::Push(Box::new(LineScreen::drill(line, Vec::new()))),
                None => Nav::Stay,
            },
            KeyCode::Char('r') => Self::review_due(data),
            _ => Nav::Stay,
        }
    }

    fn mouse(&mut self, mouse: MouseEvent, area: Rect, data: &mut CoachData, _now: Instant) -> Nav {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Nav::Stay;
        }
        let (_, list) = layout(area);
        let rows = usize::from(list.height.saturating_sub(2));
        let Some(offset) = mouse.row.checked_sub(list.y + 1) else {
            return Nav::Stay;
        };
        let index = visible_skip(self.selected, rows) + usize::from(offset);
        if usize::from(offset) < rows && matches!(self.rows.get(index), Some(Row::Line(..))) {
            self.selected = index;
            return self.open(data, false);
        }
        Nav::Stay
    }
}
