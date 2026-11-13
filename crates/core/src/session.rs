//! Sessions: their logs, what is kept beside them, what they are called, where they are stored,
//! and the instructions they start with.

mod instructions;
mod log;
mod metadata;
mod name;
mod store;

pub use instructions::{SkillListing, skills_prompt, system_prompt, worktree_prompt};
pub use log::{
    AssistantEntry, Entry, ProviderPayload, SessionError, SessionLog, StopReason, ToolCall, Usage,
    first_prompt, is_parked, lock_free_read, put_off_since,
};
pub use metadata::SessionMetadata;
pub use name::{Naming, UNTITLED, known_name, listed_name, window_title};
pub use store::{
    SessionSummary, archive_session, is_named, latest_session, list_sessions, name_session,
    name_unnamed_session, new_session_path, restore_session, session_id, session_path,
    sessions_dir,
};
