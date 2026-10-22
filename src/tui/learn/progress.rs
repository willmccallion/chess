//! Your progress: ratings over time, how you play in each phase, and what to work on.

use super::super::view::{ACCENT, DIM};
use super::common::{
    BAD, GOOD, INFO, TEXT, bar, centred, dim, heading, label_value, panel, sparkline,
};
use super::data::CoachData;
use super::{Nav, Screen};
use crate::coach::judgement::Phase;
use crate::coach::profile::{Focus, PeriodStats, RECENT_GAMES};
use crate::coach::review::CauseKind;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use std::time::Instant;

const WIDTH: u16 = 96;
/// A change in errors per game smaller than this is shown as steady.
const TREND_THRESHOLD: f64 = 0.2;

#[derive(Default)]
pub struct Progress {
    scroll: u16,
}

/// A short name for a focus area.
pub fn focus_title(focus: Focus) -> String {
    match focus {
        Focus::Mistakes { kind, per_game } => format!("{} ({per_game:.1} per game)", kind.name()),
        Focus::Habit { habit, games } => format!("{} (in {games} games)", habit.name()),
        Focus::Phase { phase, accuracy } => {
            format!("{} play ({accuracy:.0}% accuracy)", phase.name())
        }
    }
}

fn focus_advice(focus: Focus) -> &'static str {
    match focus {
        Focus::Mistakes { kind, .. } => kind.advice(),
        Focus::Habit { habit, .. } => habit.advice(),
        Focus::Phase {
            phase: Phase::Opening,
            ..
        } => {
            "Learn and drill your opening lines, and follow the principles: develop, castle, control the centre."
        }
        Focus::Phase {
            phase: Phase::Middlegame,
            ..
        } => {
            "Slow down in the middlegame: before each move, check every capture and check for both sides."
        }
        Focus::Phase {
            phase: Phase::Endgame,
            ..
        } => {
            "Do the endgame drills: the basic mates and king-and-pawn endings come up again and again."
        }
    }
}

fn trend_span(recent: f64, earlier: Option<f64>) -> Span<'static> {
    match earlier {
        Some(before) if recent < before - TREND_THRESHOLD => {
            Span::styled(format!(" ↓ from {before:.1}"), Style::new().fg(GOOD))
        }
        Some(before) if recent > before + TREND_THRESHOLD => {
            Span::styled(format!(" ↑ from {before:.1}"), Style::new().fg(BAD))
        }
        Some(_) => Span::styled(" steady", Style::new().fg(DIM)),
        None => Span::raw(""),
    }
}

fn mistakes_lines(recent: &PeriodStats, earlier: Option<&PeriodStats>) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let worst = CauseKind::ALL
        .iter()
        .map(|&k| recent.errors(k))
        .fold(0.0, f64::max)
        .max(1.0);
    for kind in CauseKind::ALL {
        let rate = recent.errors(kind);
        let earlier_rate = earlier.filter(|e| e.games > 0).map(|e| e.errors(kind));
        if rate == 0.0 && earlier_rate.is_none_or(|r| r == 0.0) {
            continue;
        }
        lines.push(Line::from(vec![
            Span::styled(format!("{:<22}", kind.name()), Style::new().fg(TEXT)),
            Span::styled(
                bar(rate / worst, 12),
                Style::new().fg(if rate >= 1.0 { BAD } else { ACCENT }),
            ),
            Span::styled(format!(" {rate:.1}/game"), Style::new().fg(TEXT)),
            trend_span(rate, earlier_rate),
        ]));
    }
    if lines.is_empty() {
        lines.push(dim("No mistakes or blunders recorded yet."));
    }
    lines
}

