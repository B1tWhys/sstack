//! The order in which an interface lists its Sessions, shared so that every interface puts the
//! same Session first.
//!
//! Sessions waiting for the person come first, the longest wait first, then Sessions with a Turn
//! running, the longest-running first. An interface keeps one [`Waits`] per Session, open or only
//! stored, and sorts by [`Waits::position`].

use std::time::{Duration, SystemTime};

use crate::running_harness::SessionState;

/// How long one Session has been waiting for the person, or working, which places it in the
/// queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waits {
    /// When the Session last started waiting for the person.
    waiting_since: SystemTime,
    /// When the Session's running Turn started, or last carried on after waiting on the person.
    working_since: SystemTime,
}

impl Waits {
    /// Starts timing a Session that has been waiting for the person since `since`: the last time
    /// anything happened in a stored Session, or now for one that has only just opened.
    #[must_use]
    pub fn since(since: SystemTime) -> Self {
        Self {
            waiting_since: since,
            working_since: since,
        }
    }

    /// Restarts the clock that the Session's move from `before` to `after` starts, if any.
    pub fn moved(&mut self, before: SessionState, after: SessionState) {
        if after == before {
            return;
        }
        if after.is_waiting() {
            self.waiting_since = SystemTime::now();
        } else if before == SessionState::Idle {
            self.working_since = SystemTime::now();
        }
    }

    /// Returns a value that sorts the Session, now in `state`, into its place in the queue.
    #[must_use]
    pub fn position(&self, state: SessionState) -> (bool, SystemTime) {
        if state.is_waiting() {
            (false, self.waiting_since)
        } else {
            (true, self.working_since)
        }
    }

    /// Returns how long the Session has been waiting for the person as of `now`. A clock that
    /// went backwards counts as no time at all.
    #[must_use]
    pub fn waited(&self, now: SystemTime) -> Duration {
        now.duration_since(self.waiting_since).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::running_harness::SessionState::{AwaitingApproval, Idle, Working};

    /// A moment well before any test runs.
    const LONG_AGO: SystemTime = SystemTime::UNIX_EPOCH;

    #[test]
    fn waiting_sessions_come_first_and_the_longest_wait_first() {
        let older = Waits::since(LONG_AGO);
        let newer = Waits::since(LONG_AGO + Duration::from_secs(1));

        assert!(older.position(Idle) < newer.position(Idle));
        assert!(newer.position(Idle) < older.position(Working));
    }

    #[test]
    fn starting_work_times_the_turn_and_waiting_again_restarts_the_wait() {
        let mut waits = Waits::since(LONG_AGO);

        waits.moved(Idle, Working);
        let (_, working_since) = waits.position(Working);
        assert!(working_since > LONG_AGO);
        let later = LONG_AGO + Duration::from_secs(5);
        assert_eq!(waits.waited(later), Duration::from_secs(5));

        waits.moved(Working, AwaitingApproval);
        assert_eq!(waits.waited(LONG_AGO), Duration::ZERO);
        waits.moved(AwaitingApproval, Working);
        assert_eq!(waits.position(Working), (true, working_since));
    }

    #[test]
    fn staying_put_changes_nothing() {
        let mut waits = Waits::since(LONG_AGO);
        waits.moved(Idle, Idle);
        assert_eq!(waits, Waits::since(LONG_AGO));
    }
}
