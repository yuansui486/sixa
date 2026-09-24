//! A bounded, FIFO worker queue. Reservations never block an async runtime thread.
use domain::{Error, Result};
use recognition::ocr::OcrRun;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use uuid::Uuid;

const MAX_QUEUED: usize = 128;
pub struct Scheduler {
    inner: Mutex<Inner>,
    changed: Notify,
}
struct Inner {
    limit: usize,
    busy: Vec<bool>,
    raster_busy: bool,
    waiting: VecDeque<Uuid>,
    paused: bool,
    stopping: bool,
    shutdown_generation: u64,
    last_activity: Instant,
}
pub struct Lease {
    pub index: usize,
    raster: bool,
    scheduler: Arc<Scheduler>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.scheduler.inner.lock() {
            inner.busy[self.index] = false;
            if self.raster {
                inner.raster_busy = false;
            }
            inner.last_activity = Instant::now();
        }
        self.scheduler.changed.notify_waiters();
    }
}
struct Ticket {
    id: Uuid,
    scheduler: Arc<Scheduler>,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.scheduler.inner.lock() {
            inner.waiting.retain(|id| *id != self.id);
        }
        self.scheduler.changed.notify_waiters();
    }
}
pub struct Maintenance {
    scheduler: Arc<Scheduler>,
}
impl Drop for Maintenance {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.scheduler.inner.lock() {
            inner.paused = false;
        }
        self.scheduler.changed.notify_waiters();
    }
}
impl Scheduler {
    pub fn new(capacity: usize, limit: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                limit: limit.clamp(1, capacity),
                busy: vec![false; capacity],
                raster_busy: false,
                waiting: VecDeque::new(),
                paused: false,
                stopping: false,
                shutdown_generation: 0,
                last_activity: Instant::now(),
            }),
            changed: Notify::new(),
        })
    }
    pub fn set_limit(&self, limit: usize) -> Result<()> {
        let mut inner = self.inner.lock().map_err(crate::poisoned)?;
        if inner.stopping {
            return Err(shutdown_error());
        }
        inner.limit = limit.clamp(1, inner.busy.len());
        drop(inner);
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn idle_for(&self, duration: Duration) -> bool {
        self.inner.lock().is_ok_and(|inner| {
            !inner.stopping
                && !inner.paused
                && inner.waiting.is_empty()
                && !inner.busy.iter().any(|busy| *busy)
                && inner.last_activity.elapsed() >= duration
        })
    }
    pub fn pause_if_idle(self: &Arc<Self>, duration: Duration) -> Result<Option<Maintenance>> {
        let mut inner = self.inner.lock().map_err(crate::poisoned)?;
        if inner.stopping
            || inner.paused
            || !inner.waiting.is_empty()
            || inner.busy.iter().any(|busy| *busy)
            || inner.last_activity.elapsed() < duration
        {
            return Ok(None);
        }
        inner.paused = true;
        Ok(Some(Maintenance {
            scheduler: self.clone(),
        }))
    }
    #[cfg(test)]
    pub async fn acquire(self: &Arc<Self>, run: Option<&OcrRun>) -> Result<Lease> {
        self.acquire_with_budget(run, false).await
    }
    pub async fn acquire_with_budget(
        self: &Arc<Self>,
        run: Option<&OcrRun>,
        raster: bool,
    ) -> Result<Lease> {
        let id = Uuid::new_v4();
        {
            let mut inner = self.inner.lock().map_err(crate::poisoned)?;
            if inner.stopping {
                return Err(shutdown_error());
            }
            if inner.waiting.len() >= MAX_QUEUED {
                return Err(Error::State(
                    "等待任务过多，请等待现有任务完成后重试".into(),
                ));
            }
            inner.waiting.push_back(id);
        }
        let _ticket = Ticket {
            id,
            scheduler: self.clone(),
        };
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if run.is_some_and(OcrRun::is_cancelled) {
                return Err(Error::State("任务已取消".into()));
            }
            {
                let mut inner = self.inner.lock().map_err(crate::poisoned)?;
                if inner.stopping || !inner.waiting.contains(&id) {
                    return Err(shutdown_error());
                }
                if !inner.paused
                    && inner.waiting.front() == Some(&id)
                    && inner.busy.iter().filter(|busy| **busy).count() < inner.limit
                    && (!raster || !inner.raster_busy)
                    && let Some(index) = inner.busy.iter().position(|busy| !busy)
                {
                    inner.waiting.pop_front();
                    inner.busy[index] = true;
                    if raster {
                        inner.raster_busy = true;
                    }
                    drop(inner);
                    self.changed.notify_waiters();
                    return Ok(Lease {
                        index,
                        raster,
                        scheduler: self.clone(),
                    });
                }
            }
            // OcrRun is shared with native inference; poll only its atomic cancel flag.
            if run.is_some() {
                let _ = tokio::time::timeout(Duration::from_millis(50), notified).await;
            } else {
                notified.await;
            }
        }
    }
    /// Caller serializes maintenance with model_operations. No new jobs enter after pausing.
    pub async fn pause(self: &Arc<Self>) -> Result<Maintenance> {
        let generation = {
            let mut inner = self.inner.lock().map_err(crate::poisoned)?;
            if inner.stopping {
                return Err(shutdown_error());
            }
            inner.paused = true;
            inner.shutdown_generation
        };
        let guard = Maintenance {
            scheduler: self.clone(),
        };
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let inner = self.inner.lock().map_err(crate::poisoned)?;
                if inner.stopping || inner.shutdown_generation != generation {
                    return Err(shutdown_error());
                }
                if !inner.busy.iter().any(|busy| *busy) {
                    return Ok(guard);
                }
            }
            notified.await;
        }
    }

    /// Close admission and invalidate queued jobs and maintenance waiters.
    /// Existing leases remain valid until their jobs finish/cancel and drop them.
    pub fn shutdown(&self) -> Result<()> {
        let mut inner = self.inner.lock().map_err(crate::poisoned)?;
        inner.stopping = true;
        inner.shutdown_generation = inner.shutdown_generation.wrapping_add(1);
        inner.waiting.clear();
        drop(inner);
        self.changed.notify_waiters();
        Ok(())
    }
    /// Cancelling the exit permits fresh jobs; removed queue tickets stay cancelled.
    pub fn resume(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.stopping = false;
        }
        self.changed.notify_waiters();
    }
}

