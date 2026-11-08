//! The list of Sessions beside the one on screen, which shows at a glance which ones need the
//! person.

use std::time::Duration;

use iced::widget::text::Wrapping;
use iced::widget::{Column, button, column, row, scrollable, space, text};
use iced::{Element, Fill};

use super::{Message, SPACING};

/// How wide the sidebar is, in logical pixels.
const WIDTH: f32 = 260.0;

/// The most characters of a Session Name that the sidebar shows, after which it is cut short.
const MAX_NAME_CHARS: usize = 28;

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
    /// Returns the word shown for this activity.
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
}

/// One Session as the sidebar shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The Session's id, which a click on the row shows.
    pub id: String,
    /// The Session Name. May be any length, and span lines.
    pub name: String,
    /// What the Session is doing.
    pub activity: Activity,
    /// How long the Session has been waiting for the person, if it is.
    pub waiting: Option<Duration>,
}

impl Row {
    /// Returns what the row says the Session is doing, with how long it has waited.
    pub fn status(&self) -> String {
        match self.waiting {
            Some(waited) => format!("{} {}", self.activity.label(), age(waited)),
            None => self.activity.label().to_owned(),
        }
    }

    /// Draws the row as a button that shows its Session. The Session on screen stands out, and
    /// one that needs the person is drawn as a warning.
    fn view(self, on_screen: bool) -> Element<'static, Message> {
        let label = row![
            text(fit(&self.name)).wrapping(Wrapping::None),
            space::horizontal(),
            text(self.status()).wrapping(Wrapping::None),
        ]
        .spacing(SPACING);
        let style = match (on_screen, self.activity) {
            (true, _) => button::primary,
            (false, Activity::Blocked | Activity::Failed) => button::danger,
            (false, _) => button::secondary,
        };
        button(label)
            .style(style)
            .width(Fill)
            .on_press(Message::Show(self.id))
            .into()
    }
}

/// Draws the sidebar: `rows` in queue order, with the one at position `on_screen` marked, and a
/// button that starts a new Session.
pub fn view(rows: Vec<Row>, on_screen: usize) -> Element<'static, Message> {
    let listed = rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| row.view(index == on_screen));
    let heading = row![
        text("Sessions"),
        space::horizontal(),
        button("New").on_press(Message::NewSession)
    ];
    column![
        heading,
        scrollable(Column::with_children(listed).spacing(SPACING / 2.0))
    ]
    .spacing(SPACING)
    .width(WIDTH)
    .into()
}

/// Returns `age` in the largest whole unit that suits it.
fn age(age: Duration) -> String {
    let seconds = age.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..172_800 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// Returns the first line of `name`, cut short if it is too long for the sidebar.
fn fit(name: &str) -> String {
    let line = name.lines().next().unwrap_or_default();
    if line.chars().count() > MAX_NAME_CHARS || line.len() < name.trim_end().len() {
        let kept: String = line.chars().take(MAX_NAME_CHARS).collect();
        format!("{}..", kept.trim_end())
    } else {
        line.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_use_the_largest_whole_unit() {
        let ages = [
            (59, "59s"),
            (60, "1m"),
            (3_599, "59m"),
            (7_200, "2h"),
            (172_800, "2d"),
        ];
        for (seconds, shown) in ages {
            assert_eq!(age(Duration::from_secs(seconds)), shown);
        }
    }

    #[test]
    fn long_or_several_line_names_are_cut_short() {
        assert_eq!(fit("short"), "short");
        assert_eq!(fit("two\nlines"), "two..");
        let long = "a".repeat(40);
        assert_eq!(fit(&long), format!("{}..", "a".repeat(MAX_NAME_CHARS)));
    }

    #[test]
    fn a_waiting_session_says_for_how_long() {
        let mut row = Row {
            id: "1".into(),
            name: "x".into(),
            activity: Activity::Idle,
            waiting: Some(Duration::from_secs(90)),
        };
        assert_eq!(row.status(), "idle 1m");
        row.activity = Activity::Working;
        row.waiting = None;
        assert_eq!(row.status(), "working");
    }
}
