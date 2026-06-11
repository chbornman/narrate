//! The capture engine (spec/CAPTURE.md §5–§7, §9, §11): mic state machine,
//! utterance lifecycle, VAD-onset binding, voice-event minting, the audio
//! policy, and stroke↔utterance linking.
//!
//! Seam-driven and headless: the engine consumes the P1.2 `Transcriber` /
//! `VoiceActivityDetector` traits (photoproof-connectors) and the [`Clock`]
//! seam — scripted mocks plus a fake clock drive every test; nothing here
//! sleeps or reads real time. ASR readiness is asked through the
//! `Transcriber::stream` seam itself (`Err` at open = not ready), so P6.2's
//! supervised client plugs in without engine changes.

use std::task::{Context, Poll, Waker};

use futures_core::stream::BoxStream;
use photoproof_connectors::mock::AudioFeed;
use photoproof_connectors::transcriber::{
    AudioFrame, SegmentKind, StreamMs, Transcriber, TranscriptSegment,
};
use photoproof_connectors::vad::{VadEvent, VoiceActivityDetector};

use crate::event::Event;
use crate::id::{ContentHash, Minted, SessionId, UtcMillis};
use crate::store::{AppendError, EventDraft, EventStore, RemarkSource};

use super::audio::AudioRing;
use super::clock::Clock;
use super::link::{StrokeSpan, UtteranceSpan, resolve_stroke_link, resolve_utterance_link};
use super::scope::{ScopeRing, ScopeSnapshot, ScopeView};
use super::session::CaptureDrain;
use crate::event::StrokePayload;

/// §1 error budget: VAD-onset latency + clock conversion, combined.
pub const ONSET_ERROR_BUDGET_MS: u64 = 250;
/// §2.5/§6.4: trailing-final acceptance window after disarm/end_stream.
pub const DRAIN_WINDOW_MS: u64 = 5_000;
/// Debug-panel note cap (in-memory only).
const DEBUG_NOTE_CAP: usize = 256;

/// CAPTURE §6.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicState {
    Disarmed,
    Arming,
    ArmedIdle,
    ArmedSpeaking,
    /// `Disarmed(error)` — device/ASR failure; quiet indicator state.
    DisarmedError,
}

impl MicState {
    pub fn as_str(self) -> &'static str {
        match self {
            MicState::Disarmed => "disarmed",
            MicState::Arming => "arming",
            MicState::ArmedIdle => "armedIdle",
            MicState::ArmedSpeaking => "armedSpeaking",
            MicState::DisarmedError => "disarmedError",
        }
    }

    pub fn is_armed(self) -> bool {
        matches!(
            self,
            MicState::Arming | MicState::ArmedIdle | MicState::ArmedSpeaking
        )
    }
}

/// CAPTURE §11 — the indicator data contract. No text content ever rides
/// this (partials are debug-panel-only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndicatorState {
    /// What a typed note would bind to NOW.
    pub current_scope: ScopeView,
    pub mic: MicState,
    pub streaming_utterance: Option<StreamingView>,
    pub degraded: DegradedFlags,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamingView {
    /// `scope_at(onset)` — §5.4: shown even when the live selection moved on.
    pub bound_scope: ScopeView,
    pub started_at: UtcMillis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DegradedFlags {
    pub asr_unavailable: bool,
}

/// One in-flight (Streaming) utterance: VAD onset seen, no Final yet.
struct InFlight {
    onset_stream: StreamMs,
    onset_mono: u64,
    /// VAD SpeechEnd, when seen (bounds the durable span — §6.5).
    end_stream: Option<StreamMs>,
    /// Held binding snapshot: `scope_at(onset)` (§5.1).
    snapshot: ScopeSnapshot,
    /// Id + ts fixed at onset (EVENTS §1.2).
    minted: Minted,
    /// FIFO-associated ASR utterance id, once a segment referenced it.
    utterance_id: Option<u64>,
}

/// A committed event's span bookkeeping for §9 link resolution (same
/// session, this process — resolution runs exactly once, at commit).
struct Committed {
    utterances: Vec<UtteranceSpan>,
    strokes: Vec<StrokeSpan>,
}

struct Pipeline<'t> {
    feed: AudioFeed,
    stream: BoxStream<'t, photoproof_connectors::ConnectorResult<TranscriptSegment>>,
    /// Stream clock anchor: capture-clock instant of stream position 0,
    /// fixed at FIRST audio buffer submission (§1).
    anchor_mono: Option<u64>,
    /// Set at disarm/close: trailing finals accepted until this instant.
    drain_deadline: Option<u64>,
}

