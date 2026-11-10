//! The panel that offers the Connections a Session can switch to.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::tui::component::{Component, KeyOutcome};
use crate::tui::event::Action;
use crate::tui::theme;

/// How much of the screen's width the panel takes. Its height grows to fit, up to the whole
/// screen.
const WIDTH: Constraint = Constraint::Percentage(50);

/// What the panel tells the person about its keys.
const HINT: &str = "Up/Down choose, Enter switches from the next Turn, Esc keeps it";

/// The panel that offers the Connections the Session on screen can switch to, holding the
/// keyboard while it is open. Choosing one asks the App to switch; nothing changes until then.
#[derive(Debug, Default)]
pub struct ConnectionPicker {
    /// What the panel offers while it is open. Missing while it is closed.
    open: Option<Offer>,
}

/// The Connections on offer, and where the person is among them.
#[derive(Debug)]
struct Offer {
    /// The names of the Connections, in the order shown.
    names: Vec<String>,
    /// The position of the Connection the Session runs on now, which is marked.
    current: usize,
    /// The position of the highlighted Connection.
    highlighted: usize,
}

impl ConnectionPicker {
    /// Offers `names`, starting on `current`, the one the Session runs on now.
    pub fn open(&mut self, names: Vec<String>, current: &str) {
        let current = names.iter().position(|name| name == current).unwrap_or(0);
        self.open = Some(Offer {
            names,
            current,
            highlighted: current,
        });
    }

    /// Returns whether the panel is open.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }
}

impl Offer {
    /// Moves the highlight for `code`, keeping it on the list.
    fn moved(&mut self, code: KeyCode) {
        let last = self.names.len().saturating_sub(1);
        self.highlighted = match code {
            KeyCode::Up => self.highlighted.saturating_sub(1),
            KeyCode::Down => (self.highlighted + 1).min(last),
            KeyCode::Char(digit) => digit
                .to_digit(10)
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| (1..=last + 1).contains(n))
                .map_or(self.highlighted, |n| n - 1),
            _ => self.highlighted,
        };
    }

    /// Returns the rows, with the current Connection marked and the highlighted one picked out,
    /// then the hint.
    fn text(&self) -> Text<'_> {
        let mut lines: Vec<Line> = (self.names.iter().enumerate())
            .map(|(index, name)| {
                let now = if index == self.current { " (now)" } else { "" };
                let label = format!("{}. {name}{now}", index + 1);
                if index == self.highlighted {
                    Line::styled(format!("> {label}"), theme::SELECTION)
                } else {
                    Line::styled(format!("  {label}"), theme::TEXT)
                }
            })
            .collect();
        lines.push(Line::default());
        lines.push(Line::styled(HINT, theme::MARKER));
        Text::from(lines)
    }
}

impl Component for ConnectionPicker {
    fn key(&mut self, key: KeyEvent) -> KeyOutcome {
        let Some(offer) = &mut self.open else {
            return KeyOutcome::Ignored;
        };
        match key.code {
            KeyCode::Esc => self.open = None,
            KeyCode::Enter => {
                let name = offer.names.get(offer.highlighted).cloned();
                self.open = None;
                if let Some(name) = name {
                    return KeyOutcome::Act(Action::SwitchConnection(name));
                }
            }
            code => offer.moved(code),
        }
        KeyOutcome::Handled
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let Some(offer) = &self.open else {
            return;
        };
        let block = Block::bordered()
            .title(" Connection ")
            .border_style(theme::BORDER)
            .title_style(theme::PROMPT);
        let text_width = block.inner(area.centered_horizontally(WIDTH)).width;
        let panel = Paragraph::new(offer.text())
            .wrap(Wrap { trim: false })
            .block(block);
        let height = u16::try_from(panel.line_count(text_width)).unwrap_or(u16::MAX);
        let popup = area.centered(WIDTH, Constraint::Length(height.min(area.height)));
        frame.render_widget(Clear, popup);
        frame.render_widget(panel, popup);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::{drawn, key};
    use insta::assert_snapshot;

    /// Returns a picker open on three Connections, with the Session on `work`.
    fn picker() -> ConnectionPicker {
        let mut picker = ConnectionPicker::default();
        let names = ["anthropic", "openai", "work"].map(str::to_owned).to_vec();
        picker.open(names, "work");
        picker
    }

    #[test]
    fn nothing_is_drawn_and_keys_pass_through_while_closed() {
        let mut picker = ConnectionPicker::default();
        assert_eq!(picker.key(key(KeyCode::Enter)), KeyOutcome::Ignored);
        let shown = drawn(30, 5, |frame| picker.render(frame, frame.area()));
        assert_snapshot!(shown.backend());
    }

    #[test]
    fn the_connections_show_with_the_current_one_marked_and_chosen() {
        let picker = picker();
        let shown = drawn(80, 10, |frame| picker.render(frame, frame.area()));
        assert_snapshot!(shown.backend());
    }

    #[test]
    fn arrows_and_numbers_choose_and_enter_switches() {
        let mut picker = picker();
        assert_eq!(picker.key(key(KeyCode::Down)), KeyOutcome::Handled);
        assert_eq!(picker.key(key(KeyCode::Up)), KeyOutcome::Handled);
        let chosen = picker.key(key(KeyCode::Enter));
        assert_eq!(
            chosen,
            KeyOutcome::Act(Action::SwitchConnection("openai".into()))
        );
        assert!(!picker.is_open());

        let mut picker = self::picker();
        picker.key(key(KeyCode::Char('1')));
        picker.key(key(KeyCode::Char('9')));
        picker.key(key(KeyCode::Tab));
        let chosen = picker.key(key(KeyCode::Enter));
        assert_eq!(
            chosen,
            KeyOutcome::Act(Action::SwitchConnection("anthropic".into()))
        );
    }

    #[test]
    fn escape_closes_without_switching() {
        let mut picker = picker();
        assert_eq!(picker.key(key(KeyCode::Esc)), KeyOutcome::Handled);
        assert!(!picker.is_open());
    }

    #[test]
    fn a_picker_with_nothing_on_offer_switches_to_nothing() {
        let mut picker = ConnectionPicker::default();
        picker.open(Vec::new(), "gone");
        assert_eq!(picker.key(key(KeyCode::Down)), KeyOutcome::Handled);
        assert_eq!(picker.key(key(KeyCode::Enter)), KeyOutcome::Handled);
        assert!(!picker.is_open());
    }
}
