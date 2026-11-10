//! The panel that puts the model's Questions to the person.

use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use sstack::question::{self, Answer, Question, Response};
use sstack::session_report::{QuestionRequest, SessionReport};

use crate::describe;
use crate::tui::component::{Component, KeyOutcome};
use crate::tui::event::{Action, Event};
use crate::tui::theme;

/// How much of the screen's width the panel takes. Its height grows to fit, up to the whole
/// screen.
const WIDTH: Constraint = Constraint::Percentage(70);

/// What the panel tells the person about its keys.
const HINT: &str = "Up/Down choose, Enter confirms, Shift-Tab goes back, Esc answers in the chat";

/// The panel that shows the model's Questions one at a time, holding the keyboard until the person
/// answers them all or declines.
///
/// Every Question offers the model's choices and a last row for the person's own words, which
/// they reach by moving down or simply by typing. Questions still open are dropped, giving no
/// answer, when their Turn ends. If a Rebuild put them off instead, the Turn asks them again when
/// it carries on, and they start again from the recommended choices.
#[derive(Debug, Default)]
pub struct Questions {
    /// Sets of Questions still to answer, oldest first. Only the oldest is shown.
    waiting: VecDeque<Form>,
}

/// One set of the model's Questions as the person works through it.
#[derive(Debug)]
struct Form {
    /// The Questions, and where the answers go.
    request: QuestionRequest,
    /// The position of the Question on screen.
    current: usize,
    /// What the person has chosen or typed for each Question so far.
    picks: Vec<Pick>,
}

/// What the person has chosen or typed for one Question so far.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pick {
    /// The highlighted row: a choice, or one past the last choice for the person's own words.
    highlighted: usize,
    /// What the person has typed as their own answer, kept while they look at the choices.
    own: String,
}

/// Where a set of Questions stands after a key.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// The person is still answering.
    Answering,
    /// The person answered every Question.
    Done(Vec<Answer>),
    /// The person would rather answer in the conversation.
    Declined,
}

impl Pick {
    /// Starts on the choice the model recommends, or the first row.
    fn new(question: &Question) -> Self {
        Self {
            highlighted: question.recommended.unwrap_or(0),
            own: String::new(),
        }
    }

    /// Returns the answer the highlighted row gives, or nothing for own words not yet typed.
    fn answer(&self, question: &Question) -> Option<Answer> {
        question.answer(self.highlighted, &self.own)
    }

    /// Acts on a key that edits the answer, for a Question whose own-words row is at `own_row`.
    fn edit(&mut self, code: KeyCode, own_row: usize) {
        match code {
            KeyCode::Up => self.highlighted = self.highlighted.saturating_sub(1),
            KeyCode::Down => self.highlighted = (self.highlighted + 1).min(own_row),
            KeyCode::Backspace => {
                self.own.pop();
            }
            KeyCode::Char(character) => self.typed(character, own_row),
            _ => {}
        }
    }

    /// Acts on a typed character: a choice's number highlights it, and anything else is part of
    /// the person's own words, which it moves to.
    fn typed(&mut self, character: char, own_row: usize) {
        let number = character.to_digit(10).and_then(|n| usize::try_from(n).ok());
        match number {
            Some(n @ 1..) if self.highlighted < own_row && n <= own_row => self.highlighted = n - 1,
            _ => {
                self.highlighted = own_row;
                self.own.push(character);
            }
        }
    }
}

impl Form {
    /// Starts answering `request` at its first Question.
    fn new(request: QuestionRequest) -> Self {
        let picks = request.questions.iter().map(Pick::new).collect();
        Self {
            request,
            current: 0,
            picks,
        }
    }

    /// Returns the Question on screen.
    fn question(&self) -> &Question {
        &self.request.questions[self.current]
    }

    /// Acts on a key, returning where the Questions stand.
    fn key(&mut self, code: KeyCode) -> Step {
        match code {
            KeyCode::Esc => return Step::Declined,
            KeyCode::Enter => return self.confirm(),
            KeyCode::BackTab => self.current = self.current.saturating_sub(1),
            code => {
                let own_row = self.question().own_row();
                self.picks[self.current].edit(code, own_row);
            }
        }
        Step::Answering
    }

    /// Settles the Question on screen if it has an answer, moving to the next, or finishing after
    /// the last.
    fn confirm(&mut self) -> Step {
        let questions = &self.request.questions;
        let answers: Option<Vec<Answer>> = (self.picks.iter().zip(questions))
            .take(self.current + 1)
            .map(|(pick, question)| pick.answer(question))
            .collect();
        match answers {
            Some(answers) if answers.len() == questions.len() => Step::Done(answers),
            Some(_) => {
                self.current += 1;
                Step::Answering
            }
            None => Step::Answering,
        }
    }

    /// Returns the panel's title, which says which Question is on screen.
    fn title(&self) -> String {
        let header = &self.question().header;
        match self.request.questions.len() {
            1 => format!(" Question: {header} "),
            total => format!(" Question {} of {total}: {header} ", self.current + 1),
        }
    }

