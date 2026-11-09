//! Archives that run in the background, so that deleting a worktree, which can take seconds, never
//! holds up the screen.

use std::mem;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::task::JoinHandle;

/// The runner for archives in the background, which the harness waits on before it exits or
/// relaunches, so that neither cuts a deletion off halfway.
///
/// Clones share one queue. Archives run one at a time, because git can refuse to delete a branch
/// while another git changes the project's branches.
#[derive(Debug, Clone, Default)]
pub struct Archiver {
    /// The lock an archive holds while it runs.
    one_at_a_time: Arc<Mutex<()>>,
    /// Every archive started and not yet waited for, finished or not.
    started: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Archiver {
    /// Starts `archive` on a thread where it may block. Returns at once.
    pub fn start(&self, archive: impl FnOnce() + Send + 'static) {
        let lock = Arc::clone(&self.one_at_a_time);
        let handle = tokio::task::spawn_blocking(move || {
            let _held = lock.lock().unwrap_or_else(PoisonError::into_inner);
            archive();
        });
        let mut started = self.started.lock().unwrap_or_else(PoisonError::into_inner);
        started.retain(|handle| !handle.is_finished());
        started.push(handle);
    }

    /// Waits until every archive started so far has finished.
    pub async fn finish(&self) {
        let started = mem::take(&mut *self.started.lock().unwrap_or_else(PoisonError::into_inner));
        for handle in started {
            let _ = handle.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[tokio::test]
    async fn an_archive_runs_in_the_background_until_waited_for() {
        let archiver = Archiver::default();
        let (go, wait) = mpsc::channel::<()>();
        let done = Arc::new(Mutex::new(false));
        let marked = Arc::clone(&done);

        archiver.start(move || {
            wait.recv().unwrap();
            *marked.lock().unwrap() = true;
        });
        assert!(!*done.lock().unwrap());

        go.send(()).unwrap();
        archiver.finish().await;
        assert!(*done.lock().unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn archives_run_one_at_a_time() {
        let archiver = Archiver::default();
        let running = Arc::new(Mutex::new((0, 0)));
        for _ in 0..3 {
            let running = Arc::clone(&running);
            archiver.start(move || {
                let mut counts = running.lock().unwrap();
                counts.0 += 1;
                counts.1 = counts.1.max(counts.0);
                drop(counts);
                std::thread::sleep(Duration::from_millis(20));
                running.lock().unwrap().0 -= 1;
            });
        }
        archiver.finish().await;
        assert_eq!(*running.lock().unwrap(), (0, 1));
    }
}
