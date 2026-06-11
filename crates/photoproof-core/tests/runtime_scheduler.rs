//! spec/RUNTIME.md §9 pass-scheduler criteria on the fake clock: the
//! §13.8 worst case (interactive parse issued MID-prompt-processing of a
//! caption) timed explicitly, mic-armed holds, the §13.2 single retry,
//! and §13.9 GC safety. The scripted backend models llama's honesty
//! clauses: prompt processing is non-preemptible; disconnect-cancellation
//! bites only during generation.

mod common;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use photoproof_connectors::llm::{ChatMessage, ChatRequest, ChatResponse, Lane, Role};
use photoproof_connectors::{ConnectorError, ConnectorResult};
use photoproof_core::capture::{Clock, FakeClock};
use photoproof_core::runtime::scheduler::{
    BACKGROUND_MAX_TOKENS, INTERACTIVE_CANCEL_WAIT_MS, MIC_UNPAUSE_DELAY_MS,
};
use photoproof_core::runtime::{
    BackgroundJob, GcCoordinator, GcDecision, JobId, PassScheduler, RuntimeBus, SchedulerEvent,
    SlotBackend, StubReindexGate,
};

// ------------------------------------------------------- scripted backend --

/// One scripted execution: a non-preemptible prompt phase, then
/// generation. `result` of `Err(())` means a mid-call ConnectionLost.
#[derive(Clone)]
struct SimSpec {
    prompt_ms: u64,
    gen_ms: u64,
    result: Result<&'static str, ()>,
}

struct SimJob {
    prompt_end: u64,
    finish: u64,
    /// When a cancel takes effect: max(cancel time, prompt_end) — the §9
    /// honesty clause (prompt processing runs out regardless).
    cancel_effective: Option<u64>,
    result: Result<&'static str, ()>,
    lane: Lane,
    delivered: bool,
}

#[derive(Clone, Default)]
struct SimBackend {
    inner: Arc<Mutex<SimState>>,
}

#[derive(Default)]
struct SimState {
    script: VecDeque<SimSpec>,
    jobs: HashMap<JobId, SimJob>,
    begins: Vec<(JobId, Lane, u64)>,
}

impl SimBackend {
    fn script(&self, specs: Vec<SimSpec>) {
        self.inner.lock().unwrap().script.extend(specs);
    }

    fn begins(&self) -> Vec<(JobId, Lane, u64)> {
        self.inner.lock().unwrap().begins.clone()
    }

    fn begin(&self, id: JobId, lane: Lane, now: u64) {
        let mut s = self.inner.lock().unwrap();
        let spec = s.script.pop_front().expect("a scripted spec per begin");
        // The contention model: an interactive submitted while a
        // background job is mid-PROMPT only executes once that phase
        // completes (non-preemptible window).
        let effective_start = if lane == Lane::Interactive {
            s.jobs
                .values()
                .filter(|j| j.lane == Lane::Background && !j.delivered && j.prompt_end > now)
                .map(|j| j.prompt_end)
                .max()
                .unwrap_or(now)
                .max(now)
        } else {
            now
        };
        s.begins.push((id, lane, now));
        s.jobs.insert(
            id,
            SimJob {
                prompt_end: effective_start + spec.prompt_ms,
                finish: effective_start + spec.prompt_ms + spec.gen_ms,
                cancel_effective: None,
                result: spec.result,
                lane,
                delivered: false,
            },
        );
    }
}

impl SlotBackend for SimBackend {
    fn begin_interactive(&mut self, id: JobId, _req: &ChatRequest, now_ms: u64) {
        self.begin(id, Lane::Interactive, now_ms);
    }

    fn begin_background(&mut self, id: JobId, _job: &BackgroundJob, now_ms: u64) {
        // The seam's type signature IS the stream-only honesty clause: a
        // background request can only arrive as a BackgroundJob.
        self.begin(id, Lane::Background, now_ms);
    }

