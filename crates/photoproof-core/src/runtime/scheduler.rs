//! The pass scheduler (spec/RUNTIME.md §9): one llama-server, two lanes,
//! the app side owns every request.
//!
//! - `Lane::Interactive` (query parse): queue depth 1, head-of-line; if
//!   it would otherwise wait more than **250 ms**, the background slot's
//!   current request is *cancelled* (HTTP disconnect — llama frees the
//!   slot) and re-queued.
//! - `Lane::Background` (summaries, captions): single in-flight, bounded
//!   `max_tokens`, **STREAM-ONLY** — encoded in the types: the backend
//!   seam only accepts a [`BackgroundJob`], which can only be
//!   constructed streaming-bounded. A non-streaming background request
//!   is unrepresentable.
//! - **Honesty clause**: prompt processing is NOT preemptible —
//!   disconnect-cancellation takes effect during token generation; the
//!   scripted backend in the §13.8 test models the non-preemptible
//!   prompt window and the worst case is timed explicitly on the fake
//!   clock.
//! - **Mic armed ⇒ background passes hold**: in-flight items finish
//!   (bounded), nothing new starts; unpause at disarm + 5 s.
//! - Mid-call crash: the caller's request retries **exactly once** after
//!   the supervisor reports Ready again (§13.2).
//! - GC takes this scheduler's lock (§11): a model with in-flight or
//!   queued work is untouchable; while a GC hold is out, no background
//!   work starts.
//!
//! Tick-driven through the Clock seam — no sleeps in decision logic.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use photoproof_connectors::llm::{ChatRequest, ChatResponse, Lane};
use photoproof_connectors::openai::OpenAiCompatClient;
use photoproof_connectors::{ConnectorError, ConnectorResult};

use crate::capture::Clock;

pub type JobId = u64;

/// §9: interactive cancels the background slot if it would otherwise
/// wait more than 250 ms.
pub const INTERACTIVE_CANCEL_WAIT_MS: u64 = 250;
/// §9: unpause background passes at mic disarm + 5 s.
pub const MIC_UNPAUSE_DELAY_MS: u64 = 5_000;
/// §9: background prompts are kept deliberately small; max_tokens is
/// bounded at construction.
pub const BACKGROUND_MAX_TOKENS: u32 = 512;

/// A background-lane request. The ONLY way to construct one bounds
/// `max_tokens` and fixes the lane; the backend seam's signature then
/// guarantees `stream: true` — the §9 honesty clauses, unrepresentable
/// to violate.
#[derive(Debug, Clone)]
pub struct BackgroundJob {
    req: ChatRequest,
    /// Model the work belongs to (GC untouchability, §11/§13.9).
    model_id: String,
}

impl BackgroundJob {
    pub fn new(mut req: ChatRequest, model_id: impl Into<String>) -> Self {
        req.max_tokens = req.max_tokens.min(BACKGROUND_MAX_TOKENS);
        req.priority = Lane::Background;
        Self {
            req,
            model_id: model_id.into(),
        }
    }

