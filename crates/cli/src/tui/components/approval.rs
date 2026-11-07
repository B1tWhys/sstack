//! Asking the person whether a shell command may run.

use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::tui::component::{Component, KeyOutcome};
use crate::tui::event::{Action, ApprovalRequest, Event};
use crate::tui::theme;

/// Asks the person about each shell command the model wants to run, holding the keyboard until
/// they answer.
///
/// Only `y` approves. Every unanswered question is denied when its Turn ends.
#[derive(Debug, Default)]
pub struct Approval {
    /// Questions still to answer, oldest first. Only the oldest is shown.
    waiting: VecDeque<ApprovalRequest>,
}

impl Component for Approval {
    fn update(&mut self, event: &Event) {
        match event {
            Event::Approval(request) => self.waiting.push_back(request.clone()),
            Event::TurnEnded(_) => self.waiting.clear(),
            _ => {}
        }
    }

    fn key(&mut self, key: KeyEvent) -> KeyOutcome {
        let Some(request) = self.waiting.front() else {
            return KeyOutcome::Ignored;
        };
        let Some(approved) = answer(key.code) else {
            return KeyOutcome::Handled;
        };
        request.reply.send(approved);
        self.waiting.pop_front();
        KeyOutcome::Act(Action::ApprovalAnswered)
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let Some(request) = self.waiting.front() else {
            return;
        };
        let popup = area.centered(Constraint::Percentage(60), Constraint::Length(5));
        let question = format!("Run `{}`? [y/N]", request.command);
        let block = Block::bordered()
            .title(" Approve shell command ")
            .border_style(theme::ATTENTION)
            .title_style(theme::ATTENTION);
        frame.render_widget(Clear, popup);
        let paragraph = Paragraph::new(question)
            .style(theme::TEXT)
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph.block(block), popup);
    }
}

/// Returns the answer a key gives, or nothing if it is not an answer. Anything that confirms
/// without saying yes counts as no.
fn answer(code: KeyCode) -> Option<bool> {
    match code {
        KeyCode::Char('y' | 'Y') => Some(true),
        KeyCode::Char('n' | 'N') | KeyCode::Enter | KeyCode::Esc => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::event::Reply;
    use crate::tui::test_support::{key, screen};
    use sstack::agent::TurnEnd;
    use tokio::sync::oneshot;

    fn ask(approval: &mut Approval, command: &str) -> oneshot::Receiver<bool> {
        let (reply, answer) = Reply::new();
        let command = command.to_owned();
        approval.update(&Event::Approval(ApprovalRequest { command, reply }));
        answer
    }

    #[test]
    fn keys_pass_through_while_nothing_is_asked() {
        let mut approval = Approval::default();
        assert_eq!(approval.key(key(KeyCode::Char('y'))), KeyOutcome::Ignored);
    }

    #[test]
    fn questions_are_answered_in_order_and_hold_every_key() {
        let mut approval = Approval::default();
        let mut first = ask(&mut approval, "ls");
        let mut second = ask(&mut approval, "rm");

        assert_eq!(approval.key(key(KeyCode::Char('x'))), KeyOutcome::Handled);
        assert!(first.try_recv().is_err());
        let answered = KeyOutcome::Act(Action::ApprovalAnswered);
        assert_eq!(approval.key(key(KeyCode::Char('Y'))), answered);
        assert_eq!(approval.key(key(KeyCode::Enter)), answered);

        assert_eq!(first.try_recv(), Ok(true));
        assert_eq!(second.try_recv(), Ok(false));
    }

    #[test]
    fn only_yes_approves() {
        assert_eq!(answer(KeyCode::Char('y')), Some(true));
        for denial in [KeyCode::Char('n'), KeyCode::Esc, KeyCode::Enter] {
            assert_eq!(answer(denial), Some(false));
        }
        assert_eq!(answer(KeyCode::Tab), None);
    }

    #[test]
    fn the_end_of_a_turn_denies_what_is_still_asked() {
        let mut approval = Approval::default();
        let mut asked = ask(&mut approval, "ls");
        approval.update(&Event::Resize);
        approval.update(&Event::TurnEnded(Ok(TurnEnd::Cancelled)));
        assert!(asked.try_recv().is_err());
        assert_eq!(approval.key(key(KeyCode::Char('y'))), KeyOutcome::Ignored);
    }

    #[test]
    fn the_oldest_question_is_shown_over_the_screen_and_nothing_otherwise() {
        let mut approval = Approval::default();
        let empty = screen(30, 7, |frame| approval.render(frame, frame.area()));
        assert!(empty.iter().all(String::is_empty));

        let _answer = ask(&mut approval, "ls");
        let shown = screen(30, 7, |frame| approval.render(frame, frame.area()));
        assert!(shown.join("\n").contains("Run `ls`? [y/N]"), "{shown:#?}");
    }
}