/// The engine. Lifetime-bound to the transcriber it streams from (the
/// shell holds a process-lifetime client; tests hold one on the stack).
pub struct CaptureEngine<'t, C: Clock> {
    clock: C,
    transcriber: &'t dyn Transcriber,
    vad: Box<dyn VoiceActivityDetector>,
    ring: ScopeRing,
    audio: AudioRing,
    mic: MicState,
    degraded: bool,
    pipeline: Option<Pipeline<'t>>,
    in_flight: Vec<InFlight>,
    committed: Committed,
    session: SessionId,
    abandoned: u64,
    debug: Vec<String>,
    /// §2.1: most recent capture-side ACTIVITY — mic arm/disarm, VAD
    /// speech (gated frames and boundary events), partial/final arrival
    /// while armed — as `(capture clock ms, wall clock)`. Fed to the
    /// session engine through the `CaptureDrain` seam so speech refreshes
    /// the idle timer (§2.2: a boundary never bisects an in-flight
    /// utterance).
    last_activity: Option<(u64, UtcMillis)>,
}

impl<'t, C: Clock> CaptureEngine<'t, C> {
    pub fn new(
        clock: C,
        transcriber: &'t dyn Transcriber,
        vad: Box<dyn VoiceActivityDetector>,
        session: SessionId,
    ) -> Self {
        let (now_mono, now_wall) = (clock.mono_ms(), clock.wall());
        Self {
            clock,
            transcriber,
            vad,
            ring: ScopeRing::new(now_mono, now_wall),
            audio: AudioRing::new(),
            mic: MicState::Disarmed,
            degraded: false,
            pipeline: None,
            in_flight: Vec::new(),
            committed: Committed {
                utterances: Vec::new(),
                strokes: Vec::new(),
            },
            session,
            abandoned: 0,
            debug: Vec::new(),
            last_activity: None,
        }
    }

    /// Record capture-side activity NOW (§2.1) for the session engine's
    /// idle decisions.
    fn touch_activity(&mut self) {
        self.last_activity = Some((self.clock.mono_ms(), self.clock.wall()));
    }

    // -- scope (§3) -----------------------------------------------------------

    /// The UI reports its selection/view-derived target list; the engine
    /// snapshots it into the ring. Returns the echoed snapshot.
    pub fn set_scope(&mut self, targets: Vec<ContentHash>) -> &ScopeSnapshot {
        let (m, w) = (self.clock.mono_ms(), self.clock.wall());
        self.ring.push(targets, m, w)
    }

    pub fn scope_ring(&self) -> &ScopeRing {
        &self.ring
    }

    /// Session rotation: subsequent commits mint into the new session, and
    /// the §9 candidate registry resets (linking is same-session only).
    pub fn set_session(&mut self, session: SessionId) {
        if session != self.session {
            self.session = session;
            self.committed.utterances.clear();
            self.committed.strokes.clear();
        }
    }

    // -- mic state machine (§6.4) ----------------------------------------------

    /// Toggle-arm. Opens the Transcriber stream; `Err` at open IS the
    /// readiness answer (P6.2's supervised client returns `NotReady` until
    /// the child is up) → `Disarmed(error)`, quietly degraded (§6.6).
    pub fn arm(&mut self) -> MicState {
        if self.mic.is_armed() {
            return self.mic;
        }
        self.touch_activity(); // §2.1: mic arm is activity
        self.mic = MicState::Arming;
        let (feed, stream) = AudioFeed::new();
        match self.transcriber.stream(stream) {
            Ok(out) => {
                self.vad.reset(); // re-arm rebuilds the chain from scratch (§6.2)
                self.pipeline = Some(Pipeline {
                    feed,
                    stream: out,
                    anchor_mono: None,
                    drain_deadline: None,
                });
                self.degraded = false;
                self.mic = MicState::ArmedIdle;
            }
            Err(e) => {
                self.note(format!("arming failed (ASR not ready): {e}"));
                self.degraded = true;
                self.mic = MicState::DisarmedError;
            }
        }
        self.mic
    }

