use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::parse_input;
use crate::connection::ToolSpec;
use crate::protocol::{self, Question, Response};
use crate::tools::{Asker, Tool, ToolError};

/// The most Questions the model may ask at once, so the person is not buried in them.
const MAX_QUESTIONS: usize = 4;
/// The most choices one Question may offer, besides the person's own words.
const MAX_CHOICES: usize = 6;
/// What the model hears when the person would rather answer in the conversation.
const DECLINED: &str = "the user declined to answer here. End your turn and wait for them to \
    reply in the conversation";

/// The tool for putting [`Question`]s to the person and waiting for their answers.
pub(super) struct Ask {
    /// Who answers.
    asker: Arc<dyn Asker>,
}

impl Ask {
    /// Creates the tool, asking through `asker`.
    pub(super) fn new(asker: Arc<dyn Asker>) -> Self {
        Self { asker }
    }
}

/// The input to the `ask` tool.
#[derive(Deserialize)]
struct AskInput {
    /// The Questions, in the order the person answers them.
    questions: Vec<Question>,
}

/// Refuses Questions the person could not answer sensibly, saying why for the model.
fn check(questions: &[Question]) -> Result<(), ToolError> {
    if !(1..=MAX_QUESTIONS).contains(&questions.len()) {
        return Err(ToolError::Failed(format!(
            "ask between 1 and {MAX_QUESTIONS} questions at once"
        )));
    }
    questions.iter().try_for_each(check_one)
}

/// Refuses one Question the person could not answer sensibly, saying why for the model.
fn check_one(question: &Question) -> Result<(), ToolError> {
    let blank = [&question.header, &question.text]
        .iter()
        .any(|words| words.trim().is_empty());
    let crowded = question.choices.len() > MAX_CHOICES;
    let unknown = (question.recommended).is_some_and(|i| i >= question.choices.len());
    let faults = [
        (
            blank,
            "every question needs a header and a question".to_owned(),
        ),
        (
            crowded,
            format!("offer at most {MAX_CHOICES} options per question"),
        ),
        (
            unknown,
            format!("`{}` recommends an option it lacks", question.header),
        ),
    ];
    let fault = faults
        .into_iter()
        .find_map(|(faulty, why)| faulty.then_some(why));
    fault.map_or(Ok(()), |fault| Err(ToolError::Failed(fault)))
}

#[async_trait]
impl Tool for Ask {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ask".into(),
            description: format!(
                "Ask the user 1 to {MAX_QUESTIONS} questions and wait for their answers. Each \
                 question can offer up to {MAX_CHOICES} options and recommend one; the user can \
                 always answer in their own words instead. Use it when you need the user to \
                 decide something before you go on, such as in a grilling session, rather than \
                 asking in your reply. Never ask for facts you can find out with other tools."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_QUESTIONS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "header": {
                                    "type": "string",
                                    "description": "What the question decides, in 1 to 3 words"
                                },
                                "question": {
                                    "type": "string",
                                    "description": "The question, with any context the user \
                                        needs. May span paragraphs."
                                },
                                "options": {
                                    "type": "array",
                                    "maxItems": MAX_CHOICES,
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": {
                                                "type": "string",
                                                "description": "The answer in a few words"
                                            },
                                            "description": {
                                                "type": "string",
                                                "description": "What choosing it means"
                                            }
                                        },
                                        "required": ["label"]
                                    }
                                },
                                "recommended": {
                                    "type": "integer",
                                    "description": "Index in options of the answer you \
                                        recommend"
                                }
                            },
                            "required": ["header", "question"]
                        }
                    }
                },
                "required": ["questions"]
            }),
        }
    }

    async fn run(&self, input: Value) -> Result<String, ToolError> {
        let AskInput { questions } = parse_input(input)?;
        check(&questions)?;
        match self.asker.ask(questions.clone()).await {
            Response::Answered(answers) => Ok(protocol::describe_all(&questions, &answers)),
            Response::Declined => Err(ToolError::Failed(DECLINED.to_owned())),
            Response::Deferred => Err(ToolError::Deferred),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Answer;
    use std::sync::Mutex;

    /// An Asker that gives `answers`, or declines if there are none, and remembers what it was
    /// asked.
    #[derive(Default)]
    struct Scripted {
        /// What to answer.
        answers: Option<Vec<Answer>>,
        /// Whether a Rebuild puts the Questions off, whatever `answers` says.
        put_off: bool,
        /// The Questions it was asked, if any.
        asked: Mutex<Vec<Question>>,
    }

    #[async_trait]
    impl Asker for Scripted {
        async fn ask(&self, questions: Vec<Question>) -> Response {
            *self.asked.lock().unwrap() = questions;
            match &self.answers {
                _ if self.put_off => Response::Deferred,
                Some(answers) => Response::Answered(answers.clone()),
                None => Response::Declined,
            }
        }
    }

    fn question(header: &str) -> Value {
        json!({"header": header, "question": "Which?", "options": [{"label": "A"}, {"label": "B"}]})
    }

    #[tokio::test]
    async fn returns_the_persons_answers_under_each_header() {
        let asker = Arc::new(Scripted {
            answers: Some(vec![Answer::Choice(1), Answer::Own("neither".into())]),
            ..Scripted::default()
        });
        let tool = Ask::new(asker.clone());

        let input = json!({"questions": [question("First"), question("Second")]});
        let text = tool.run(input).await.unwrap();

        assert_eq!(text, "First: B\nSecond: \"neither\" (own answer)");
        assert_eq!(asker.asked.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn declining_tells_the_model_to_wait_for_the_conversation() {
        let tool = Ask::new(Arc::new(Scripted::default()));
        let error = tool
            .run(json!({"questions": [question("One")]}))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), DECLINED);
    }

    #[tokio::test]
    async fn questions_put_off_leave_the_call_without_a_result() {
        let asker = Arc::new(Scripted {
            put_off: true,
            ..Scripted::default()
        });
        let tool = Ask::new(asker);
        let error = tool
            .run(json!({"questions": [question("One")]}))
            .await
            .unwrap_err();
        assert_eq!(error, ToolError::Deferred);
    }

    #[tokio::test]
    async fn refuses_questions_the_person_could_not_answer() {
        let asker = Arc::new(Scripted::default());
        let tool = Ask::new(asker.clone());
        let many_options: Vec<Value> = (0..=MAX_CHOICES).map(|_| json!({"label": "x"})).collect();
        let faults = [
            json!({"questions": []}),
            json!({"questions": vec![question("q"); MAX_QUESTIONS + 1]}),
            json!({"questions": [{"header": "", "question": "Why?"}]}),
            json!({"questions": [{"header": "h", "question": "Why?", "options": many_options}]}),
            json!({"questions": [{"header": "h", "question": "Why?", "recommended": 0}]}),
        ];

        for input in faults {
            assert!(tool.run(input).await.is_err());
        }
        assert!(asker.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn offers_itself_as_ask() {
        let tool = Ask::new(Arc::new(Scripted::default()));
        assert_eq!(tool.spec().name, "ask");
    }
}