    /// Returns the Question on screen, its rows and the hint, styled.
    fn text(&self) -> Text<'_> {
        let question = self.question();
        let pick = &self.picks[self.current];
        let mut lines: Vec<Line> = question
            .text
            .split('\n')
            .map(|line| Line::styled(line, theme::TEXT))
            .collect();
        lines.push(Line::default());
        for (index, choice) in question.choices.iter().enumerate() {
            let recommended = if question.is_recommended(index) {
                " (recommended)"
            } else {
                ""
            };
            let label = format!("{}. {}{recommended}", index + 1, choice.label);
            lines.push(row(&label, index == pick.highlighted));
            if !choice.description.is_empty() {
                lines.push(Line::styled(
                    format!("     {}", choice.description),
                    theme::NOTE,
                ));
            }
        }
        lines.push(own_row(question, pick));
        lines.push(Line::default());
        lines.push(Line::styled(HINT, theme::MARKER));
        Text::from(lines)
    }
}

/// Returns a row the person can choose, marked if `highlighted`.
fn row(label: &str, highlighted: bool) -> Line<'static> {
    if highlighted {
        Line::styled(format!("> {label}"), theme::SELECTION)
    } else {
        Line::styled(format!("  {label}"), theme::TEXT)
    }
}

/// Returns the row for the person's own words, with what they have typed and, while it is
/// highlighted, a cursor after it.
fn own_row(question: &Question, pick: &Pick) -> Line<'static> {
    let own_row = question.own_row();
    let highlighted = pick.highlighted == own_row;
    let cursor = if highlighted { "_" } else { "" };
    let label = format!("{}. Own answer: {}{cursor}", own_row + 1, pick.own);
    row(&label, highlighted)
}

impl Component for Questions {
    fn update(&mut self, event: &Event) {
        match event {
            Event::Report(SessionReport::Question(request)) => {
                self.waiting.push_back(Form::new(request.clone()));
            }
            Event::Report(SessionReport::TurnEnded(_)) => self.waiting.clear(),
            _ => {}
        }
    }