    /// Toggle-disarm (§6.4): stop pushing audio, `end_stream()`, accept
    /// trailing finals up to 5 s (their onsets predate the disarm; they
    /// mint normally), then the stream closes entirely — never
    /// paused-but-open — and the ring buffer zeroes. Pump to completion.
    pub fn disarm(&mut self, store: &EventStore) -> Vec<Event> {
        if !self.mic.is_armed() {
            return Vec::new();
        }
        self.touch_activity(); // §2.1: mic disarm is activity (user toggle)
        self.disarm_inner(store)
    }

    /// The disarm mechanics without the §2.1 activity touch — the
    /// session-close drain disarms machine-initiated, which is NOT
    /// activity ("not activity: … sidecar-writer or model-runtime
    /// activity"; only the user's toggle is).
    fn disarm_inner(&mut self, store: &EventStore) -> Vec<Event> {
        self.mic = MicState::Disarmed;
        if let Some(p) = &mut self.pipeline {
            p.feed.close();
            p.drain_deadline = Some(self.clock.mono_ms() + DRAIN_WINDOW_MS);
        }
        self.pump(store)
    }

    pub fn mic(&self) -> MicState {
        self.mic
    }

    pub fn degraded(&self) -> DegradedFlags {
        DegradedFlags {
            asr_unavailable: self.degraded,
        }
    }

    /// The cpal-stream seam state: `false` = torn down (§6.4/§7 — closed,
    /// never paused-but-open; the OS mic dot agrees with the armed state).
    pub fn stream_open(&self) -> bool {
        self.pipeline.is_some()
    }

    pub fn audio_is_zeroed(&self) -> bool {
        self.audio.is_zeroed()
    }

    /// Streaming utterances (SpeechStart seen, no Final).
    pub fn streaming_count(&self) -> usize {
        self.in_flight.len()
    }

    /// Utterances abandoned (fatal error / drain timeout) — nothing minted.
    pub fn abandoned_count(&self) -> u64 {
        self.abandoned
    }

    /// Debug-panel notes (dev builds render them; never persisted).
    pub fn debug_notes(&self) -> &[String] {
        &self.debug
    }

    // -- audio path (§6.2, §7) ---------------------------------------------------

    /// One capture-stream frame: ring-buffered (§7), VAD-processed (onset
    /// binding + silence gate), gated frames shipped to the ASR, then the
    /// output stream pumped. Returns events committed by this push.
    pub fn push_audio(&mut self, store: &EventStore, frame: AudioFrame) -> Vec<Event> {
        if !self.mic.is_armed() || self.pipeline.is_none() {
            return Vec::new();
        }
        self.audio.write(&frame.samples);
        let anchor = {
            let p = self.pipeline.as_mut().expect("checked above");
            *p.anchor_mono
                .get_or_insert_with(|| self.clock.mono_ms().saturating_sub(frame.captured_at))
        };
        let result = self.vad.process_frame(&frame);
        // §2.1: VAD speech activity is ACTIVITY — any speech-gated frame or
        // VAD boundary event while armed refreshes the idle timer (through
        // the CaptureDrain seam), so an idle boundary never bisects an
        // in-flight utterance (§2.2). Silence frames refresh nothing.
        if result.gate_open || !result.events.is_empty() {
            self.touch_activity();
        }
        for ev in result.events {
            match ev {
                VadEvent::SpeechStart { onset } => self.on_speech_start(store, anchor, onset),
                VadEvent::SpeechEnd { end } => {
                    if let Some(u) = self
                        .in_flight
                        .iter_mut()
                        .find(|u| u.end_stream.is_none() && u.onset_stream < end)
                    {
                        u.end_stream = Some(end);
                    }
                }
            }
        }
        if result.gate_open
            && let Some(p) = &self.pipeline
        {
            p.feed.push(frame);
        }
        self.pump(store)
    }

    fn on_speech_start(&mut self, store: &EventStore, anchor: u64, onset: StreamMs) {
        let onset_mono = anchor + onset;
        let lookup = self.ring.scope_at(onset_mono);
        if lookup.predated {
            self.note(format!(
                "speech onset at mono {onset_mono} predates the scope ring; bound to oldest"
            ));
        }
        // Mint at onset (EVENTS §1.2): ts = wall clock AT the estimated
        // onset (detection lags ≤ 300 ms; the conversion is the §1 budget).
        let now_mono = self.clock.mono_ms();
        let onset_wall = UtcMillis::from_epoch_ms(
            self.clock.wall().epoch_ms() - now_mono.saturating_sub(onset_mono) as i64,
        );
        let minted = store.mint_at(onset_wall);
        self.in_flight.push(InFlight {
            onset_stream: onset,
            onset_mono,
            end_stream: None,
            snapshot: lookup.snapshot,
            minted,
            utterance_id: None,
        });
        self.mic = MicState::ArmedSpeaking;
    }