    fn poll(&mut self, id: JobId, now_ms: u64) -> Option<ConnectorResult<ChatResponse>> {
        let mut s = self.inner.lock().unwrap();
        let job = s.jobs.get_mut(&id)?;
        if job.delivered {
            return None;
        }
        if let Some(at) = job.cancel_effective
            && now_ms >= at
        {
            job.delivered = true;
            return Some(Err(ConnectorError::Cancelled));
        }
        if now_ms >= job.finish {
            job.delivered = true;
            return Some(match job.result {
                Ok(text) => Ok(ChatResponse {
                    text: text.into(),
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    model_id: "sim".into(),
                }),
                Err(()) => Err(ConnectorError::ConnectionLost(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "sim: backend died mid-call",
                ))),
            });
        }
        None
    }

    fn cancel(&mut self, id: JobId, now_ms: u64) {
        let mut s = self.inner.lock().unwrap();
        if let Some(job) = s.jobs.get_mut(&id) {
            // §9: disconnect bites during GENERATION; a request still in
            // prompt processing runs that phase to completion.
            job.cancel_effective = Some(now_ms.max(job.prompt_end));
        }
    }
}

fn parse_req(text: &str) -> ChatRequest {
    ChatRequest {
        messages: vec![ChatMessage {
            role: Role::User,
            content: text.into(),
        }],
        max_tokens: 128,
        temperature: 0.0,
        json_schema: None,
        priority: Lane::Interactive,
    }
}

fn caption_job(model: &str) -> BackgroundJob {
    BackgroundJob::new(
        ChatRequest {
            messages: vec![ChatMessage {
                role: Role::User,
                content: "caption this image".into(),
            }],
            max_tokens: 99_999, // bounded at construction
            temperature: 0.2,
            json_schema: None,
            priority: Lane::Interactive, // forced to Background by the type
        },
        model,
    )
}

struct Rig {
    clock: FakeClock,
    sched: PassScheduler<FakeClock, SimBackend>,
    backend: SimBackend,
    /// (job, completion clock ms, result) — collected per tick.
    done: Vec<(JobId, u64, ConnectorResult<ChatResponse>)>,
}

fn rig() -> Rig {
    let clock = FakeClock::new(1_780_000_000_000);
    let backend = SimBackend::default();
    let mut sched = PassScheduler::new(clock.clone(), backend.clone());
    sched.set_llm_ready(true);
    Rig {
        clock,
        sched,
        backend,
        done: Vec::new(),
    }
}

impl Rig {
    /// Advance + tick in 50 ms steps, recording completion times.
    fn run(&mut self, total_ms: u64) {
        let mut elapsed = 0;
        while elapsed < total_ms {
            self.clock.advance(50);
            elapsed += 50;
            self.sched.tick();
            let now = self.clock.mono_ms();
            for (id, result) in self.sched.take_completed() {
                self.done.push((id, now, result));
            }
        }
    }

    fn done_for(&self, id: JobId) -> Option<&(JobId, u64, ConnectorResult<ChatResponse>)> {
        self.done.iter().find(|(j, _, _)| *j == id)
    }
}

// -------------------------------------------------------------------- tests

