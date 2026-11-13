//! The order in which an interface lists its Sessions, shared so that every interface puts the
//! same Session first.
//!
//! Sessions waiting for the person come first, the longest wait first, then Sessions with a Turn
//! running, the longest-running first. An interface keeps one [`Waits`] per Session, open or only
//! stored, and sorts by [`Waits::position`].

use std::time::{Duration, SystemTime};

use crate::open_session::SessionState;

/// How long one Session has been waiting for the person, or working, which places it in the
/// queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waits {
    /// When the Session last started waiting for the person.
    waiting_since: SystemTime,
    /// When the Session's running Turn started, or last carried on after waiting on the person.
    working_since: SystemTime,
    /// When the person was first asked what the Session's Turn asked before a Rebuild put it off,
    /// while that Turn is parked or carrying on to ask it again. Asking again restores it, so the
    /// Session keeps its place in the queue.
    first_asked: Option<SystemTime>,
}

impl Waits {
    /// Starts timing a Session that has been waiting for the person since `since`: the last time
    /// anything happened in a stored Session, or now for one that has only just opened.
    #[must_use]
    pub fn since(since: SystemTime) -> Self {
        Self {
            waiting_since: since,
            working_since: since,
            first_asked: None,
        }
    }

    /// Notes that the Session's Turn parked because a Rebuild put off what it asked the person,
    /// who was first asked at `since`, so that asking again keeps that wait. An interface calls
    /// this for a Session an earlier run left that way, and [`Self::moved`] sees to it otherwise.
    pub fn put_off(&mut self, since: SystemTime) {
        self.first_asked = Some(since);
    }

    /// Restarts the clock that the Session's move from `before` to `after` starts, if any. A
    /// Turn that asks again what a Rebuild put off goes back to waiting since it was first asked.
    pub fn moved(&mut self, before: SessionState, after: SessionState) {
        if after == before {
            return;
        }
        self.restart(before, after);
        self.first_asked = self.still_put_off(before, after);
    }

    /// Restarts the clock the move from `before` to `after` starts, if any. A Session that waits
    /// on the person again after a Rebuild put that off waits since it was first asked instead of
    /// now.
    fn restart(&mut self, before: SessionState, after: SessionState) {
        let now = SystemTime::now();
        if after == SessionState::AwaitingAnswer {
            self.waiting_since = self.first_asked.unwrap_or(now);
        } else if after.is_waiting() {
            self.waiting_since = now;
        } else if before == SessionState::Idle {
            self.working_since = now;
        }
    }

    /// Returns when the person was first asked what a Rebuild put off, if the Session's Turn is
    /// still on its way to asking it again after the move from `before` to `after`.
    fn still_put_off(&self, before: SessionState, after: SessionState) -> Option<SystemTime> {
        match (before, after) {
            (SessionState::AwaitingAnswer, SessionState::Parked) => Some(self.waiting_since),
            (SessionState::Parked, SessionState::Working) => self.first_asked,
            _ => None,
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
    use crate::open_session::SessionState::{AwaitingAnswer, Idle, Parked, Working};

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

        waits.moved(Working, AwaitingAnswer);
        assert_eq!(waits.waited(LONG_AGO), Duration::ZERO);
        waits.moved(AwaitingAnswer, Working);
        assert_eq!(waits.position(Working), (true, working_since));
    }

    #[test]
    fn asking_again_what_a_rebuild_put_off_keeps_the_wait() {
        let mut waits = Waits::since(LONG_AGO);
        waits.moved(Idle, Working);
        waits.moved(Working, AwaitingAnswer);
        let asked = waits.position(AwaitingAnswer);

        waits.moved(AwaitingAnswer, Parked);
        waits.moved(Parked, Working);
        waits.moved(Working, AwaitingAnswer);

        assert_eq!(waits.position(AwaitingAnswer), asked);
        waits.moved(AwaitingAnswer, Working);
        waits.moved(Working, AwaitingAnswer);
        assert!(waits.position(AwaitingAnswer) > asked);
    }

    #[test]
    fn a_wait_put_off_in_an_earlier_run_is_kept_when_asked_again() {
        let mut waits = Waits::since(LONG_AGO + Duration::from_secs(60));
        waits.put_off(LONG_AGO);

        waits.moved(Parked, Working);
        waits.moved(Working, AwaitingAnswer);

        assert_eq!(waits.position(AwaitingAnswer), (false, LONG_AGO));
    }

    #[test]
    fn a_put_off_wait_is_forgotten_once_the_turn_goes_on_without_asking() {
        let mut waits = Waits::since(LONG_AGO);
        waits.put_off(LONG_AGO);

        waits.moved(Parked, Working);
        waits.moved(Working, Parked);
        waits.moved(Parked, Working);
        waits.moved(Working, AwaitingAnswer);

        assert!(waits.position(AwaitingAnswer) > (false, LONG_AGO));
    }

    #[test]
    fn staying_put_changes_nothing() {
        let mut waits = Waits::since(LONG_AGO);
        waits.moved(Idle, Idle);
        assert_eq!(waits, Waits::since(LONG_AGO));
    }
}