    // -- transcript pump (§6.5) ----------------------------------------------------

    fn poll_stream(
        stream: &mut BoxStream<'t, photoproof_connectors::ConnectorResult<TranscriptSegment>>,
    ) -> Poll<Option<photoproof_connectors::ConnectorResult<TranscriptSegment>>> {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        stream.as_mut().poll_next(&mut cx)
    }

    /// Drain everything the Transcriber has ready (non-blocking; mocks are
    /// poll-deterministic). Returns events committed by this pump.
    pub fn pump(&mut self, store: &EventStore) -> Vec<Event> {
        let mut committed = Vec::new();
        while self.pipeline.is_some() {
            let polled = {
                let p = self.pipeline.as_mut().expect("checked in loop condition");
                Self::poll_stream(&mut p.stream)
            };
            match polled {
                Poll::Ready(Some(Ok(seg))) => match seg.kind {
                    SegmentKind::Partial => self.on_partial(&seg),
                    SegmentKind::Final => {
                        if let Some(e) = self.on_final(store, &seg) {
                            committed.push(e);
                        }
                    }
                },
                Poll::Ready(Some(Err(e))) => {
                    self.on_fatal(format!("fatal ASR error: {e}"));
                    break;
                }
                Poll::Ready(None) => {
                    if self
                        .pipeline
                        .as_ref()
                        .is_some_and(|p| p.drain_deadline.is_some())
                    {
                        self.finish_drain("stream ended");
                    } else {
                        // The stream ended while armed: the ASR side is gone.
                        self.on_fatal("transcriber stream ended unexpectedly".into());
                    }
                    break;
                }
                Poll::Pending => {
                    let deadline_passed = self
                        .pipeline
                        .as_ref()
                        .and_then(|p| p.drain_deadline)
                        .is_some_and(|d| self.clock.mono_ms() >= d);
                    if deadline_passed {
                        self.finish_drain("5 s drain window elapsed");
                    }
                    break;
                }
            }
        }
        committed
    }

    fn on_partial(&mut self, seg: &TranscriptSegment) {
        self.associate(seg.utterance_id);
        if self.mic.is_armed() {
            self.mic = MicState::ArmedSpeaking;
            self.touch_activity(); // §2.1: Partial while armed is activity
        }
        // Partials are NEVER persisted and never displayed in the
        // indicator; the dev-build debug panel MAY show them (§6.5).
        self.note(format!(
            "partial[{}] @{}ms: {:?}",
            seg.utterance_id, seg.onset, seg.text
        ));
    }

    /// FIFO association: VAD onsets and ASR utterances arrive in stream
    /// order; the first segment naming a new utterance id claims the oldest
    /// unclaimed onset.
    fn associate(&mut self, utterance_id: u64) -> Option<usize> {
        if let Some(i) = self
            .in_flight
            .iter()
            .position(|u| u.utterance_id == Some(utterance_id))
        {
            return Some(i);
        }
        if let Some(i) = self.in_flight.iter().position(|u| u.utterance_id.is_none()) {
            self.in_flight[i].utterance_id = Some(utterance_id);
            return Some(i);
        }
        None
    }

