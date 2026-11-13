//! Questions the model puts to the person, and the answers that come back.
//!
//! The model asks through the `ask` tool, and each interface shows the questions its own way
//! through an [`Asker`](crate::tools::Asker). A Question offers choices, one of which the model
//! may recommend, and the person can always answer in their own words instead.

use serde::Deserialize;

/// Something the model asks the person and waits for them to decide, holding up its Turn.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Question {
    /// A few words naming what the Question decides, such as `Storage`, for showing where space
    /// is short and for labelling the answer.
    pub header: String,
    /// The Question itself, which may run to several paragraphs.
    #[serde(rename = "question")]
    pub text: String,
    /// The answers the model suggests, in the order to show them. May be empty, which leaves the
    /// person only their own words.
    #[serde(default, rename = "options")]
    pub choices: Vec<Choice>,
    /// The position in `choices` of the answer the model recommends, if it recommends one.
    #[serde(default)]
    pub recommended: Option<usize>,
}

/// An answer the model suggests for a [`Question`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Choice {
    /// The answer in a few words, which the person picks by.
    pub label: String,
    /// What picking it would mean, if the label needs explaining. Empty if it does not.
    #[serde(default)]
    pub description: String,
}

/// How the person answered one [`Question`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// They picked the choice at this position.
    Choice(usize),
    /// They answered in their own words.
    Own(String),
}

impl Question {
    /// Returns whether the choice at `index` is the one the model recommends.
    #[must_use]
    pub fn is_recommended(&self, index: usize) -> bool {
        self.recommended == Some(index)
    }

    /// Returns the row for the person's own words, which comes after every choice.
    #[must_use]
    pub fn own_row(&self) -> usize {
        self.choices.len()
    }

    /// Returns the answer the person gives by picking `row`: a choice, or at [`Self::own_row`]
    /// their own `words`, trimmed. Blank words are no answer yet.
    #[must_use]
    pub fn answer(&self, row: usize, words: &str) -> Option<Answer> {
        if row < self.own_row() {
            return Some(Answer::Choice(row));
        }
        let words = words.trim();
        (!words.is_empty()).then(|| Answer::Own(words.to_owned()))
    }

    /// Describes `answer` to this Question in a few words, for the model and for a record of what
    /// the person said.
    #[must_use]
    pub fn describe(&self, answer: &Answer) -> String {
        let said = match answer {
            Answer::Choice(index) => self
                .choices
                .get(*index)
                .map_or_else(|| "(no such choice)".to_owned(), |c| c.label.clone()),
            Answer::Own(words) => format!("\"{words}\" (own answer)"),
        };
        format!("{}: {said}", self.header)
    }
}

/// Describes every answer in `answers` to the matching Question in `questions`, one per line.
#[must_use]
pub fn describe_all(questions: &[Question], answers: &[Answer]) -> String {
    let lines: Vec<String> = questions
        .iter()
        .zip(answers)
        .map(|(question, answer)| question.describe(answer))
        .collect();
    lines.join("\n")
}

/// What became of the model's [`Question`]s once they were put to the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// The person gave one [`Answer`] for each Question, in order.
    Answered(Vec<Answer>),
    /// The person would rather answer in the conversation, or nobody answered before the Turn
    /// ended.
    Declined,
    /// Nobody decided, because a Rebuild put the Questions off. The call that asked them runs
    /// again, asking them again, when its Turn carries on.
    Deferred,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn storage() -> Question {
        serde_json::from_value(json!({
            "header": "Storage",
            "question": "Where should Sessions live?",
            "options": [{"label": "Files", "description": "One per Session"}, {"label": "SQLite"}],
            "recommended": 0
        }))
        .unwrap()
    }

    #[test]
    fn reads_the_models_words_for_a_question() {
        let question = storage();
        assert_eq!(question.text, "Where should Sessions live?");
        assert_eq!(question.choices[0].description, "One per Session");
        assert_eq!(question.choices[1].description, "");
        assert!(question.is_recommended(0));
        assert!(!question.is_recommended(1));
    }

    #[test]
    fn a_question_needs_no_choices() {
        let open: Question =
            serde_json::from_value(json!({"header": "Name", "question": "What name?"})).unwrap();
        assert_eq!(open.choices, []);
        assert_eq!(open.recommended, None);
    }

    #[test]
    fn answers_are_described_under_their_headers() {
        let question = storage();
        let answers = [Answer::Choice(1), Answer::Own("in git".into())];
        let described = describe_all(&[question.clone(), question], &answers);
        assert_eq!(
            described,
            "Storage: SQLite\nStorage: \"in git\" (own answer)"
        );
    }

    #[test]
    fn a_row_answers_with_its_choice_or_the_persons_own_words() {
        let question = storage();
        assert_eq!(question.own_row(), 2);
        assert_eq!(question.answer(1, "ignored"), Some(Answer::Choice(1)));
        assert_eq!(question.answer(2, " git "), Some(Answer::Own("git".into())));
        assert_eq!(question.answer(2, "  "), None);
    }

    #[test]
    fn a_choice_that_does_not_exist_says_so() {
        assert_eq!(
            storage().describe(&Answer::Choice(9)),
            "Storage: (no such choice)"
        );
    }
}