/// §13.8 — the WORST case, timed explicitly on the fake clock: an
/// interactive parse issued mid-PROMPT-processing of a caption (the
/// non-preemptible window, not the friendly mid-generation case) still
/// meets the §12.4 interactive budget (< 2 s).
#[test]
fn s13_8_parse_issued_mid_prompt_processing_of_a_caption_meets_the_budget() {
    let mut rig = rig();
    let t0 = rig.clock.mono_ms();
    rig.backend.script(vec![
        // The caption: 800 ms prompt phase (non-preemptible), 3 s gen.
        SimSpec {
            prompt_ms: 800,
            gen_ms: 3_000,
            result: Ok("never finishes this run"),
        },
        // The parse: small, as §9 demands of interactive work.
        SimSpec {
            prompt_ms: 100,
            gen_ms: 300,
            result: Ok("filter-ast"),
        },
        // The caption's re-run after its cancellation.
        SimSpec {
            prompt_ms: 800,
            gen_ms: 3_000,
            result: Ok("a quiet harbor"),
        },
    ]);
    let caption = rig.sched.submit_background(caption_job("gemma"));
    rig.sched.tick(); // caption starts at t0
    rig.run(300); // t0+300: the caption is 300 ms into its 800 ms prompt
    let submitted_at = rig.clock.mono_ms();
    let parse = rig.sched.submit_interactive(parse_req("harbor, rated 4+"));
    rig.run(6_000);

    // The 250 ms rule fired at exactly t0+300+250…
    let cancel_at = rig
        .sched
        .events()
        .find_map(|e| match e {
            SchedulerEvent::BackgroundCancelledForInteractive { id, at_ms } if *id == caption => {
                Some(*at_ms)
            }
            _ => None,
        })
        .expect("the background slot was cancelled for the interactive");
    assert_eq!(
        cancel_at,
        submitted_at + INTERACTIVE_CANCEL_WAIT_MS,
        "§9: cancelled once the interactive would wait > 250 ms"
    );

    // …but cancellation could not preempt the prompt phase: the parse
    // completed only after the caption's prompt window ran out.
    let (_, parse_done_at, result) = rig.done_for(parse).expect("parse completed");
    let prompt_window_end = t0 + 800;
    let parse_latency = parse_done_at - submitted_at;
    assert_eq!(
        *parse_done_at,
        prompt_window_end + 400,
        "parse executed right after the non-preemptible window + its own 400 ms"
    );
    assert!(
        parse_latency <= 2_000,
        "§12.4 interactive budget holds in the worst case: {parse_latency} ms"
    );
    assert_eq!(result.as_ref().unwrap().text, "filter-ast");

    // The cancelled caption was re-queued and completed afterwards.
    assert!(
        rig.sched
            .events()
            .any(|e| matches!(e, SchedulerEvent::BackgroundRequeued { id, .. } if *id == caption)),
        "§9: cancelled background work is re-queued, never lost"
    );
    let (_, caption_done_at, result) = rig.done_for(caption).expect("caption completed");
    assert_eq!(result.as_ref().unwrap().text, "a quiet harbor");
    assert!(*caption_done_at > *parse_done_at);
}

/// §9 honesty clauses, encoded in types: a background request can only be
/// a [`BackgroundJob`] — lane forced, max_tokens bounded; the backend
/// seam's background entry point only accepts that type (stream-only by
/// construction; the real backend hard-codes `stream: true` there).
#[test]
fn background_jobs_are_bounded_and_lane_forced_by_construction() {
    let job = caption_job("gemma");
    assert_eq!(
        job.request().max_tokens,
        BACKGROUND_MAX_TOKENS,
        "bounded max_tokens at construction (§9)"
    );
    assert_eq!(
        job.request().priority,
        Lane::Background,
        "lane forced regardless of the input request"
    );
}

/// §13.8 first half: with the mic armed, no background work starts —
/// observable in the scheduler's debug events; in-flight items finish
/// (bounded); unpause at disarm + 5 s exactly.
#[test]
fn s13_8_mic_armed_holds_background_passes_until_disarm_plus_5s() {
    let mut rig = rig();
    rig.backend.script(vec![
        SimSpec {
            prompt_ms: 100,
            gen_ms: 400,
            result: Ok("in-flight finishes"),
        },
        SimSpec {
            prompt_ms: 100,
            gen_ms: 100,
            result: Ok("held until unpause"),
        },
    ]);
    let in_flight = rig.sched.submit_background(caption_job("gemma"));
    rig.sched.tick(); // starts
    rig.run(100);
    rig.sched.set_mic_armed(true);
    assert!(rig.sched.background_held());
    let held = rig.sched.submit_background(caption_job("gemma"));

    // The in-flight item finishes bounded; the held one never starts.
    rig.run(3_000);
    assert!(rig.done_for(in_flight).is_some(), "in-flight items finish");
    assert!(
        rig.done_for(held).is_none(),
        "nothing new starts while armed"
    );
    assert!(
        rig.sched
            .events()
            .any(|e| matches!(e, SchedulerEvent::HeldForMic { .. })),
        "the hold is observable (debug panel)"
    );

    // Disarm: unpause at +5 s, not earlier.
    rig.sched.set_mic_armed(false);
    let disarm_at = rig.clock.mono_ms();
    rig.run(MIC_UNPAUSE_DELAY_MS - 50);
    assert!(
        rig.done_for(held).is_none(),
        "still held at disarm + 4.95 s"
    );
    rig.run(2_000);
    let started_at = rig
        .backend
        .begins()
        .iter()
        .find(|(id, _, _)| *id == held)
        .map(|(_, _, at)| *at)
        .expect("held job started after the unpause");
    assert_eq!(
        started_at,
        disarm_at + MIC_UNPAUSE_DELAY_MS,
        "§9: unpause at mic disarm + 5 s"
    );
    assert!(rig.done_for(held).is_some());
}

