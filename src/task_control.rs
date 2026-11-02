use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Clone, Debug, Default)]
pub struct TaskControl {
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
}

impl TaskControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Wait at a safe work boundary. Cancellation always releases a paused worker.
    pub fn checkpoint(&self) -> bool {
        while self.is_paused() && !self.is_cancelled() {
            thread::sleep(Duration::from_millis(50));
        }
        !self.is_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use super::TaskControl;
    use std::{thread, time::Duration};

    #[test]
    fn paused_worker_resumes_at_the_next_checkpoint() {
        let control = TaskControl::default();
        control.set_paused(true);
        let resume_control = control.clone();
        let resumer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            resume_control.set_paused(false);
        });
        let started = std::time::Instant::now();

        assert!(control.checkpoint());
        assert!(started.elapsed() >= Duration::from_millis(20));
        resumer.join().unwrap();
    }

    #[test]
    fn cancellation_releases_a_paused_worker() {
        let control = TaskControl::default();
        control.set_paused(true);
        let cancel_control = control.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancel_control.cancel();
        });

        assert!(!control.checkpoint());
        canceller.join().unwrap();
    }
}