    fn on_final(&mut self, store: &EventStore, seg: &TranscriptSegment) -> Option<Event> {
        if self.mic.is_armed() {
            self.touch_activity(); // §2.1: Final while armed is activity
        }
        let now_mono = self.clock.mono_ms();
        let anchor = self
            .pipeline
            .as_ref()
            .and_then(|p| p.anchor_mono)
            .unwrap_or(now_mono);
        let held = self
            .associate(seg.utterance_id)
            .map(|i| self.in_flight.remove(i));

        // Empty/whitespace-only finals mint NOTHING (§6.5).
        if seg.text.trim().is_empty() {
            self.settle_speaking_state();
            return None;
        }

        let (snapshot, minted, onset_mono, end_mono) = match held {
            Some(u) => {
                // THE BINDING RULE (§5): the held VAD-onset snapshot is
                // authoritative. The segment's own onset is the ASR-side
                // timestamp — a CROSS-CHECK ONLY: when it disagrees across a
                // scope change by more than the §1 budget, log; NEVER rebind.
                let token_onset_mono = anchor + seg.onset;
                let disagreement = token_onset_mono.abs_diff(u.onset_mono);
                if disagreement > ONSET_ERROR_BUDGET_MS {
                    let token_scope = self.ring.scope_at(token_onset_mono);
                    if token_scope.snapshot.targets != u.snapshot.targets {
                        self.note(format!(
                            "token-time cross-check: segment {} token onset disagrees with VAD \
                             onset by {disagreement} ms across a scope change; binding kept at \
                             VAD onset (§5.1)",
                            seg.utterance_id
                        ));
                    }
                }
                let end_stream = u
                    .end_stream
                    .filter(|&e| e > u.onset_stream)
                    .unwrap_or(seg.end);
                (u.snapshot, u.minted, u.onset_mono, anchor + end_stream)
            }
            None => {
                // No VAD onset held (e.g. the ASR endpointed one continuous
                // span into multiple segments — §5.3: each final binds
                // independently by its own onset).
                let onset_mono = anchor + seg.onset;
                let lookup = self.ring.scope_at(onset_mono);
                if lookup.predated {
                    self.note(format!(
                        "final[{}] onset predates the scope ring; bound to oldest",
                        seg.utterance_id
                    ));
                }
                let onset_wall = UtcMillis::from_epoch_ms(
                    self.clock.wall().epoch_ms() - now_mono.saturating_sub(onset_mono) as i64,
                );
                (
                    lookup.snapshot,
                    store.mint_at(onset_wall),
                    onset_mono,
                    anchor + seg.end,
                )
            }
        };

        let end_mono = end_mono.max(onset_mono);
        // §9.2: the voice remark is the later-committed event here — it
        // carries the backward link to an earlier committed stroke.
        let linked_event = resolve_utterance_link(
            (onset_mono, end_mono),
            &snapshot.targets,
            &self.committed.strokes,
        );
        let draft = EventDraft::Remark {
            source: RemarkSource::Voice {
                // exp(mean token log-prob), uncalibrated, OPTIONAL — omitted
                // when the model exposes no token log-probs (§6.5).
                conf_pm: seg
                    .confidence
                    .map(|c| (c.clamp(0.0, 1.0) * 1000.0).round() as u16),
                dur_ms: u32::try_from(end_mono - onset_mono).unwrap_or(u32::MAX),
                linked_event,
            },
            text: seg.text.clone(),            // verbatim (§6.5)
            targets: snapshot.targets.clone(), // session snapshot ⇒ zero targets
        };
        match store.append(&self.session, draft, Some(minted)) {
            Ok(event) => {
                self.committed.utterances.push(UtteranceSpan {
                    id: event.id.clone(),
                    start: onset_mono,
                    end: end_mono,
                    targets: snapshot.targets,
                });
                self.audio.note_finalized(now_mono);
                self.settle_speaking_state();
                Some(event)
            }
            Err(e) => {
                self.note(format!("voice commit failed: {e}"));
                self.settle_speaking_state();
                None
            }
        }
    }

    fn settle_speaking_state(&mut self) {
        if self.mic == MicState::ArmedSpeaking && self.in_flight.is_empty() {
            self.mic = MicState::ArmedIdle;
        }
    }

    /// §6.6: fatal ASR error — Streaming utterances Abandoned (nothing
    /// minted, debug note), mic auto-disarms to `Disarmed(error)`, the ring
    /// buffer zeroes, the stream tears down. Quiet: indicator-only.
    fn on_fatal(&mut self, why: String) {
        self.abandon_in_flight(&why);
        self.note(why);
        self.mic = MicState::DisarmedError;
        self.degraded = true;
        self.teardown();
    }

    fn finish_drain(&mut self, why: &str) {
        if !self.in_flight.is_empty() {
            self.abandon_in_flight(why);
        }
        self.teardown();
    }

    fn abandon_in_flight(&mut self, why: &str) {
        for u in self.in_flight.drain(..) {
            self.abandoned += 1;
            self.debug.push(format!(
                "utterance abandoned ({why}): onset mono {}, nothing persisted",
                u.onset_mono
            ));
            if self.debug.len() > DEBUG_NOTE_CAP {
                self.debug.remove(0);
            }
        }
        self.settle_speaking_state();
    }

