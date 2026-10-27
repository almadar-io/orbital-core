//! A row-set reference resolved through a host-answered store (EXEC-RENDER-3): the same `@read`-checked
//! resolve as [`RuntimeKernel::resolve_rows`], run as a resumable step so each read it makes is a request the
//! host answers. The step replays with the recorded answers, as a dispatch does.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::*;
use crate::resumable::{
    accept_into, accept_replies, number_requests, DispatchError, Exchange, IoAnswer, IoReply, IoRequest, Progress,
    ResolveContinuation, ResolveProgress, StoreLeg,
};

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
            store: None,
            seeds: None,
        })
    }

    /// Go on with a resolve that returned [`ResolveProgress::Await`], given an answer to each of its requests.
    pub fn resume_resolve_rows(
        &self,
        mut continuation: ResolveContinuation,
        replies: Vec<IoReply>,
    ) -> Result<ResolveProgress, DispatchError> {
        let check = |request: &IoRequest, answer: IoAnswer| self.checked_answer(request, answer);
        match continuation.store.as_mut() {
            Some(leg) => accept_into(&mut leg.driver, replies, &check)?,
            None => continuation
                .journal
                .extend(accept_replies(&continuation.pending, replies, &check)?),
        }
        continuation.pending.clear();
        self.drive_resolve(continuation)
    }

    fn drive_resolve(&self, mut c: ResolveContinuation) -> Result<ResolveProgress, DispatchError> {
        loop {
            let seeds = c.seeds.get_or_insert_with(|| self.pending_store_seeds());
            if c.store.is_none() {
                if let Some((origin, ask)) = seeds.pop_front() {
                    c.store = self.seed_leg_for(origin, ask);
                    continue;
                }
            }
            let Some(leg) = c.store.take() else { break };
            let StoreLeg { ask, driver, seed } = *leg;
            match self.drive_resumable(driver)? {
                Progress::Await {
                    continuation,
                    requests,
                } => {
                    c.pending = requests.clone();
                    c.store = Some(Box::new(StoreLeg {
                        ask,
                        driver: continuation,
                        seed,
                    }));
                    return Ok(ResolveProgress::Await {
                        continuation: c,
                        requests,
                    });
                }
                Progress::Done(outcomes) => {
                    let answer = self.store_reply(&ask, &outcomes);
                    match seed {
                        Some(origin) => self.seed_landed(&ask, &answer, &origin),
                        None => c.journal.push(Exchange::new(ask.request, answer)),
                    }
                }
            }
        }
        let journal = std::mem::take(&mut c.journal);
        let (result, end) = self
            .io
            .run_step(journal, || self.resolve_rows(&c.reference, &c.viewer));
        if let Some(diverged) = end.diverged {
            return Err(DispatchError::Resume(diverged));
        }
        if let Some(ask) = end.store_asks.into_iter().next() {
            if result.is_ok() {
                return Err(DispatchError::Kernel(KernelError::Internal(
                    "a resolve committed while it awaited its store driver".to_string(),
                )));
            }
            c.journal = end.journal;
            c.store = match self.store_driver_run(&ask) {
                Ok(driver) => Some(Box::new(StoreLeg {
                    ask,
                    driver,
                    seed: None,
                })),
                Err(message) => {
                    let answer = Self::store_failed(&ask, message);
                    c.journal.push(Exchange::new(ask.request, answer));
                    None
                }
            };
            return self.drive_resolve(c);
        }
        if !end.pending.is_empty() {
            if result.is_ok() {
                return Err(DispatchError::Kernel(KernelError::Internal(
                    "a resolve committed while it awaited host I/O".to_string(),
                )));
            }
            c.journal = end.journal;
            c.pending = number_requests(&mut c.next_id, end.pending);
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
