use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Clone, Debug, Default)]
pub struct TaskControl {
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    pause_generation: Arc<AtomicU64>,
    pause_ack_generation: Arc<AtomicU64>,
}

impl TaskControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn set_paused(&self, paused: bool) {
        if paused {
            if !self.paused.load(Ordering::SeqCst) {
                self.pause_generation.fetch_add(1, Ordering::SeqCst);
            }
            self.paused.store(true, Ordering::SeqCst);
        } else {
            self.paused.store(false, Ordering::SeqCst);
        }
    }

    pub fn is_paused_at_boundary(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
            && self.pause_ack_generation.load(Ordering::SeqCst)
                == self.pause_generation.load(Ordering::SeqCst)
    }

    /// Wait at a safe work boundary. Cancellation always releases a paused worker.
    pub fn checkpoint(&self) -> bool {
        loop {
            if self.is_cancelled() {
                return false;
            }
            if !self.paused.load(Ordering::SeqCst) {
                return true;
            }
            let generation = self.pause_generation.load(Ordering::SeqCst);
            self.pause_ack_generation
                .store(generation, Ordering::SeqCst);
            while self.paused.load(Ordering::SeqCst)
                && self.pause_generation.load(Ordering::SeqCst) == generation
                && !self.is_cancelled()
            {
                thread::sleep(Duration::from_millis(50));
            }
        }
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

    #[test]
    fn reports_when_a_paused_worker_reaches_a_safe_boundary() {
        let control = TaskControl::default();
        control.set_paused(true);
        let worker_control = control.clone();
        let worker = thread::spawn(move || worker_control.checkpoint());

        let started = std::time::Instant::now();
        while !control.is_paused_at_boundary() && started.elapsed() < Duration::from_secs(1) {
            thread::sleep(Duration::from_millis(1));
        }
        let reached_boundary = control.is_paused_at_boundary();
        control.set_paused(false);

        assert!(reached_boundary);
        assert!(worker.join().unwrap());
        assert!(!control.is_paused_at_boundary());
        control.set_paused(true);
        assert!(!control.is_paused_at_boundary());
    }
}