    /// Close the stream entirely (never paused-but-open) and zero the
    /// audio ring immediately (§6.4/§7).
    fn teardown(&mut self) {
        if let Some(p) = self.pipeline.take() {
            p.feed.close();
        }
        self.audio.zero();
    }

    fn note(&mut self, s: String) {
        self.debug.push(s);
        if self.debug.len() > DEBUG_NOTE_CAP {
            self.debug.remove(0);
        }
    }

    // -- strokes (§9) ------------------------------------------------------------

    /// Commit one stroke through §9 link resolution: span = pen-up `now`
    /// minus the payload's `t_last` (exact since B41's terminal pen-up
    /// sample). In-flight suppression: a streaming utterance whose
    /// span-so-far (onset .. now) overlaps the stroke keeps the stroke
    /// unlinked — the utterance carries the link when IT commits.
    pub fn commit_stroke(
        &mut self,
        store: &EventStore,
        target: ContentHash,
        payload: StrokePayload,
    ) -> Result<Event, AppendError> {
        let now = self.clock.mono_ms();
        let t_last = payload.points.last().map(|p| u64::from(p.t)).unwrap_or(0);
        let span = (now.saturating_sub(t_last), now);
        // The suppression gate compares against the streaming utterance's
        // span-SO-FAR (onset .. now), not its eventual final span — and a
        // span ending at `now` always touches a stroke committing at `now`,
        // so any utterance in flight at pen-up suppresses.
        let suppressed = !self.in_flight.is_empty();
        let linked_event = if suppressed {
            self.note(
                "stroke overlaps a streaming utterance; committed unlinked \
                 (the utterance carries the link — §9.2)"
                    .into(),
            );
            None
        } else {
            resolve_stroke_link(span, &target, &self.committed.utterances)
        };
        let event = store.append(
            &self.session,
            EventDraft::Stroke {
                payload,
                target: target.clone(),
                linked_event,
            },
            None,
        )?;
        self.committed.strokes.push(StrokeSpan {
            id: event.id.clone(),
            start: span.0,
            end: span.1,
            image: target,
        });
        Ok(event)
    }

    // -- indicator (§11) ------------------------------------------------------------

    pub fn indicator(&self) -> IndicatorState {
        IndicatorState {
            current_scope: self.ring.current().view(),
            mic: self.mic,
            // §5.4: the in-flight utterance shows the scope it is BOUND to,
            // even when the live selection has changed. With several in
            // flight, the most recent onset is the one being spoken.
            streaming_utterance: self.in_flight.last().map(|u| StreamingView {
                bound_scope: u.snapshot.view(),
                started_at: u.minted.ts,
            }),
            degraded: self.degraded(),
        }
    }
}

impl<C: Clock> CaptureDrain for CaptureEngine<'_, C> {
    /// §2.5 step 1: disarm if armed, end the stream, accept trailing finals
    /// (they mint into the CLOSING session), then abandon and tear down.
    /// The engine never blocks: with the scripted mocks the stream ends at
    /// close and the drain completes inline; a still-pending real stream is
    /// the shell's wait loop (P6.2), bounded by `DRAIN_WINDOW_MS`.
    fn drain_for_close(&mut self, store: &EventStore, closing: &SessionId) {
        self.session = closing.clone();
        if self.mic.is_armed() {
            // Machine-initiated: NOT §2.1 activity (unlike the user toggle).
            self.disarm_inner(store);
        }
        if self.pipeline.is_some() {
            // Pending past the synchronous close: abandon rather than mint
            // trailing finals into a later session.
            self.finish_drain("session close");
        }
        self.committed.utterances.clear();
        self.committed.strokes.clear();
    }

    /// §2.1 activity feed: arm/disarm toggles, VAD speech (gated frames +
    /// boundary events), and partial/final arrivals while armed.
    fn last_capture_activity(&self) -> Option<(u64, UtcMillis)> {
        self.last_activity
    }

    /// §2.2 rotation follow-through: subsequent commits mint into the new
    /// session and the §9 candidate registry resets (same-session only).
    fn session_rotated(&mut self, opened: &SessionId) {
        self.set_session(opened.clone());
    }
}
