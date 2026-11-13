//! The model's Questions as the window's transcript shows them, from being asked to being
//! settled.

use iced::widget::{button, column, mouse_area, radio, row, text, text_input};
use iced::{Element, Fill};
use sstack::protocol::{self, Answer, Question};

/// Space between the parts of a set of Questions, in logical pixels.
const SPACING: f32 = 10.0;

/// The size of a choice's description, in logical pixels, a little smaller than its label.
const DESCRIPTION_TEXT: f32 = 13.0;

/// What pressing the controls of open Questions sends to the window. Each Question is named by
/// its position in its set, and each row by its position in its Question, the person's own words
/// coming after every choice.
#[derive(Debug, Clone)]
pub struct Controls<Message> {
    /// Sent when the person picks a row of a Question.
    pub choose: fn(usize, usize) -> Message,
    /// Sent when the person changes their own words for a Question.
    pub write: fn(usize, String) -> Message,
    /// Sent when the person sends their answers.
    pub send: Message,
    /// Sent when the person would rather answer in the conversation.
    pub decline: Message,
}

/// A set of the model's Questions, as the person works through them and once settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    /// The Questions, in the order the model asked them.
    pub questions: Vec<Question>,
    /// What the person has picked and typed for each Question so far.
    pub drafts: Vec<Draft>,
    /// Where the set stands.
    pub state: Asking,
}

/// What the person has picked and typed for one Question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    /// The row picked, if any: a choice, or the Question's own-words row.
    pub picked: Option<usize>,
    /// The person's own words so far, kept while another row is picked.
    pub own: String,
}

/// Where a set of the model's Questions stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asking {
    /// The Questions wait for the person.
    Open,
    /// The person sent these answers, one for each Question.
    Answered(Vec<Answer>),
    /// The person chose to answer in the conversation instead.
    Declined,
    /// The Turn ended before the person answered.
    Unanswered,
    /// A Rebuild put the Questions off, and they show nothing until the Turn asks them again,
    /// which opens them once more as the person left them.
    PutOff,
}

impl Asked {
    /// Opens `questions` to the person, each starting on the choice the model recommends.
    pub fn new(questions: Vec<Question>) -> Self {
        let drafts = questions
            .iter()
            .map(|question| Draft {
                picked: question.recommended,
                own: String::new(),
            })
            .collect();
        Self {
            questions,
            drafts,
            state: Asking::Open,
        }
    }

    /// Picks `row` of the Question at position `question`.
    pub fn choose(&mut self, question: usize, row: usize) {
        if let Some(draft) = self.drafts.get_mut(question) {
            draft.picked = Some(row);
        }
    }

    /// Sets the person's own words for the Question at position `question` to `words`, which
    /// picks its own-words row too.
    pub fn write(&mut self, question: usize, words: String) {
        let own_row = self.questions.get(question).map(Question::own_row);
        if let (Some(draft), Some(own_row)) = (self.drafts.get_mut(question), own_row) {
            draft.picked = Some(own_row);
            draft.own = words;
        }
    }

    /// Returns an answer for every Question, or nothing while one still lacks an answer.
    pub fn answers(&self) -> Option<Vec<Answer>> {
        (self.questions.iter().zip(&self.drafts))
            .map(|(question, draft)| question.answer(draft.picked?, &draft.own))
            .collect()
    }

