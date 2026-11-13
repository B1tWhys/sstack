//! What a person can run by typing `/`: Human Commands, which the harness acts on itself, and
//! Skills, whose instructions go to the model.

mod human;
mod skills;

pub use human::{Command, parse_command};
pub use skills::{
    Location, Lookup, Scope, Skill, claude_directory, discover_skills, find_skill, load_skill,
    skill_locations,
};