    pub fn request(&self) -> &ChatRequest {
        &self.req
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}

/// The two-lane slot backend seam. The real implementation
/// ([`LlmSlotBackend`]) maps onto llama-server's `--parallel 2` slots
/// through the OpenAI client; tests script timing on the fake clock.
pub trait SlotBackend: Send {
    fn begin_interactive(&mut self, id: JobId, req: &ChatRequest, now_ms: u64);
    /// Background = [`BackgroundJob`] only (stream-only by construction).
    fn begin_background(&mut self, id: JobId, job: &BackgroundJob, now_ms: u64);
    fn poll(&mut self, id: JobId, now_ms: u64) -> Option<ConnectorResult<ChatResponse>>;
    /// HTTP disconnect. Effective only during token generation — a
    /// request mid-prompt-processing runs that phase out (§9).
    fn cancel(&mut self, id: JobId, now_ms: u64);
}

/// Debug-panel scheduler decisions (§9 observability; §13.8 "observable
/// in the debug panel").
#[derive(Debug, Clone, PartialEq)]
pub enum SchedulerEvent {
    InteractiveStarted {
        id: JobId,
        at_ms: u64,
    },
    BackgroundStarted {
        id: JobId,
        at_ms: u64,
    },
    /// The 250 ms rule fired: background cancelled for an interactive.
    BackgroundCancelledForInteractive {
        id: JobId,
        at_ms: u64,
    },
    BackgroundRequeued {
        id: JobId,
        at_ms: u64,
    },
    /// Mic armed: background passes held.
    HeldForMic {
        at_ms: u64,
    },
    UnpausedAfterDisarm {
        at_ms: u64,
    },
    /// §13.2: one automatic retry after Ready.
    RetriedOnce {
        id: JobId,
        at_ms: u64,
    },
    /// §9: ort embedder GPU→CPU fallback, recorded for the debug panel.
    EmbedderFallback {
        detail: String,
        at_ms: u64,
    },
    GcHoldTaken {
        model_id: String,
        at_ms: u64,
    },
    GcHoldReleased {
        model_id: String,
        at_ms: u64,
    },
    GcRefused {
        model_id: String,
        reason: String,
        at_ms: u64,
    },
}

struct ActiveJob {
    id: JobId,
    /// Background job kept for requeue-on-cancel.
    job: Option<BackgroundJob>,
    /// Interactive request kept for the §13.2 single retry.
    req: Option<ChatRequest>,
    retried: bool,
    cancel_requested: bool,
}

const EVENT_CAP: usize = 256;

pub struct PassScheduler<C: Clock, B: SlotBackend> {
    clock: C,
    backend: B,
    next_id: JobId,
    llm_ready: bool,
    interactive_queue: Option<(JobId, ChatRequest, bool)>,
    interactive_active: Option<ActiveJob>,
    background_queue: VecDeque<(JobId, BackgroundJob, bool)>,
    background_active: Option<ActiveJob>,
    /// Set when an interactive arrives with background in flight.
    cancel_deadline: Option<u64>,
    mic_armed: bool,
    unpause_at: Option<u64>,
    gc_hold: Option<String>,
    completed: Vec<(JobId, ConnectorResult<ChatResponse>)>,
    events: VecDeque<SchedulerEvent>,
}

impl<C: Clock, B: SlotBackend> PassScheduler<C, B> {
    pub fn new(clock: C, backend: B) -> Self {
        Self {
            clock,
            backend,
            next_id: 1,
            llm_ready: false,
            interactive_queue: None,
            interactive_active: None,
            background_queue: VecDeque::new(),
            background_active: None,
            cancel_deadline: None,
            mic_armed: false,
            unpause_at: None,
            gc_hold: None,
            completed: Vec::new(),
            events: VecDeque::new(),
        }
    }

    // ------------------------------------------------------------ inputs ----

    /// Supervisor readiness (§8.3: background passes gate on their
    /// dependencies; nothing schedules while not Ready).
    pub fn set_llm_ready(&mut self, ready: bool) {
        self.llm_ready = ready;
    }

    /// §9: mic armed ⇒ hold background passes; disarm starts the 5 s
    /// unpause timer.
    pub fn set_mic_armed(&mut self, armed: bool) {
        let now = self.clock.mono_ms();
        if armed && !self.mic_armed {
            self.push_event(SchedulerEvent::HeldForMic { at_ms: now });
            self.unpause_at = None;
        }
        if !armed && self.mic_armed {
            self.unpause_at = Some(now + MIC_UNPAUSE_DELAY_MS);
        }
        self.mic_armed = armed;
    }

    /// Interactive lane: queue depth 1, head-of-line. A newer submission
    /// displaces a still-queued one (the displaced job completes as
    /// Cancelled — search-as-you-type supersedes itself).
    pub fn submit_interactive(&mut self, mut req: ChatRequest) -> JobId {
        req.priority = Lane::Interactive;
        let id = self.next_id;
        self.next_id += 1;
        if let Some((old_id, _, _)) = self.interactive_queue.take() {
            self.completed
                .push((old_id, Err(ConnectorError::Cancelled)));
        }
        self.interactive_queue = Some((id, req, false));
        // The 250 ms rule arms the moment an interactive request exists
        // with the background slot occupied.
        if self.background_active.is_some() && self.cancel_deadline.is_none() {
            self.cancel_deadline = Some(self.clock.mono_ms() + INTERACTIVE_CANCEL_WAIT_MS);
        }
        id
    }

    pub fn submit_background(&mut self, job: BackgroundJob) -> JobId {
        let id = self.next_id;
        self.next_id += 1;
        self.background_queue.push_back((id, job, false));
        id
    }

    /// §9: the ort embedder's GPU→CPU fallback is recorded for the debug
    /// panel.
    pub fn note_embedder_fallback(&mut self, detail: impl Into<String>) {
        let at_ms = self.clock.mono_ms();
        self.push_event(SchedulerEvent::EmbedderFallback {
            detail: detail.into(),
            at_ms,
        });
    }

    // ---------------------------------------------------------------- GC ----