    /// Draws the set: in full with its controls while open, and otherwise as a line saying what
    /// became of it.
    pub fn view<'a, Message: Clone + 'a>(
        &'a self,
        controls: &Controls<Message>,
    ) -> Element<'a, Message> {
        match &self.state {
            Asking::Open => self.open_view(controls),
            Asking::Answered(answers) => text(format!(
                "Answered: {}",
                protocol::describe_all(&self.questions, answers)
            ))
            .into(),
            Asking::Declined => text("Note: you chose to answer the questions in the chat").into(),
            Asking::Unanswered => text("Note: the questions were not answered").into(),
            Asking::PutOff => column![].into(),
        }
    }

    /// Draws the open Questions, each with its rows, and the buttons that send the answers or
    /// decline. Send does nothing until every Question has an answer.
    fn open_view<'a, Message: Clone + 'a>(
        &'a self,
        controls: &Controls<Message>,
    ) -> Element<'a, Message> {
        let questions = (0..self.questions.len()).map(|index| self.question_view(index, controls));
        let send = button("Send answers")
            .on_press_maybe(self.answers().is_some().then(|| controls.send.clone()));
        let decline = button("Answer in the chat").on_press(controls.decline.clone());
        column(questions)
            .push(row![send, decline].spacing(SPACING))
            .spacing(SPACING * 2.0)
            .into()
    }

    /// Draws the Question at position `index` of the set: its header and words, a row for each
    /// choice, and a row for the person's own words.
    fn question_view<'a, Message: Clone + 'a>(
        &'a self,
        index: usize,
        controls: &Controls<Message>,
    ) -> Element<'a, Message> {
        let (question, draft) = (&self.questions[index], &self.drafts[index]);
        let (choose, write) = (controls.choose, controls.write);
        let choices = question.choices.iter().enumerate().map(|(row, choice)| {
            let pick = pickable(
                choice_label(question, row),
                choose(index, row),
                draft.picked == Some(row),
            );
            let description = (!choice.description.is_empty())
                .then(|| text(choice.description.as_str()).size(DESCRIPTION_TEXT));
            column![pick]
                .push(description)
                .spacing(SPACING / 2.0)
                .into()
        });
        let own_row = question.own_row();
        let own = row![
            pickable(
                "Own answer:".to_owned(),
                choose(index, own_row),
                draft.picked == Some(own_row),
            ),
            text_input("Type your own answer", &draft.own)
                .on_input(move |words| write(index, words))
                .width(Fill),
        ]
        .spacing(SPACING);
        column![
            text(format!("Question: {}", question.header)),
            text(question.text.as_str())
        ]
        .extend(choices)
        .push(own)
        .spacing(SPACING)
        .into()
    }
}

/// Returns how the choice at position `row` of `question` is labelled, marking the one the model
/// recommends.
fn choice_label(question: &Question, row: usize) -> String {
    let label = &question.choices[row].label;
    if question.is_recommended(row) {
        format!("{label} (recommended)")
    } else {
        label.clone()
    }
}

/// Draws one row of a Question as a radio button with `label` beside it, which is marked if
/// `picked` and sends `pick` when either is pressed.
///
/// The label is drawn apart from the radio button so that tests can find it, which they cannot
/// do with a radio button's own label.
fn pickable<'a, Message: Clone + 'a>(
    label: String,
    pick: Message,
    picked: bool,
) -> Element<'a, Message> {
    let sent = pick.clone();
    let button = radio("", (), picked.then_some(()), move |()| sent).spacing(0);
    let label = mouse_area(text(label)).on_press(pick);
    row![button, label].spacing(SPACING / 2.0).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sstack::protocol::Choice;

    fn question(labels: &[&str], recommended: Option<usize>) -> Question {
        Question {
            header: "Pick".into(),
            text: "Which?".into(),
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

    #[test]
    fn each_question_starts_on_the_recommended_choice() {
        let asked = Asked::new(vec![question(&["a", "b"], Some(1)), question(&["a"], None)]);
        assert_eq!(asked.drafts[0].picked, Some(1));
        assert_eq!(asked.drafts[1].picked, None);
        assert_eq!(asked.answers(), None);
    }

    #[test]
    fn writing_picks_the_own_words_row() {
        let mut asked = Asked::new(vec![question(&["a"], Some(0)), question(&[], None)]);
        asked.write(1, "  ".into());
        assert_eq!(asked.answers(), None);

        asked.write(1, "mine".into());
        asked.choose(0, 0);

        let answers = vec![Answer::Choice(0), Answer::Own("mine".into())];
        assert_eq!(asked.answers(), Some(answers));
    }

    #[test]
    fn choosing_another_row_keeps_what_was_written() {
        let mut asked = Asked::new(vec![question(&["a"], None)]);
        asked.write(0, "mine".into());
        asked.choose(0, 0);
        assert_eq!(asked.drafts[0].own, "mine");
        asked.choose(0, 1);
        assert_eq!(asked.answers(), Some(vec![Answer::Own("mine".into())]));
    }

    #[test]
    fn controls_for_a_question_that_is_not_there_do_nothing() {
        let mut asked = Asked::new(vec![question(&["a"], None)]);
        let before = asked.clone();
        asked.choose(5, 0);
        asked.write(5, "x".into());
        assert_eq!(asked, before);
    }
}
