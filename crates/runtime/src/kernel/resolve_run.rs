//! A row-set reference resolved through a host-answered store (EXEC-RENDER-3): the same `@read`-checked
//! resolve as [`RuntimeKernel::resolve_rows`], run as a resumable step so each read it makes is a request the
//! host answers. The step replays with the recorded answers, as a dispatch does.

use super::*;
use crate::resumable::{accept_replies, DispatchError, IoReply, PendingRequest, RequestId, ResolveContinuation, ResolveProgress};

impl RuntimeKernel {
    /// [`Self::resolve_rows`] as a resumable step: it returns [`ResolveProgress::Await`] at the first read the
    /// host has not answered, and [`ResolveProgress::Done`] with the page once every read is answered.
    pub fn start_resolve_rows(
        &self,
        reference: &RowSetRef,
        viewer: &Value,
    ) -> Result<ResolveProgress, DispatchError> {
        self.drive_resolve(ResolveContinuation {
            reference: reference.clone(),
            viewer: viewer.clone(),
            journal: Vec::new(),
            pending: Vec::new(),
            next_id: 0,
        })
    }

    /// Go on with a resolve that returned [`ResolveProgress::Await`], given an answer to each of its requests.
    pub fn resume_resolve_rows(
        &self,
        mut continuation: ResolveContinuation,
        replies: Vec<IoReply>,
    ) -> Result<ResolveProgress, DispatchError> {
        let answered = accept_replies(&continuation.pending, replies)?;
        continuation.journal.extend(answered);
        continuation.pending.clear();
        self.drive_resolve(continuation)
    }

    fn drive_resolve(&self, mut c: ResolveContinuation) -> Result<ResolveProgress, DispatchError> {
        let journal = std::mem::take(&mut c.journal);
        let (result, end) = self
            .io
            .run_step(journal, || self.resolve_rows(&c.reference, &c.viewer));
        if let Some(diverged) = end.diverged {
            return Err(DispatchError::Resume(diverged));
        }
        if let Some(request) = end.pending {
            if result.is_ok() {
                return Err(DispatchError::Kernel(KernelError::Internal(
                    "a resolve committed while it awaited host I/O".to_string(),
                )));
            }
            c.journal = end.journal;
            let id = RequestId::from_raw(c.next_id);
            c.next_id += 1;
            c.pending = vec![PendingRequest { id, request }];
            let requests = c.pending.clone();
            return Ok(ResolveProgress::Await {
                continuation: c,
                requests,
            });
        }
        result
            .map(ResolveProgress::Done)
            .map_err(|e| DispatchError::Kernel(KernelError::Internal(e)))
    }
}