    /// §11: GC takes the scheduler lock. Refused while the model has any
    /// in-flight or queued work (untouchable, §13.9); while the hold is
    /// out, no background work starts.
    pub fn try_gc_hold(&mut self, model_id: &str) -> bool {
        let now = self.clock.mono_ms();
        let busy_active = self
            .background_active
            .as_ref()
            .and_then(|a| a.job.as_ref())
            .is_some_and(|j| j.model_id == model_id);
        let busy_queued = self
            .background_queue
            .iter()
            .any(|(_, j, _)| j.model_id == model_id);
        if busy_active || busy_queued {
            self.push_event(SchedulerEvent::GcRefused {
                model_id: model_id.to_owned(),
                reason: if busy_active {
                    "in-flight work".into()
                } else {
                    "queued work".into()
                },
                at_ms: now,
            });
            return false;
        }
        if self.gc_hold.is_some() {
            self.push_event(SchedulerEvent::GcRefused {
                model_id: model_id.to_owned(),
                reason: "another GC hold is out".into(),
                at_ms: now,
            });
            return false;
        }
        self.gc_hold = Some(model_id.to_owned());
        self.push_event(SchedulerEvent::GcHoldTaken {
            model_id: model_id.to_owned(),
            at_ms: now,
        });
        true
    }

    pub fn release_gc_hold(&mut self) {
        if let Some(model_id) = self.gc_hold.take() {
            let at_ms = self.clock.mono_ms();
            self.push_event(SchedulerEvent::GcHoldReleased { model_id, at_ms });
        }
    }

    pub fn has_work_for(&self, model_id: &str) -> bool {
        self.background_active
            .as_ref()
            .and_then(|a| a.job.as_ref())
            .is_some_and(|j| j.model_id == model_id)
            || self
                .background_queue
                .iter()
                .any(|(_, j, _)| j.model_id == model_id)
    }

    // ----------------------------------------------------------- outputs ----

    pub fn take_completed(&mut self) -> Vec<(JobId, ConnectorResult<ChatResponse>)> {
        std::mem::take(&mut self.completed)
    }

    pub fn events(&self) -> impl Iterator<Item = &SchedulerEvent> {
        self.events.iter()
    }

    pub fn background_held(&self) -> bool {
        let now = self.clock.mono_ms();
        self.mic_armed || self.unpause_at.is_some_and(|t| now < t) || self.gc_hold.is_some()
    }

    pub fn queue_depths(&self) -> (usize, usize) {
        (
            usize::from(self.interactive_queue.is_some())
                + usize::from(self.interactive_active.is_some()),
            self.background_queue.len() + usize::from(self.background_active.is_some()),
        )
    }

    // -------------------------------------------------------------- tick ----

    pub fn tick(&mut self) {
        let now = self.clock.mono_ms();
        self.collect_interactive(now);
        self.collect_background(now);
        self.enforce_cancel_rule(now);
        self.start_interactive(now);
        self.start_background(now);
    }

    fn collect_interactive(&mut self, now: u64) {
        let Some(active) = &self.interactive_active else {
            return;
        };
        let id = active.id;
        if let Some(result) = self.backend.poll(id, now) {
            let active = self.interactive_active.take().expect("checked above");
            match result {
                Err(ConnectorError::ConnectionLost(_)) if !active.retried => {
                    // §13.2: retry exactly once after Ready (the
                    // supervisor restarts; readiness re-gates the start).
                    // A newer queued interactive supersedes the retry —
                    // queue depth stays 1, head-of-line.
                    if self.interactive_queue.is_none()
                        && let Some(req) = active.req
                    {
                        self.push_event(SchedulerEvent::RetriedOnce { id, at_ms: now });
                        self.interactive_queue = Some((id, req, true));
                    } else {
                        self.completed.push((
                            id,
                            Err(ConnectorError::ConnectionLost(std::io::Error::new(
                                std::io::ErrorKind::ConnectionReset,
                                "lost mid-call; superseded before retry",
                            ))),
                        ));
                    }
                }
                other => self.completed.push((id, other)),
            }
        }
    }

    fn collect_background(&mut self, now: u64) {
        let Some(active) = &self.background_active else {
            return;
        };
        let id = active.id;
        if let Some(result) = self.backend.poll(id, now) {
            let active = self.background_active.take().expect("checked above");
            self.cancel_deadline = None;
            match result {
                _ if active.cancel_requested => {
                    // Cancelled for an interactive: re-queue, never lost
                    // (§9 "cancelled … and re-queued"). Not a retry-once
                    // consumption.
                    if let Some(job) = active.job {
                        self.push_event(SchedulerEvent::BackgroundRequeued { id, at_ms: now });
                        self.background_queue.push_front((id, job, active.retried));
                    }
                }
                Err(ConnectorError::ConnectionLost(_)) if !active.retried => {
                    self.push_event(SchedulerEvent::RetriedOnce { id, at_ms: now });
                    if let Some(job) = active.job {
                        self.background_queue.push_front((id, job, true));
                    }
                }
                other => self.completed.push((id, other)),
            }
        }
    }

