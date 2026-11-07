//! The list of Sessions, which shows at a glance which ones need the person.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::tui::component::Component;
use crate::tui::theme;

/// How many columns the sidebar takes up, border included.
pub const WIDTH: u16 = 34;
/// How many rows sit above the first Session.
const HEADING_ROWS: u16 = 1;
/// How many columns of a row a Session's title can use.
const TITLE_COLUMNS: usize = 17;
/// Replaces the end of a title too long to show in full.
const ELLIPSIS: &str = "..";

/// What a Session is doing, as far as the person needs to know at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// A Turn is running without needing the person.
    Working,
    /// A Turn is waiting for the person to answer about a shell command.
    Blocked,
    /// A Turn stopped with an error the person has not seen yet.
    Failed,
    /// A Turn ended with output the person has not seen yet.
    Done,
    /// A Turn stopped between Steps and will carry on.
    Parked,
    /// Nothing is happening and nothing is new.
    Idle,
}

impl Activity {
    /// Returns the single character that stands for this activity.
    fn marker(self) -> &'static str {
        match self {
            Self::Working => "*",
            Self::Blocked => "!",
            Self::Failed => "x",
            Self::Done => "+",
            Self::Parked => "-",
            Self::Idle => " ",
        }
    }

    /// Returns the name shown for this activity.
    fn label(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Done => "done",
            Self::Parked => "parked",
            Self::Idle => "idle",
        }
    }

    /// Returns the look of a row with this activity, which draws the eye to what needs the person.
    fn style(self) -> Style {
        match self {
            Self::Blocked | Self::Failed => theme::ATTENTION,
            Self::Working | Self::Done => theme::TEXT,
            Self::Parked | Self::Idle => theme::NOTE,
        }
    }
}

/// One Session as the sidebar shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The Session's id, which identifies it when clicked.
    pub id: String,
    /// Tells the Session apart from the others. May be any length.
    pub title: String,
    /// What the Session is doing.
    pub activity: Activity,
    /// How long the Session has been waiting for the person, if it is.
    pub waiting: Option<Duration>,
}

impl Row {
    /// Returns the row as drawn, marking it if it is the one on `screen`.
    fn line(&self, on_screen: bool) -> Line<'_> {
        let cursor = if on_screen { ">" } else { " " };
        let age = self.waiting.map_or_else(String::new, age);
        let text = format!(
            "{} {:<width$} {:<7} {age:>3}",
            self.activity.marker(),
            fit(&self.title, TITLE_COLUMNS),
            self.activity.label(),
            width = TITLE_COLUMNS,
        );
        Line::from(vec![
            Span::styled(format!("{cursor} "), theme::MARKER),
            Span::styled(text, self.activity.style()),
        ])
    }
}

/// Lists the Sessions in the order the App gives them, keeping the one on screen in view.
#[derive(Debug, Default)]
pub struct Sidebar {
    /// What to show, in order.
    rows: Vec<Row>,
    /// The position in `rows` of the Session on screen.
    on_screen: usize,
    /// Where the sidebar sits on the screen. Empty when it is hidden.
    area: Rect,
    /// What the harness is doing instead of the usual heading, if anything.
    progress: Option<String>,
}

impl Sidebar {
    /// Replaces what is listed. `on_screen` is the position in `rows` of the Session on screen.
    pub fn list(&mut self, rows: Vec<Row>, on_screen: usize) {
        self.rows = rows;
        self.on_screen = on_screen;
    }

    /// Sets what the harness is doing, which replaces the heading until it is cleared.
    pub fn set_progress(&mut self, progress: Option<String>) {
        self.progress = progress;
    }

    /// Tells the sidebar where it will be drawn, so that clicks can be matched to its rows. An
    /// empty `area` means it is hidden.
    pub fn place(&mut self, area: Rect) {
        self.area = area;
    }

    /// Returns the id of the Session drawn at this screen position, if any.
    pub fn session_at(&self, column: u16, row: u16) -> Option<&str> {
        if !self.area.contains(Position::new(column, row)) {
            return None;
        }
        let from_first = row.checked_sub(self.area.y + HEADING_ROWS)?;
        let index = self.first_shown() + usize::from(from_first);
        self.rows.get(index).map(|row| row.id.as_str())
    }

    /// Returns how many Sessions fit on screen at once.
    fn capacity(&self) -> usize {
        usize::from(self.area.height.saturating_sub(HEADING_ROWS))
    }

    /// Returns the position of the top row shown, which scrolls only as far as it must.
    fn first_shown(&self) -> usize {
        (self.on_screen + 1).saturating_sub(self.capacity().max(1))
    }
}

impl Component for Sidebar {
    fn render(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let heading = match &self.progress {
            Some(progress) => Line::styled(progress.as_str(), theme::ATTENTION),
            None => Line::styled("sessions", theme::NOTE),
        };
        let shown = self.rows.iter().enumerate().skip(self.first_shown());
        let rows = shown.map(|(index, row)| row.line(index == self.on_screen));
        let lines: Vec<Line> = std::iter::once(heading).chain(rows).collect();
        let border = Block::new()
            .borders(Borders::RIGHT)
            .border_style(theme::BORDER);
        frame.render_widget(Paragraph::new(lines).block(border), area);
    }
}

/// Returns `age` in the largest whole unit that suits it, in at most four columns.
fn age(age: Duration) -> String {
    let seconds = age.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..172_800 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", (seconds / 86_400).min(999)),
    }
}

