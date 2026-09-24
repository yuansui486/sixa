//! One replaceable draft preview; queued obsolete renders never compete with the current page.
use domain::{Error, Result};
use recognition::ocr::OcrRun;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Default)]
pub struct PreviewJobs {
    inner: Mutex<Inner>,
}
#[derive(Default)]
struct Inner {
    current: Option<(Uuid, OcrRun)>,
    cancelled: VecDeque<Uuid>,
}
pub struct Ticket {
    id: Uuid,
    run: OcrRun,
    owner: Arc<PreviewJobs>,
}
impl PreviewJobs {
    pub fn start(self: &Arc<Self>, id: Uuid) -> Result<Ticket> {
        let mut inner = self.inner.lock().map_err(crate::poisoned)?;
        if inner.cancelled.contains(&id) {
            return Err(Error::State("预览已取消".into()));
        }
        if let Some((_, previous)) = inner.current.take() {
            previous.cancel()?;
        }
        let run = OcrRun::new()?;
        inner.current = Some((id, run.clone()));
        Ok(Ticket {
            id,
            run,
            owner: self.clone(),
        })
    }
    pub fn cancel(&self, id: Uuid) -> Result<()> {
        let mut inner = self.inner.lock().map_err(crate::poisoned)?;
        if let Some((current, run)) = &inner.current
            && *current == id
        {
            run.cancel()?;
        }
        if !inner.cancelled.contains(&id) {
            inner.cancelled.push_back(id);
        }
        while inner.cancelled.len() > 128 {
            inner.cancelled.pop_front();
        }
        Ok(())
    }
    pub fn cancel_all(&self) {
        if let Ok(inner) = self.inner.lock()
            && let Some((_, run)) = &inner.current
        {
            let _ = run.cancel();
        }
    }
}
impl Ticket {
    pub fn run(&self) -> OcrRun {
        self.run.clone()
    }
    pub fn check(&self) -> Result<()> {
        if self.run.is_cancelled() {
            Err(Error::State("预览已取消".into()))
        } else {
            Ok(())
        }
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        let _ = self.run.cancel();
        if let Ok(mut inner) = self.owner.inner.lock()
            && inner.current.as_ref().is_some_and(|(id, _)| *id == self.id)
        {
            inner.current = None;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacing_a_preview_cancels_old_work_without_removing_new_work() {
        let jobs = Arc::new(PreviewJobs::default());
        let first = jobs.start(Uuid::new_v4()).unwrap();
        let next_id = Uuid::new_v4();
        let second = jobs.start(next_id).unwrap();
        assert!(first.check().is_err());
        drop(first);
        assert!(second.check().is_ok());
        jobs.cancel(next_id).unwrap();
        assert!(second.check().is_err());
    }
    #[test]
    fn cancelling_before_admission_and_shutdown_are_observed() {
        let jobs = Arc::new(PreviewJobs::default());
        let id = Uuid::new_v4();
        jobs.cancel(id).unwrap();
        assert!(jobs.start(id).is_err());
        let ticket = jobs.start(Uuid::new_v4()).unwrap();
        jobs.cancel_all();
        assert!(ticket.check().is_err());
        drop(ticket);
        assert!(jobs.inner.lock().unwrap().current.is_none());
    }
}