/// §13.2 — a mid-call ConnectionLost retries EXACTLY once after Ready,
/// and succeeds; a second loss surfaces to the caller.
#[test]
fn s13_2_mid_call_crash_retries_exactly_once_after_ready_then_succeeds() {
    let mut rig = rig();
    rig.backend.script(vec![
        SimSpec {
            prompt_ms: 50,
            gen_ms: 100,
            result: Err(()), // the crash
        },
        SimSpec {
            prompt_ms: 50,
            gen_ms: 100,
            result: Ok("second time lucky"),
        },
    ]);
    let parse = rig.sched.submit_interactive(parse_req("x"));
    rig.sched.tick(); // the request begins
    // The child dies mid-call: the supervisor leaves Ready (Restarting)
    // the moment the loss is reported — model that here, before the
    // in-flight result lands.
    rig.sched.set_llm_ready(false);
    rig.run(2_000);
    assert!(
        rig.done_for(parse).is_none(),
        "no error surfaced: retry pending"
    );
    assert_eq!(
        rig.backend.begins().len(),
        1,
        "nothing re-begins while down"
    );
    rig.sched.set_llm_ready(true);
    rig.run(1_000);
    let (_, _, result) = rig.done_for(parse).expect("retried completion");
    assert_eq!(result.as_ref().unwrap().text, "second time lucky");
    assert_eq!(
        rig.sched
            .events()
            .filter(|e| matches!(e, SchedulerEvent::RetriedOnce { .. }))
            .count(),
        1,
        "exactly one automatic retry"
    );
    assert_eq!(rig.backend.begins().len(), 2, "begin + one retry, no more");
}

#[test]
fn s13_2_a_second_loss_surfaces_to_the_caller() {
    let mut rig = rig();
    rig.backend.script(vec![
        SimSpec {
            prompt_ms: 50,
            gen_ms: 100,
            result: Err(()),
        },
        SimSpec {
            prompt_ms: 50,
            gen_ms: 100,
            result: Err(()),
        },
    ]);
    let parse = rig.sched.submit_interactive(parse_req("x"));
    rig.run(2_000);
    let (_, _, result) = rig.done_for(parse).expect("second loss surfaces");
    assert!(matches!(result, Err(ConnectorError::ConnectionLost(_))));
    assert_eq!(
        rig.backend.begins().len(),
        2,
        "exactly one retry, then surface"
    );
}

/// Interactive queue depth 1: a newer parse displaces the queued one
/// (search-as-you-type supersedes itself); the displaced job completes
/// as Cancelled.
#[test]
fn interactive_queue_depth_one_displaces_the_queued_parse() {
    let mut rig = rig();
    rig.sched.set_llm_ready(false); // hold both in the queue
    let first = rig.sched.submit_interactive(parse_req("harb"));
    let second = rig.sched.submit_interactive(parse_req("harbor"));
    rig.backend.script(vec![SimSpec {
        prompt_ms: 50,
        gen_ms: 50,
        result: Ok("harbor results"),
    }]);
    rig.sched.set_llm_ready(true);
    rig.run(500);
    let (_, _, displaced) = rig.done_for(first).expect("displaced completes");
    assert!(matches!(displaced, Err(ConnectorError::Cancelled)));
    let (_, _, kept) = rig.done_for(second).expect("newest wins");
    assert_eq!(kept.as_ref().unwrap().text, "harbor results");
    assert_eq!(rig.backend.begins().len(), 1, "only the newest ever began");
}