    fn enforce_cancel_rule(&mut self, now: u64) {
        // The 250 ms rule: an interactive request exists (queued or
        // running) while the background slot is still occupied past the
        // deadline → disconnect the background request.
        let interactive_waiting =
            self.interactive_queue.is_some() || self.interactive_active.is_some();
        if let (Some(deadline), Some(active), true) = (
            self.cancel_deadline,
            self.background_active.as_mut(),
            interactive_waiting,
        ) && now >= deadline
            && !active.cancel_requested
        {
            active.cancel_requested = true;
            let id = active.id;
            self.backend.cancel(id, now);
            self.push_event(SchedulerEvent::BackgroundCancelledForInteractive { id, at_ms: now });
        }
    }

    fn start_interactive(&mut self, now: u64) {
        if !self.llm_ready || self.interactive_active.is_some() {
            return;
        }
        let Some((id, req, retried)) = self.interactive_queue.take() else {
            return;
        };
        self.backend.begin_interactive(id, &req, now);
        self.push_event(SchedulerEvent::InteractiveStarted { id, at_ms: now });
        // The request is retained for the §13.2 single retry.
        self.interactive_active = Some(ActiveJob {
            id,
            job: None,
            req: Some(req),
            retried,
            cancel_requested: false,
        });
        if self.background_active.is_some() && self.cancel_deadline.is_none() {
            self.cancel_deadline = Some(now + INTERACTIVE_CANCEL_WAIT_MS);
        }
    }

    fn start_background(&mut self, now: u64) {
        if !self.llm_ready || self.background_active.is_some() {
            return;
        }
        // §9: scheduled only when no interactive call is queued (or
        // running — protects the interactive budget), the mic hold is
        // clear, and no GC hold is out.
        if self.interactive_queue.is_some() || self.interactive_active.is_some() {
            return;
        }
        if self.background_held_at(now) {
            return;
        }
        let Some((id, job, retried)) = self.background_queue.pop_front() else {
            return;
        };
        self.backend.begin_background(id, &job, now);
        self.push_event(SchedulerEvent::BackgroundStarted { id, at_ms: now });
        self.background_active = Some(ActiveJob {
            id,
            job: Some(job),
            req: None,
            retried,
            cancel_requested: false,
        });
    }

    fn background_held_at(&self, now: u64) -> bool {
        self.mic_armed || self.unpause_at.is_some_and(|t| now < t) || self.gc_hold.is_some()
    }

    fn push_event(&mut self, e: SchedulerEvent) {
        self.events.push_back(e);
        if self.events.len() > EVENT_CAP {
            self.events.pop_front();
        }
    }
}

// ----------------------------------------------------- real slot backend ----

/// Production backend over the OpenAI-compatible client: one worker
/// thread per request (the blocking IO lives there), disconnect handles
/// captured for cancellation.
pub struct LlmSlotBackend {
    client: Arc<OpenAiCompatClient>,
    running: HashMap<JobId, RunningRequest>,
}

struct RunningRequest {
    rx: Receiver<ConnectorResult<ChatResponse>>,
    disconnect: Arc<Mutex<Option<photoproof_connectors::http::Disconnect>>>,
}

impl LlmSlotBackend {
    pub fn new(client: Arc<OpenAiCompatClient>) -> Self {
        Self {
            client,
            running: HashMap::new(),
        }
    }

    fn begin(&mut self, id: JobId, req: ChatRequest, stream: bool) {
        let (tx, rx) = channel();
        let disconnect: Arc<Mutex<Option<photoproof_connectors::http::Disconnect>>> =
            Arc::new(Mutex::new(None));
        let slot = disconnect.clone();
        let client = self.client.clone();
        std::thread::Builder::new()
            .name("pp-llm-request".into())
            .spawn(move || {
                let result = client.complete_transport(&req, stream, &mut |d| {
                    *slot.lock().expect("disconnect slot") = Some(d);
                });
                let _ = tx.send(result);
            })
            .expect("spawn llm request");
        self.running.insert(id, RunningRequest { rx, disconnect });
    }
}

impl SlotBackend for LlmSlotBackend {
    fn begin_interactive(&mut self, id: JobId, req: &ChatRequest, _now_ms: u64) {
        // Interactive may run non-streaming (cancellation never targets
        // this lane).
        self.begin(id, req.clone(), false);
    }

    fn begin_background(&mut self, id: JobId, job: &BackgroundJob, _now_ms: u64) {
        // STREAM-ONLY: the only call site for background requests, and it
        // hard-codes stream: true (§9).
        self.begin(id, job.request().clone(), true);
    }

    fn poll(&mut self, id: JobId, _now_ms: u64) -> Option<ConnectorResult<ChatResponse>> {
        let result = self.running.get(&id)?.rx.try_recv().ok()?;
        self.running.remove(&id);
        Some(result)
    }

    fn cancel(&mut self, id: JobId, _now_ms: u64) {
        if let Some(r) = self.running.get(&id)
            && let Some(d) = r.disconnect.lock().expect("disconnect slot").as_ref()
        {
            d.disconnect();
        }
    }
}