    fn key(&mut self, key: KeyEvent) -> KeyOutcome {
        let Some(form) = self.waiting.front_mut() else {
            return KeyOutcome::Ignored;
        };
        let note = match form.key(key.code) {
            Step::Answering => return KeyOutcome::Handled,
            Step::Done(answers) => {
                let described = question::describe_all(&form.request.questions, &answers);
                form.request.reply.send(Response::Answered(answers));
                describe::answered(&described)
            }
            Step::Declined => {
                form.request.reply.send(Response::Declined);
                describe::DECLINED.to_owned()
            }
        };
        self.waiting.pop_front();
        KeyOutcome::Act(Action::QuestionsAnswered(note))
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let Some(form) = self.waiting.front() else {
            return;
        };
        let block = Block::bordered()
            .title(form.title())
            .border_style(theme::ATTENTION)
            .title_style(theme::ATTENTION);
        let text_width = block.inner(area.centered_horizontally(WIDTH)).width;
        let panel = Paragraph::new(form.text())
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
    use crate::tui::test_support::{drawn, key, type_into};
    use insta::assert_snapshot;
    use sstack::agent::TurnEnd;
    use sstack::question::Choice;
    use sstack::session_report::Reply;
    use tokio::sync::oneshot;

    /// What the person answered, or `None` if they declined.
    type Answered = oneshot::Receiver<Response>;

    /// Returns a Question headed `header` offering `labels`, recommending the one at
    /// `recommended`.
    fn question(header: &str, labels: &[&str], recommended: Option<usize>) -> Question {
        Question {
            header: header.into(),
            text: format!("Which {header}?"),
            choices: labels
                .iter()
                .map(|label| Choice {
                    label: (*label).into(),
                    description: String::new(),
                })
                .collect(),
            recommended,
        }
    }

    /// Puts `questions` to the panel, returning where the answers arrive.
    fn ask(panel: &mut Questions, questions: Vec<Question>) -> Answered {
        let (reply, answer) = Reply::new();
        let request = QuestionRequest { questions, reply };
        panel.update(&Event::Report(SessionReport::Question(request)));
        answer
    }

    /// Presses `code` on the panel.
    fn press(panel: &mut Questions, code: KeyCode) -> KeyOutcome {
        panel.key(key(code))
    }

    #[test]
    fn keys_pass_through_while_nothing_is_asked() {
        let mut panel = Questions::default();
        assert_eq!(press(&mut panel, KeyCode::Enter), KeyOutcome::Ignored);
    }

    #[test]
    fn enter_takes_the_recommended_choice_and_moves_to_the_next_question() {
        let mut panel = Questions::default();
        let mut answered = ask(
            &mut panel,
            vec![
                question("Store", &["Files", "SQLite"], Some(1)),
                question("Name", &["a", "b"], None),
            ],
        );

        assert_eq!(press(&mut panel, KeyCode::Enter), KeyOutcome::Handled);
        assert!(answered.try_recv().is_err());
        let outcome = press(&mut panel, KeyCode::Enter);

        let note = "[answered] Store: SQLite\nName: a".to_owned();
        assert_eq!(outcome, KeyOutcome::Act(Action::QuestionsAnswered(note)));
        let answers = vec![Answer::Choice(1), Answer::Choice(0)];
        assert_eq!(answered.try_recv(), Ok(Response::Answered(answers)));
    }

    #[test]
    fn arrows_and_numbers_choose_and_typing_answers_in_own_words() {
        let mut panel = Questions::default();
        let mut answered = ask(
            &mut panel,
            vec![
                question("One", &["a", "b", "c"], None),
                question("Two", &["a", "b"], None),
                question("Three", &["a"], None),
            ],
        );

        press(&mut panel, KeyCode::Down);
        press(&mut panel, KeyCode::Down);
        press(&mut panel, KeyCode::Up);
        press(&mut panel, KeyCode::Enter);
        press(&mut panel, KeyCode::Char('2'));
        press(&mut panel, KeyCode::Enter);
        type_into(&mut panel, "x1z");
        press(&mut panel, KeyCode::Backspace);
        press(&mut panel, KeyCode::Enter);

        let answers = vec![
            Answer::Choice(1),
            Answer::Choice(1),
            Answer::Own("x1".into()),
        ];
        assert_eq!(answered.try_recv(), Ok(Response::Answered(answers)));
    }

    #[test]
    fn own_words_must_be_typed_before_they_answer() {
        let mut panel = Questions::default();
        let mut answered = ask(&mut panel, vec![question("Name", &[], None)]);

        press(&mut panel, KeyCode::Enter);
        press(&mut panel, KeyCode::Char(' '));
        press(&mut panel, KeyCode::Enter);
        assert!(answered.try_recv().is_err());

        type_into(&mut panel, "sstack");
        press(&mut panel, KeyCode::Enter);
        assert_eq!(
            answered.try_recv(),
            Ok(Response::Answered(vec![Answer::Own("sstack".into())]))
        );
    }

    #[test]
    fn shift_tab_goes_back_to_change_an_earlier_answer() {
        let mut panel = Questions::default();
        let mut answered = ask(
            &mut panel,
            vec![
                question("One", &["a", "b"], None),
                question("Two", &["a"], None),
            ],
        );

        press(&mut panel, KeyCode::Enter);
        press(&mut panel, KeyCode::BackTab);
        press(&mut panel, KeyCode::Down);
        press(&mut panel, KeyCode::Enter);
        press(&mut panel, KeyCode::Tab);
        press(&mut panel, KeyCode::Enter);

        let answers = vec![Answer::Choice(1), Answer::Choice(0)];
        assert_eq!(answered.try_recv(), Ok(Response::Answered(answers)));
    }

    #[test]
    fn escape_declines_to_answer_here() {
        let mut panel = Questions::default();
        let mut answered = ask(&mut panel, vec![question("One", &["a"], None)]);

        let outcome = press(&mut panel, KeyCode::Esc);

        let note = describe::DECLINED.to_owned();
        assert_eq!(outcome, KeyOutcome::Act(Action::QuestionsAnswered(note)));
        assert_eq!(answered.try_recv(), Ok(Response::Declined));
        assert_eq!(press(&mut panel, KeyCode::Esc), KeyOutcome::Ignored);
    }

    #[test]
    fn the_end_of_a_turn_drops_what_is_still_asked() {
        let mut panel = Questions::default();
        let mut answered = ask(&mut panel, vec![question("One", &["a"], None)]);
        panel.update(&Event::Resize);
        panel.update(&Event::Report(SessionReport::TurnEnded(Ok(
            TurnEnd::Cancelled,
        ))));
        assert!(answered.try_recv().is_err());
        assert_eq!(press(&mut panel, KeyCode::Enter), KeyOutcome::Ignored);
    }

    #[test]
    fn nothing_is_drawn_while_nothing_is_asked() {
        let panel = Questions::default();
        let shown = drawn(30, 5, |frame| panel.render(frame, frame.area()));
        let blank = shown.backend().buffer().content.iter();
        assert!(blank.map(ratatui::buffer::Cell::symbol).all(|s| s == " "));
    }

    #[test]
    fn a_question_shows_its_choices_with_the_recommended_one_chosen() {
        let mut panel = Questions::default();
        let mut store = question("Storage", &["Files", "SQLite"], Some(0));
        store.choices[0].description = "One per Session".into();
        let _answered = ask(&mut panel, vec![store, question("Name", &[], None)]);
        let shown = drawn(60, 14, |frame| panel.render(frame, frame.area()));
        assert_snapshot!(shown.backend());
    }

    #[test]
    fn own_words_show_with_a_cursor_while_typed() {
        let mut panel = Questions::default();
        let _answered = ask(&mut panel, vec![question("Name", &["a"], None)]);
        type_into(&mut panel, "sstack");
        let shown = drawn(60, 10, |frame| panel.render(frame, frame.area()));
        assert_snapshot!(shown.backend());
    }
}