fn shutdown_error() -> Error {
    Error::State("应用正在退出，不能启动新的任务".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immediate_shutdown_resume_rejects_old_waiters_and_runs_fresh_work() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let scheduler = Scheduler::new(1, 1);
                let busy = scheduler.acquire(None).await.unwrap();
                let queued_scheduler = scheduler.clone();
                let queued = tokio::spawn(async move { queued_scheduler.acquire(None).await });
                let maintenance_scheduler = scheduler.clone();
                let maintenance = tokio::spawn(async move { maintenance_scheduler.pause().await });
                tokio::task::yield_now().await;
                assert_eq!(scheduler.inner.lock().unwrap().waiting.len(), 1);
                scheduler.shutdown().unwrap();
                scheduler.resume(); // Deliberately do not let either waiter poll the stopped state.
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), queued)
                        .await
                        .unwrap()
                        .unwrap()
                        .is_err()
                );
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), maintenance)
                        .await
                        .unwrap()
                        .unwrap()
                        .is_err()
                );
                drop(busy);
                let fresh =
                    tokio::time::timeout(Duration::from_millis(100), scheduler.acquire(None))
                        .await
                        .unwrap()
                        .unwrap();
                drop(fresh);
                assert!(scheduler.pause().await.is_ok());
            });
    }

    #[test]
    fn resume_keeps_live_maintenance_exclusive_until_it_finishes() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let scheduler = Scheduler::new(1, 1);
                let maintenance = scheduler.pause().await.unwrap();
                scheduler.shutdown().unwrap();
                scheduler.resume();
                assert!(
                    tokio::time::timeout(Duration::from_millis(10), scheduler.acquire(None))
                        .await
                        .is_err()
                );
                drop(maintenance);
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), scheduler.acquire(None))
                        .await
                        .unwrap()
                        .is_ok()
                );
            });
    }

    #[test]
    fn shutdown_wakes_queue_and_maintenance_without_releasing_busy_work() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let scheduler = Scheduler::new(1, 1);
                let busy = scheduler.acquire(None).await.unwrap();
                let queued_scheduler = scheduler.clone();
                let queued = tokio::spawn(async move { queued_scheduler.acquire(None).await });
                let maintenance_scheduler = scheduler.clone();
                let maintenance = tokio::spawn(async move { maintenance_scheduler.pause().await });
                tokio::task::yield_now().await;
                {
                    let inner = scheduler.inner.lock().unwrap();
                    assert_eq!(inner.waiting.len(), 1);
                    assert!(inner.paused);
                }
                scheduler.shutdown().unwrap();
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), queued)
                        .await
                        .unwrap()
                        .unwrap()
                        .is_err()
                );
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), maintenance)
                        .await
                        .unwrap()
                        .unwrap()
                        .is_err()
                );
                assert!(
                    scheduler.inner.lock().unwrap().busy[0],
                    "shutdown must not prematurely release an active native worker"
                );
                assert!(scheduler.acquire(None).await.is_err());
                assert!(scheduler.pause().await.is_err());
                assert!(scheduler.pause_if_idle(Duration::ZERO).unwrap().is_none());
                assert!(scheduler.set_limit(1).is_err());
                drop(busy);
                assert!(!scheduler.idle_for(Duration::ZERO));
                assert!(scheduler.acquire(None).await.is_err());
                assert!(scheduler.inner.lock().unwrap().waiting.is_empty());
                scheduler.shutdown().unwrap();
            });
    }

    #[test]
    fn dropping_maintenance_after_shutdown_does_not_reopen_admission() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let scheduler = Scheduler::new(1, 1);
                let maintenance = scheduler.pause().await.unwrap();
                scheduler.shutdown().unwrap();
                drop(maintenance);
                assert!(scheduler.acquire(None).await.is_err());
            });
    }

    #[test]
    fn raster_work_has_one_global_slot_even_with_four_workers() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let scheduler = Scheduler::new(4, 4);
                let raster = scheduler.acquire_with_budget(None, true).await.unwrap();
                let text = scheduler.acquire_with_budget(None, false).await.unwrap();
                assert_ne!(raster.index, text.index);
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(20),
                        scheduler.acquire_with_budget(None, true)
                    )
                    .await
                    .is_err()
                );
                drop(raster);
                let next = tokio::time::timeout(
                    Duration::from_millis(100),
                    scheduler.acquire_with_budget(None, true),
                )
                .await
                .unwrap()
                .unwrap();
                drop(next);
                drop(text);
            });
    }
    #[test]
    fn leases_enforce_limit_and_cancel_queued_work() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let scheduler = Scheduler::new(4, 1);
                let first = scheduler.acquire(None).await.unwrap();
                let run = OcrRun::new().unwrap();
                run.cancel().unwrap();
                assert!(scheduler.acquire(Some(&run)).await.is_err());
                scheduler.set_limit(2).unwrap();
                let second = scheduler.acquire(None).await.unwrap();
                assert_ne!(first.index, second.index);
                scheduler.set_limit(1).unwrap();
                assert!(
                    tokio::time::timeout(Duration::from_millis(20), scheduler.acquire(None))
                        .await
                        .is_err()
                );
                drop(first);
                assert!(
                    tokio::time::timeout(Duration::from_millis(20), scheduler.acquire(None))
                        .await
                        .is_err()
                );
                drop(second);
                assert!(scheduler.acquire(None).await.is_ok());
            });
    }
    #[test]
    fn maintenance_stops_new_jobs_and_resumes_on_drop() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let scheduler = Scheduler::new(2, 2);
                let pause = scheduler.pause().await.unwrap();
                assert!(
                    tokio::time::timeout(Duration::from_millis(20), scheduler.acquire(None))
                        .await
                        .is_err()
                );
                drop(pause);
                assert!(scheduler.acquire(None).await.is_ok());
            });
    }
    #[test]
    fn cancelling_a_waiting_job_does_not_wait_for_the_busy_worker() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let scheduler = Scheduler::new(1, 1);
                let _busy = scheduler.acquire(None).await.unwrap();
                let run = OcrRun::new().unwrap();
                let cancellation = run.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    cancellation.cancel().unwrap();
                });
                let outcome =
                    tokio::time::timeout(Duration::from_millis(300), scheduler.acquire(Some(&run)))
                        .await
                        .unwrap();
                assert!(outcome.is_err());
                assert!(scheduler.inner.lock().unwrap().waiting.is_empty());
            });
    }
    #[test]
    fn idle_unload_reservation_is_atomic_with_job_submission() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let scheduler = Scheduler::new(1, 1);
                let busy = scheduler.acquire(None).await.unwrap();
                assert!(scheduler.pause_if_idle(Duration::ZERO).unwrap().is_none());
                drop(busy);
                let pause = scheduler.pause_if_idle(Duration::ZERO).unwrap().unwrap();
                assert!(
                    tokio::time::timeout(Duration::from_millis(10), scheduler.acquire(None))
                        .await
                        .is_err()
                );
                drop(pause);
                assert!(scheduler.acquire(None).await.is_ok());
            });
    }
}
