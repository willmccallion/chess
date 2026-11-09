//! The start screen: where you stand, today's plan and the way into each part of the coach.

use super::super::view::{ACCENT, DIM};
use super::common::{GOOD, TEXT, WARN, centred, dim, draw_help};
use super::data::{CoachData, now};
use super::{Nav, Screen, endgames, games_list, openings, progress, puzzles, setup};
use crate::coach::endgames::DRILLS;
use crate::coach::profile::{LineStatus, Profile, Task, daily_plan, line_status};
use crate::coach::repertoire::{CHAPTERS, all_lines};
use crate::coach::store::TrainingItem;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::time::Instant;

const WIDTH: u16 = 96;
/// Roughly how tall the page is, for centring it.
const PAGE_HEIGHT: u16 = 22;
/// Rows above the menu: title, stats and a gap.
const MENU_TOP: u16 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entry {
    Play,
    Puzzles,
    Openings,
    Endgames,
    Review,
    Progress,
}

const ENTRIES: [Entry; 6] = [
    Entry::Play,
    Entry::Puzzles,
    Entry::Openings,
    Entry::Endgames,
    Entry::Review,
    Entry::Progress,
];

const KEYS: &[(&str, &str)] = &[
    ("↑ ↓  j k", "choose"),
    ("Enter  →", "open"),
    ("1 - 6", "open by number"),
    ("p", "pieces as pictures, symbols or letters"),
    ("q  Esc", "quit"),
];

#[derive(Default)]
pub struct Home {
    selected: usize,
    help: bool,
}

impl Entry {
    const fn name(self) -> &'static str {
        match self {
            Self::Play => "Play",
            Self::Puzzles => "Puzzles",
            Self::Openings => "Openings",
            Self::Endgames => "Endgames",
            Self::Review => "Review games",
            Self::Progress => "Progress",
        }
    }

    fn describe(self, data: &CoachData) -> String {
        match self {
            Self::Play => format!(
                "A game against the coach · level {} is your match",
                data.profile.suggested_level().number()
            ),
            Self::Puzzles => puzzles_summary(data),
            Self::Openings => openings_summary(data),
            Self::Endgames => endgames_summary(data),
            Self::Review => games_summary(data),
            Self::Progress => "Ratings, strengths, weaknesses and what to work on".to_string(),
        }
    }

    fn open(self, data: &mut CoachData) -> Nav {
        match self {
            Self::Play => Nav::Push(Box::new(setup::Setup::new(data))),
            Self::Puzzles => Nav::Push(Box::new(puzzles::Puzzles::new(data))),
            Self::Openings => Nav::Push(Box::new(openings::Openings::new(data))),
            Self::Endgames => Nav::Push(Box::new(endgames::Endgames::default())),
            Self::Review => Nav::Push(Box::new(games_list::GamesList::default())),
            Self::Progress => Nav::Push(Box::new(progress::Progress::default())),
        }
    }
}

fn puzzles_summary(data: &CoachData) -> String {
    let own = data.own_puzzles.len();
    if data.lichess.is_empty() && own == 0 {
        return "Tactics from your mistakes (import more: see Progress)".to_string();
    }
    let profile = &data.profile;
    format!(
        "Rating {} · {} solved · {} from your games",
        profile.puzzle_rating.0, profile.puzzles_solved, own
    )
}

fn openings_summary(data: &CoachData) -> String {
    let now = now();
    let statuses: Vec<LineStatus> = all_lines()
        .map(|l| line_status(l, &data.schedules, now))
        .collect();
    let count = |status: LineStatus| statuses.iter().filter(|&&s| s == status).count();
    let due = count(LineStatus::Due);
    let due_text = if due > 0 {
        format!(" · {due} due")
    } else {
        String::new()
    };
    format!(
        "Queen's Gambit and more · {} of {} learned{due_text}",
        count(LineStatus::Learned),
        statuses.len()
    )
}

fn endgames_summary(data: &CoachData) -> String {
    let done = DRILLS
        .iter()
        .filter(|d| {
            data.schedule(&TrainingItem::Endgame(d.id.to_string()))
                .is_some_and(|s| s.level > 0)
        })
        .count();
    format!(
        "Checkmates and key endings · {done} of {} done",
        DRILLS.len()
    )
}

fn games_summary(data: &CoachData) -> String {
    let Some(game) = data.games.last() else {
        return "Your games, move by move, once you've played one".to_string();
    };
    format!(
        "{} game{} · last: {} against level {}",
        data.games.len(),
        plural(data.games.len()),
        games_list::outcome_word(game.record.outcome()),
        game.record.level.number()
    )
}

fn task_text(task: Task) -> String {
    match task {
        Task::ReviewPuzzles(n) => format!("Solve {n} puzzle{} waiting for you", plural(n)),
        Task::ReviewLines(n) => format!("Review {n} opening line{} due today", plural(n)),
        Task::LearnLine(line) => {
            let chapter = CHAPTERS
                .iter()
                .find(|c| c.lines.iter().any(|l| l.id == line.id))
                .map_or("", |c| c.name);
            format!("Learn a new line: {chapter} · {}", line.name)
        }
        Task::Endgame(drill) => format!("Endgame drill: {}", drill.name),
        Task::Play(level) => format!("Play a game against level {}", level.number()),
    }
}

const fn task_entry(task: Task) -> Entry {
    match task {
        Task::ReviewPuzzles(_) => Entry::Puzzles,
        Task::ReviewLines(_) | Task::LearnLine(_) => Entry::Openings,
        Task::Endgame(_) => Entry::Endgames,
        Task::Play(_) => Entry::Play,
    }
}

const fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn page(area: Rect) -> Rect {
    let page = centred(area, WIDTH, area.height);
    let top = area.height.saturating_sub(PAGE_HEIGHT) / 3;
    Rect::new(
        page.x,
        page.y + top,
        page.width,
        page.height.saturating_sub(top),
    )
}

fn menu_row(area: Rect, index: usize) -> Rect {
    let page = page(area);
    Rect::new(page.x, page.y + MENU_TOP + index as u16, page.width, 1)
}

/// The title, and your ratings, games and streak.
fn header_lines(profile: &Profile) -> Vec<Line<'static>> {
    let streak = match profile.streak_days {
        0 => String::new(),
        1 => "   ·   1 day in a row".to_string(),
        n => format!("   ·   {n} days in a row"),
    };
    let bold = Style::new().fg(TEXT).add_modifier(Modifier::BOLD);
    vec![
        Line::from(vec![Span::styled(
            "♞ Chess Coach",
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![
            Span::styled("Rating ", Style::new().fg(DIM)),
            Span::styled(profile.rating.0.to_string(), bold),
            Span::styled("   ·   Puzzles ", Style::new().fg(DIM)),
            Span::styled(profile.puzzle_rating.0.to_string(), bold),
            Span::styled(
                format!(
                    "   ·   {} game{}{streak}",
                    profile.games(),
                    plural(profile.games())
                ),
                Style::new().fg(DIM),
            ),
        ]),
    ]
}

/// The menu's row for `entry`, the `index`th, highlighted when `chosen`.
fn menu_line(index: usize, entry: Entry, chosen: bool, data: &CoachData) -> Line<'static> {
    let marker = if chosen { "▸ " } else { "  " };
    let name_style = if chosen {
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(TEXT)
    };
    Line::from(vec![
        Span::styled(marker, Style::new().fg(ACCENT)),
        Span::styled(format!("{} {:<14}", index + 1, entry.name()), name_style),
        Span::styled(
            entry.describe(data),
            Style::new().fg(if chosen { TEXT } else { DIM }),
        ),
    ])
}

/// Today's plan, then your focus and any warnings.
fn plan_lines(data: &CoachData) -> Vec<Line<'static>> {
    let profile = &data.profile;
    let mut lines = vec![Line::styled(
        "Today",
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
    )];
    for (i, task) in daily_plan(profile, &data.own_puzzles, &data.schedules, now())
        .into_iter()
        .enumerate()
    {
        lines.push(Line::from(vec![
            Span::styled(format!("  {}. ", i + 1), Style::new().fg(DIM)),
            Span::styled(task_text(task), Style::new().fg(TEXT)),
            Span::styled(
                format!("  ({})", task_entry(task).name()),
                Style::new().fg(DIM),
            ),
        ]));
    }
    if let Some(focus) = profile.focus.first() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(
                "Focus  ",
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(super::progress::focus_title(*focus), Style::new().fg(GOOD)),
        ]));
    }
    for warning in &data.warnings {
        lines.push(Line::styled(warning.clone(), Style::new().fg(WARN)));
    }
    lines
}

impl Screen for Home {
    fn draw(&self, frame: &mut Frame, data: &CoachData, _now: Instant) {
        let area = frame.area();
        let page = page(area);
        frame.render_widget(
            Paragraph::new(header_lines(&data.profile)),
            Rect::new(page.x, page.y, page.width, 2.min(page.height)),
        );
        for (i, entry) in ENTRIES.iter().enumerate() {
            let row = menu_row(area, i);
            if row.bottom() > area.bottom() {
                break;
            }
            let line = menu_line(i, *entry, i == self.selected, data);
            frame.render_widget(Paragraph::new(line), row);
        }
        let plan_top = menu_row(area, ENTRIES.len()).y + 1;
        if plan_top < area.bottom() {
            let height = area.bottom().saturating_sub(plan_top + 1);
            frame.render_widget(
                Paragraph::new(plan_lines(data)),
                Rect::new(page.x, plan_top, page.width, height),
            );
        }
        let status = Rect::new(
            area.x,
            area.bottom().saturating_sub(1),
            area.width,
            area.height.min(1),
        );
        frame.render_widget(
            Paragraph::new(dim("  ↑↓ choose · Enter open · 1-6 jump · ? help · q quit")),
            status,
        );
        if self.help {
            draw_help(frame, KEYS);
        }
    }

    fn key(&mut self, key: KeyEvent, data: &mut CoachData, _now: Instant) -> Nav {
        if self.help {
            self.help = false;
            return Nav::Stay;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Nav::Quit,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = (self.selected + ENTRIES.len() - 1) % ENTRIES.len();
                Nav::Stay
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.selected = (self.selected + 1) % ENTRIES.len();
                Nav::Stay
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                ENTRIES[self.selected].open(data)
            }
            KeyCode::Char(c @ '1'..='6') => {
                self.selected = c as usize - '1' as usize;
                ENTRIES[self.selected].open(data)
            }
            KeyCode::Char('p') => {
                data.piece_style = data.piece_style.next();
                Nav::Stay
            }
            KeyCode::Char('?' | 'h') => {
                self.help = true;
                Nav::Stay
            }
            _ => Nav::Stay,
        }
    }

    fn mouse(&mut self, mouse: MouseEvent, area: Rect, data: &mut CoachData, _now: Instant) -> Nav {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return Nav::Stay;
        }
        if self.help {
            self.help = false;
            return Nav::Stay;
        }
        let hit = (0..ENTRIES.len())
            .find(|&i| menu_row(area, i).contains((mouse.column, mouse.row).into()));
        match hit {
            Some(i) => {
                self.selected = i;
                ENTRIES[i].open(data)
            }
            None => Nav::Stay,
        }
    }
}