fn page_lines(data: &CoachData) -> Vec<Line<'static>> {
    let profile = &data.profile;
    let mut lines = Vec::new();
    lines.push(heading("Ratings"));
    let history: Vec<f64> = profile
        .rating_history
        .iter()
        .map(|r| f64::from(r.0))
        .collect();
    let (low, high) = history
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), &r| (lo.min(r), hi.max(r)));
    let scaled: Vec<f64> = history
        .iter()
        .map(|r| {
            if high > low {
                (r - low) / (high - low) * 100.0
            } else {
                50.0
            }
        })
        .collect();
    let change =
        (profile.games() > 0).then(|| profile.rating.0 - crate::coach::rating::Elo::START.0);
    let mut rating_spans = vec![
        Span::styled(format!("{:<12}", "Games"), Style::new().fg(DIM)),
        Span::styled(
            format!("{:<6}", profile.rating.0),
            Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(sparkline(&scaled, 40), Style::new().fg(INFO)),
    ];
    if let Some(change) = change {
        rating_spans.push(Span::styled(
            format!("  {change:+} since you started"),
            Style::new().fg(DIM),
        ));
    }
    lines.push(Line::from(rating_spans));
    lines.push(label_value(
        "Puzzles",
        format!(
            "{}   {} solved first time of {} tried",
            profile.puzzle_rating.0, profile.puzzles_solved, profile.puzzles_tried
        ),
    ));
    lines.push(label_value(
        "Results",
        format!(
            "{} won · {} drawn · {} lost",
            profile.wins, profile.draws, profile.losses
        ),
    ));
    lines.push(Line::raw(""));

    let recent = &profile.recent;
    lines.push(heading(match recent.games.min(RECENT_GAMES) {
        1 => "Your last game".to_string(),
        n => format!("Your last {n} games"),
    }));
    if recent.games == 0 {
        lines.push(dim("Play a game against the coach and review it to see your strengths and weaknesses here."));
    } else {
        if let Some(accuracy) = recent.accuracy {
            lines.push(label_value(
                "Accuracy",
                format!(
                    "{accuracy:.0}%   ·   {:.1} blunders per game",
                    recent.blunders_per_game
                ),
            ));
        }
        for phase in Phase::ALL {
            if let Some(&accuracy) = recent.phase_accuracy.get(&phase) {
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<12}", phase.name()), Style::new().fg(DIM)),
                    Span::styled(
                        bar(accuracy / 100.0, 20),
                        Style::new().fg(if accuracy >= 75.0 { GOOD } else { ACCENT }),
                    ),
                    Span::styled(format!(" {accuracy:.0}%"), Style::new().fg(TEXT)),
                ]));
            }
        }
        lines.push(Line::raw(""));
        lines.push(heading("Mistakes and blunders by cause"));
        lines.extend(mistakes_lines(recent, profile.earlier.as_ref()));
        if !recent.habits.is_empty() {
            lines.push(Line::raw(""));
            lines.push(heading("Opening habits"));
            for (habit, games) in &recent.habits {
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<28}", habit.name()), Style::new().fg(TEXT)),
                    Span::styled(
                        format!("in {games} of {} games", recent.games),
                        Style::new().fg(DIM),
                    ),
                ]));
            }
        }
        let strongest = recent
            .phase_accuracy
            .iter()
            .max_by(|a, b| a.1.total_cmp(b.1));
        if let Some((phase, accuracy)) = strongest
            && recent.phase_accuracy.len() > 1
        {
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("Strength    ", Style::new().fg(DIM)),
                Span::styled(
                    format!(
                        "your {} ({accuracy:.0}% accuracy)",
                        phase.name().to_lowercase()
                    ),
                    Style::new().fg(GOOD),
                ),
            ]));
        }
    }
    lines.push(Line::raw(""));
    lines.push(heading("What to work on"));
    if profile.focus.is_empty() {
        lines.push(dim(
            "Nothing stands out yet: keep playing and reviewing your games.",
        ));
    }
    for (i, focus) in profile.focus.iter().enumerate() {
        lines.push(Line::styled(
            format!("{}. {}", i + 1, focus_title(*focus)),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::styled(
            format!("   {}", focus_advice(*focus)),
            Style::new().fg(TEXT),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(heading("Puzzle database"));
    if data.lichess.is_empty() {
        lines.push(Line::styled(
            "Not imported. Download lichess_db_puzzle.csv.zst from database.lichess.org, then run:"
                .to_string(),
            Style::new().fg(TEXT),
        ));
        lines.push(dim(
            "  zstd -dc lichess_db_puzzle.csv.zst | chess import-puzzles -",
        ));
    } else {
        lines.push(label_value(
            "Imported",
            format!("{} Lichess puzzles", data.lichess.len()),
        ));
    }
    lines.push(dim(format!("Your data: {}", data.store_dir().display())));
    lines
}

impl Screen for Progress {
    fn draw(&self, frame: &mut Frame, data: &CoachData, _now: Instant) {
        let area = frame.area();
        let page = centred(area, WIDTH, area.height.saturating_sub(1));
        frame.render_widget(
            Paragraph::new(page_lines(data))
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0))
                .block(panel("Progress")),
            page,
        );
        let status = Rect::new(
            area.x,
            area.bottom().saturating_sub(1),
            area.width,
            area.height.min(1),
        );
        frame.render_widget(Paragraph::new(dim("  ↑↓ scroll · Esc back")), status);
    }

    fn key(&mut self, key: KeyEvent, _data: &mut CoachData, _now: Instant) -> Nav {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Nav::Back,
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll = self.scroll.saturating_add(1);
                Nav::Stay
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll = self.scroll.saturating_sub(1);
                Nav::Stay
            }
            _ => Nav::Stay,
        }
    }
}