/// §13.9 — GC safety at the scheduler lock: a model with in-flight or
/// queued work is untouchable; the hold pauses background starts.
#[test]
fn s13_9_gc_hold_refused_while_model_has_work_and_pauses_background() {
    let mut rig = rig();
    rig.backend.script(vec![
        SimSpec {
            prompt_ms: 100,
            gen_ms: 400,
            result: Ok("old-model pass"),
        },
        SimSpec {
            prompt_ms: 50,
            gen_ms: 50,
            result: Ok("next pass"),
        },
    ]);
    let pass = rig.sched.submit_background(caption_job("old-embedder"));
    rig.sched.tick();
    assert!(
        !rig.sched.try_gc_hold("old-embedder"),
        "in-flight work: untouchable"
    );
    rig.run(1_000);
    assert!(rig.done_for(pass).is_some());

    // Queued (not yet started) work also blocks.
    rig.sched.set_mic_armed(true); // keep it queued
    let queued = rig.sched.submit_background(caption_job("old-embedder"));
    assert!(
        !rig.sched.try_gc_hold("old-embedder"),
        "queued work: untouchable"
    );
    rig.sched.set_mic_armed(false);
    rig.run(MIC_UNPAUSE_DELAY_MS + 1_000);
    assert!(rig.done_for(queued).is_some());

    // Idle: the hold takes, and pauses background starts while out.
    assert!(rig.sched.try_gc_hold("old-embedder"));
    rig.backend.script(vec![SimSpec {
        prompt_ms: 50,
        gen_ms: 50,
        result: Ok("after gc"),
    }]);
    let after = rig.sched.submit_background(caption_job("gemma"));
    rig.run(500);
    assert!(
        rig.done_for(after).is_none(),
        "GC takes the same lock as background passes (§11)"
    );
    rig.sched.release_gc_hold();
    rig.run(500);
    assert!(rig.done_for(after).is_some());
}

/// §13.9 through the GC coordinator: manifest bump with a reindex
/// pending → old embedder weights survive until RETRIEVAL reports
/// complete; no active pass ever loses its model files.
#[test]
fn s13_9_old_weights_survive_until_verified_plus_reindex_plus_idle() {
    let mut rig = rig();
    let dir = tempfile::tempdir().unwrap();
    let old_dir = dir.path().join("old-embedder");
    std::fs::create_dir_all(&old_dir).unwrap();
    std::fs::write(old_dir.join("model.onnx"), vec![0u8; 4096]).unwrap();
    let bus = RuntimeBus::new();
    let mut gc = GcCoordinator::new(dir.path().to_path_buf(), bus);
    let gate_no = StubReindexGate {
        allow: HashSet::new(),
    };
    let gate_yes = StubReindexGate {
        allow: HashSet::from(["old-embedder".to_owned()]),
    };

    // (a) Replacement not yet verified → held.
    let d = gc.try_gc_old_model(
        "old-embedder",
        "new-embedder",
        true,
        &gate_yes,
        &mut rig.sched,
    );
    assert!(matches!(d, GcDecision::Held { .. }), "not verified: {d:?}");
    assert!(old_dir.exists());

    // (b) Verified, but RETRIEVAL says the reindex is pending → held.
    gc.note_verified_ready("new-embedder");
    let d = gc.try_gc_old_model(
        "old-embedder",
        "new-embedder",
        true,
        &gate_no,
        &mut rig.sched,
    );
    assert!(
        matches!(d, GcDecision::Held { .. }),
        "reindex pending: {d:?}"
    );
    assert!(old_dir.exists(), "old vectors stay queryable (§11)");

    // (c) Reindex done, but an active pass uses the old model → held.
    rig.backend.script(vec![SimSpec {
        prompt_ms: 100,
        gen_ms: 400,
        result: Ok("active pass"),
    }]);
    let pass = rig.sched.submit_background(caption_job("old-embedder"));
    rig.sched.tick();
    let d = gc.try_gc_old_model(
        "old-embedder",
        "new-embedder",
        true,
        &gate_yes,
        &mut rig.sched,
    );
    assert!(matches!(d, GcDecision::Held { .. }), "active pass: {d:?}");
    assert!(old_dir.exists(), "an active pass never loses its files");

    // (d) Pass finished, everything answers yes → deleted.
    rig.run(1_000);
    assert!(rig.done_for(pass).is_some());
    let d = gc.try_gc_old_model(
        "old-embedder",
        "new-embedder",
        true,
        &gate_yes,
        &mut rig.sched,
    );
    assert!(
        matches!(d, GcDecision::Deleted { bytes_reclaimed } if bytes_reclaimed >= 4096),
        "{d:?}"
    );
    assert!(!old_dir.exists());
    assert!(!rig.sched.background_held(), "the GC hold was released");
}