/// Returns the first line of `title`, shortened to fit in `columns`.
fn fit(title: &str, columns: usize) -> String {
    let line = title.lines().next().unwrap_or_default();
    let mut fitted = String::new();
    let mut used = 0;
    for character in line.chars() {
        used += character.width().unwrap_or(0);
        if used > columns {
            return shortened(&fitted, columns);
        }
        fitted.push(character);
    }
    fitted
}

/// Returns `text`, which fits in `columns`, with room made at its end for an ellipsis.
fn shortened(text: &str, columns: usize) -> String {
    let mut kept = String::new();
    let mut used = ELLIPSIS.len();
    for character in text.chars() {
        used += character.width().unwrap_or(0);
        if used > columns {
            break;
        }
        kept.push(character);
    }
    kept + ELLIPSIS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::screen;

    fn row(id: &str, title: &str, activity: Activity) -> Row {
        Row {
            id: id.into(),
            title: title.into(),
            activity,
            waiting: None,
        }
    }

    fn sidebar(rows: Vec<Row>, on_screen: usize, height: u16) -> Sidebar {
        let mut sidebar = Sidebar::default();
        sidebar.list(rows, on_screen);
        sidebar.place(Rect::new(0, 0, WIDTH, height));
        sidebar
    }

    fn drawn(sidebar: &Sidebar, height: u16) -> Vec<String> {
        screen(WIDTH, height, |frame| sidebar.render(frame, frame.area()))
    }

    #[test]
    fn each_session_shows_what_it_is_doing_and_how_long_it_has_waited() {
        let mut blocked = row("1", "add land tool", Activity::Blocked);
        blocked.waiting = Some(Duration::from_mins(14));
        let rows = vec![
            blocked,
            row("2", "scroll transcript", Activity::Done),
            row("3", "update docs", Activity::Failed),
            row("4", "fix the approval box wrap", Activity::Working),
            row("5", "old\nsession", Activity::Idle),
        ];

        let shown = drawn(&sidebar(rows, 1, 6), 6);

        assert_eq!(shown[0], "sessions                         │");
        assert_eq!(shown[1], "  ! add land tool     blocked 14m│");
        assert_eq!(shown[2], "> + scroll transcript done       │");
        assert_eq!(shown[3], "  x update docs       failed     │");
        assert_eq!(shown[4], "  * fix the approva.. working    │");
        assert_eq!(shown[5], "    old               idle       │");
    }

    #[test]
    fn what_the_harness_is_doing_replaces_the_heading() {
        let mut sidebar = sidebar(vec![row("1", "t", Activity::Parked)], 0, 2);
        sidebar.set_progress(Some("building".into()));
        let shown = drawn(&sidebar, 2);
        assert!(shown[0].starts_with("building"), "{shown:#?}");
        assert!(shown[1].contains("- t"), "{shown:#?}");
        assert!(shown[1].contains("parked"), "{shown:#?}");
    }

    #[test]
    fn the_session_on_screen_stays_in_view() {
        let rows = (0..6)
            .map(|id| row(&id.to_string(), "t", Activity::Idle))
            .collect();
        let sidebar = sidebar(rows, 4, 3);
        let shown = drawn(&sidebar, 3);
        assert!(shown[1].starts_with("    t"), "{shown:#?}");
        assert!(shown[2].starts_with(">   t"), "{shown:#?}");
        assert_eq!(sidebar.session_at(0, 1), Some("3"));
        assert_eq!(sidebar.session_at(0, 2), Some("4"));
    }

    #[test]
    fn clicks_find_sessions_only_on_their_rows() {
        let rows = vec![row("a", "t", Activity::Idle), row("b", "t", Activity::Idle)];
        let sidebar = sidebar(rows, 0, 8);
        assert_eq!(sidebar.session_at(5, 1), Some("a"));
        assert_eq!(sidebar.session_at(5, 2), Some("b"));
        assert_eq!(sidebar.session_at(5, 0), None);
        assert_eq!(sidebar.session_at(5, 3), None);
        assert_eq!(sidebar.session_at(WIDTH, 1), None);
    }

    #[test]
    fn a_hidden_sidebar_draws_nothing_and_finds_nothing() {
        let mut sidebar = Sidebar::default();
        sidebar.list(vec![row("a", "t", Activity::Idle)], 0);
        sidebar.place(Rect::new(0, 0, 0, 8));
        let shown = screen(40, 8, |frame| sidebar.render(frame, Rect::ZERO));
        assert!(shown.iter().all(String::is_empty));
        assert_eq!(sidebar.session_at(0, 1), None);
    }

    #[test]
    fn ages_use_the_largest_unit_that_suits_them() {
        let cases = [
            (9, "9s"),
            (59, "59s"),
            (60, "1m"),
            (3_599, "59m"),
            (3_600, "1h"),
            (172_799, "47h"),
            (172_800, "2d"),
            (999_999_999, "999d"),
        ];
        for (seconds, expected) in cases {
            assert_eq!(age(Duration::from_secs(seconds)), expected);
        }
    }

    #[test]
    fn long_titles_are_shortened_by_display_width() {
        assert_eq!(fit("short", 10), "short");
        assert_eq!(fit("exactly 10", 10), "exactly 10");
        assert_eq!(fit("one more than 10", 10), "one more..");
        assert_eq!(fit("two\nlines", 10), "two");
        assert_eq!(fit("", 10), "");
        assert_eq!(fit("日本語のタイトルです", 10), "日本語の..");
    }
}
