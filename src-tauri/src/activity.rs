use domain::{Error, Result};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Inner {
    stopping: bool,
    cancellation_started: bool,
    active: usize,
    shutdown_generation: u64,
}
#[derive(Default)]
pub struct Activity {
    inner: Mutex<Inner>,
}
pub struct Guard {
    activity: Arc<Activity>,
    generation: u64,
}
impl Activity {
    pub fn admit(self: &Arc<Self>) -> Result<Guard> {
        let mut inner = self.inner.lock().map_err(crate::poisoned)?;
        if inner.stopping {
            return Err(Error::State("应用正在退出，不能启动新的任务".into()));
        }
        inner.active += 1;
        Ok(Guard {
            activity: self.clone(),
            generation: inner.shutdown_generation,
        })
    }
    // An admitted operation can still finalize after admission closes. Once
    // the activity count reaches zero, shutdown must see a stable idle state:
    // a late IPC request must not resurrect writes behind its idle check.
    pub fn track(self: &Arc<Self>) -> Result<Guard> {
        let mut inner = self.inner.lock().map_err(crate::poisoned)?;
        if inner.stopping && inner.active == 0 {
            return Err(Error::State("应用正在退出，保存操作已结束".into()));
        }
        inner.active += 1;
        Ok(Guard {
            activity: self.clone(),
            generation: inner.shutdown_generation,
        })
    }
    pub fn is_stopping(&self) -> bool {
        self.inner.lock().map_or(true, |i| i.stopping)
    }
    pub fn begin_shutdown(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !inner.cancellation_started {
            inner.shutdown_generation = inner.shutdown_generation.wrapping_add(1);
            inner.cancellation_started = true;
        }
        inner.stopping = true;
    }
    /// Only freeze if already idle: probing must not reject substeps of existing work.
    pub fn freeze_if_idle(&self) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.active != 0 {
            return false;
        }
        inner.stopping = true;
        true
    }
    pub fn resume(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.stopping = false;
        inner.cancellation_started = false;
    }
    pub fn active_count(&self) -> usize {
        self.inner.lock().map_or(usize::MAX, |i| i.active)
    }
}
impl Guard {
    /// Long operations waiting on a model mutex must not restart after the user
    /// cancels shutdown. Completion writes use tracking and need no such check.
    pub fn ensure_current(&self) -> Result<()> {
        let inner = self.activity.inner.lock().map_err(crate::poisoned)?;
        if inner.cancellation_started || inner.shutdown_generation != self.generation {
            return Err(Error::State("任务已因退出请求取消，请重新操作".into()));
        }
        Ok(())
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.activity
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_check_does_not_cancel_work_without_confirmation() {
        let activity = Arc::new(Activity::default());
        let running = activity.admit().unwrap();
        assert!(!activity.freeze_if_idle());
        assert!(activity.admit().is_ok());
        assert!(running.ensure_current().is_ok());
        activity.resume();
        assert!(running.ensure_current().is_ok());
        activity.begin_shutdown();
        assert!(running.ensure_current().is_err());
        drop(running);
        activity.resume();
        assert!(activity.freeze_if_idle());
        assert!(activity.admit().is_err());
    }
    #[test]
    fn shutdown_keeps_finalization_but_rejects_new_jobs() {
        let activity = Arc::new(Activity::default());
        let job = activity.admit().unwrap();
        activity.begin_shutdown();
        assert!(activity.admit().is_err());
        let finalization = activity.track().unwrap();
        drop(job);
        assert_eq!(activity.active_count(), 1);
        drop(finalization);
        assert_eq!(activity.active_count(), 0);
        assert!(activity.track().is_err());
        activity.resume();
        assert!(activity.admit().is_ok());
    }

    #[test]
    fn cancellation_reopens_tracking_without_resetting_running_guards() {
        let activity = Arc::new(Activity::default());
        let running = activity.admit().unwrap();
        activity.begin_shutdown();
        activity.resume();
        let fresh = activity.admit().unwrap();
        assert!(running.ensure_current().is_err());
        assert!(fresh.ensure_current().is_ok());
        assert_eq!(activity.active_count(), 2);
        drop(running);
        assert_eq!(activity.active_count(), 1);
        drop(fresh);
        assert_eq!(activity.active_count(), 0);
        assert!(activity.track().is_ok());
    }

    #[test]
    fn tracking_race_with_last_completion_never_resurrects_a_drained_shutdown() {
        for _ in 0..64 {
            let activity = Arc::new(Activity::default());
            let running = activity.admit().unwrap();
            activity.begin_shutdown();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let tracker = activity.clone();
            let worker_barrier = barrier.clone();
            let worker = std::thread::spawn(move || {
                worker_barrier.wait();
                tracker.track()
            });
            barrier.wait();
            drop(running);
            let continuation = worker.join().unwrap();
            if continuation.is_ok() {
                assert_eq!(activity.active_count(), 1);
            }
            drop(continuation);
            assert_eq!(activity.active_count(), 0);
            assert!(activity.track().is_err());
            assert!(activity.admit().is_err());
        }
    }

    #[test]
    fn abandoning_an_async_wait_does_not_finish_native_work_early() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let activity = Arc::new(Activity::default());
                let guard = activity.admit().unwrap();
                let (release, wait) = std::sync::mpsc::channel();
                let (done, finished) = tokio::sync::oneshot::channel();
                let native = tokio::task::spawn_blocking(move || {
                    let _guard = guard;
                    wait.recv().unwrap();
                    drop(_guard);
                    let _ = done.send(());
                });
                drop(native);
                activity.begin_shutdown();
                assert_eq!(activity.active_count(), 1);
                release.send(()).unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(2), finished)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(activity.active_count(), 0);
                assert!(activity.track().is_err());
            });
    }
}