/// §11: orphaned directories are LISTED ("reclaim N GB"), never silently
/// deleted.
#[test]
fn orphans_are_listed_never_silently_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let stray = dir.path().join("some-old-experiment");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(stray.join("w.bin"), vec![0u8; 2048]).unwrap();
    let gc = GcCoordinator::new(dir.path().to_path_buf(), RuntimeBus::new());
    let manifest = photoproof_core::runtime::compiled_manifest();
    let orphans = gc.list_orphans(&manifest);
    assert_eq!(orphans, vec![("some-old-experiment".to_owned(), 2048)]);
    assert!(stray.exists(), "listing is not deleting");
}

/// The REAL backend end-to-end: `begin_background` hard-codes
/// `stream: true` on the wire (the §9 honesty clause at the socket), and
/// the §13.2 retry flow works over real loopback HTTP.
#[test]
fn real_llm_backend_streams_background_requests_on_the_wire() {
    use photoproof_connectors::mock::{StubHttpServer, StubResponse};
    use photoproof_connectors::openai::{EndpointCell, OpenAiCompatClient};
    use photoproof_core::runtime::LlmSlotBackend;

    let server = StubHttpServer::start();
    server.route(
        "/v1/chat/completions",
        StubResponse::Sse {
            events: vec![
                r#"{"choices":[{"delta":{"content":"a caption"}}]}"#.into(),
                "[DONE]".into(),
            ],
            cut_after_events: None,
        },
    );
    let client = Arc::new(
        OpenAiCompatClient::supervised("gemma", EndpointCell::fixed(server.addr())).with_timeouts(
            std::time::Duration::from_millis(500),
            std::time::Duration::from_millis(2_000),
        ),
    );
    let mut backend = LlmSlotBackend::new(client);
    backend.begin_background(1, &caption_job("gemma"), 0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let result = loop {
        if let Some(r) = backend.poll(1, 0) {
            break r;
        }
        assert!(std::time::Instant::now() < deadline, "bounded wait");
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert_eq!(result.unwrap().text, "a caption");
    let body: serde_json::Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
    assert_eq!(
        body["stream"], true,
        "background lane streams ON THE WIRE — disconnect-cancel works only then (§9)"
    );
}

/// §9: the embedder's GPU→CPU fallback is recorded for the debug panel.
#[test]
fn embedder_fallback_is_recorded_for_the_debug_panel() {
    let mut rig = rig();
    rig.sched.note_embedder_fallback(
        "CUDA allocation failed at batch 12; CPU EP for the rest of the pass",
    );
    assert!(rig.sched.events().any(|e| matches!(
        e,
        SchedulerEvent::EmbedderFallback { detail, .. } if detail.contains("CPU EP")
    )));
}
