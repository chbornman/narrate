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
use photoproof_connectors::feed::AudioFeed;
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
use super::scope::{ScopeRing, ScopeSnapshot, ScopeSubject, ScopeView};
use super::session::CaptureDrain;
use crate::event::StrokePayload;

/// §1 error budget: VAD-onset latency + clock conversion, combined.
pub const ONSET_ERROR_BUDGET_MS: u64 = 250;
/// §5.1 association (B72): the largest |segment onset − VAD onset| a
/// proximity claim may bridge. WHY a bound at all: without one, an
/// ASR-split final (§5.3) whose own onset has no unclaimed partner would
/// steal a DIFFERENT utterance's held snapshot — wrong targets and wrong
/// minted ts persisted into the journal — however many seconds away it is.
/// Beyond the bound the final falls through to §5.3 independent binding.
/// WHY 2 s: token-derived onsets run systematically late (RNN-T emission
/// delay, §5.1), so the SAME utterance's two onsets can disagree by high
/// hundreds of ms (the §13.1 cross-check test pins 900 ms); 2 s clears
/// that with margin while staying below any real between-utterance pause
/// that could carry a scope change.
pub const ASSOCIATION_MAX_SKEW_MS: u64 = 2_000;
/// §2.5/§6.4: trailing-final acceptance window after disarm/end_stream.
pub const DRAIN_WINDOW_MS: u64 = 5_000;
/// §6.3: trailing-silence window shipped to the ASR after the VAD gate
/// closes, so the server-side endpoint rules (rule2 = 1.2 s of trailing
/// silence after decoded speech; rule1 = 2.4 s) can observe the silence
/// they fire on. Without it the endpointer starves and utterances only
/// finalize at disarm.
pub const TRAILING_SHIP_MS: u64 = 3_000;
/// §6.2 pre-roll: silence-gated frames retained and flushed to the ASR
/// the moment shipping resumes. WHY: the VAD only opens its gate once
/// speech-probability crosses ENTER, which is 100-400 ms INTO the first
/// word — without the pre-roll, that audio never reaches the recognizer
/// and cold-start first words come back chopped ("Okay this contact" ->
/// "This contact"; founder corpus, cold-starts card). WHY 1 s and not
/// the ~400 ms detection lag alone: the 560 ms model export holds
/// ~480 ms of attention lookahead, so after a shipping gap the encoder
/// also needs warm left context before the FIRST words' tokens emit —
/// 400 ms lost "Print this one" after 4 s gaps; 1 s restored it
/// (corpus, June 12).
///
/// This is a FEEL DIAL, not a contract (unlike `TRAILING_SHIP_MS` /
/// `DRAIN_WINDOW_MS` / the onset budget just above): it trades cold-start
/// fidelity against replaying stale audio, so it is lifted into
/// `tuning().voice.pre_roll_ms` (DESIGN-TUNING-LOOP.md voice arm) and the cap
/// site below reads THAT. This const remains the CODE DEFAULT that the tuning
/// global resolves to absent a `[voice]` override, so the value is unchanged by
/// construction; it is re-exported for API stability and the tests that pin it.
pub const PRE_ROLL_MS: u64 = 1_000;
/// Debug-panel note cap (in-memory only).
const DEBUG_NOTE_CAP: usize = 256;

/// DESIGN-VOICE-SUBJECTS.md: the seam by which a voice final reaches the
/// collection/topic note logs (`collection_notes` / `topic_notes`). The
/// engine writes EVENTS through its `&EventStore`, but those subject note
/// tables hang off the separate Collections/Topics handles (their own
/// connections over the SAME db); this trait is the thin accessor that lets
/// `on_final` route a subject final to them without the engine owning those
/// handles. The shell wires a real sink over `Arc<Collections>`/`Arc<Topics>`;
/// tests use a fake. Text is verbatim (K14); `ts` is wall-clock-now, exactly
/// as the typed composer commands pass `UtcMillis::now()`.
pub trait SubjectNoteSink: Send {
    /// Append `text` to `collection_id`'s note log. `Err` is logged to the
    /// debug ring (a stale id is the realistic failure); it never mints an
    /// image event as a fallback (that would silently misroute the words).
    fn append_collection_note(
        &self,
        collection_id: &str,
        text: &str,
        ts: UtcMillis,
    ) -> Result<(), String>;
    /// Append `text` to `topic_id`'s note log.
    fn append_topic_note(&self, topic_id: &str, text: &str, ts: UtcMillis) -> Result<(), String>;
}

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
    /// Associated ASR utterance id, once a segment claimed this onset
    /// (exact id match first, otherwise nearest onset — see `associate`).
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
    /// Stream-clock position of the last gate-open frame; drives the
    /// trailing-silence ship window (§6.3, `TRAILING_SHIP_MS`).
    last_voiced_at: Option<StreamMs>,
    /// Frames withheld by the silence gate, newest last, capped at
    /// `PRE_ROLL_MS` of audio — flushed ahead of the first shipped frame
    /// when shipping resumes so the recognizer hears the word from its
    /// true onset (§6.2 pre-roll).
    pre_roll: std::collections::VecDeque<AudioFrame>,
    debug: Vec<String>,
    /// §2.1: most recent capture-side ACTIVITY — mic arm/disarm, VAD
    /// speech (gated frames and boundary events), partial/final arrival
    /// while armed — as `(capture clock ms, wall clock)`. Fed to the
    /// session engine through the `CaptureDrain` seam so speech refreshes
    /// the idle timer (§2.2: a boundary never bisects an in-flight
    /// utterance).
    last_activity: Option<(u64, UtcMillis)>,
    /// DESIGN-VOICE-SUBJECTS.md: the subject-note seam, wired by the shell
    /// (`with_note_sink`). `None` in the bare engine — a subject final then
    /// has nowhere to land, so it logs and mints nothing (it must NOT fall
    /// back to an image event). Optional so the existing `new` signature and
    /// every test that does not exercise subjects stay untouched.
    note_sink: Option<Box<dyn SubjectNoteSink>>,
    /// Per-engine pre-roll cap override, ms. `None` (the bare/shell engine)
    /// reads the VOICE DIAL `tuning().voice.pre_roll_ms` from the process-global
    /// tuning. The voice SWEEP (`pp-sweep voice`) sets this per config because it
    /// runs every grid config in ONE process — the `OnceLock` tuning global can
    /// only be installed once, so a per-config pre-roll cannot route through it;
    /// this explicit override is that seam. It does NOT change the shipped path
    /// (the shell never sets it, so the global dial still governs production).
    pre_roll_ms_override: Option<u64>,
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
            last_voiced_at: None,
            pre_roll: std::collections::VecDeque::new(),
            debug: Vec::new(),
            last_activity: None,
            note_sink: None,
            pre_roll_ms_override: None,
        }
    }

    /// Override the pre-roll cap (ms) for THIS engine, bypassing the process
    /// tuning global. Builder form (like `with_note_sink`) so the bare `new`
    /// signature and every test stay untouched; only `pp-sweep voice` (which
    /// sweeps pre-roll per config in one process) sets it. See the field's WHY.
    pub fn with_pre_roll_ms(mut self, pre_roll_ms: u64) -> Self {
        self.pre_roll_ms_override = Some(pre_roll_ms);
        self
    }

    /// DESIGN-VOICE-SUBJECTS.md: wire the subject-note seam (the shell's
    /// `Arc<Collections>`/`Arc<Topics>` accessor). Builder form so the bare
    /// `new` signature — used by ~15 acceptance tests and the pump's
    /// reconstruction — stays untouched; only the live shell and the
    /// subject-routing tests opt in.
    pub fn with_note_sink(mut self, sink: Box<dyn SubjectNoteSink>) -> Self {
        self.note_sink = Some(sink);
        self
    }

    /// Record capture-side activity NOW (§2.1) for the session engine's
    /// idle decisions.
    fn touch_activity(&mut self) {
        self.last_activity = Some((self.clock.mono_ms(), self.clock.wall()));
    }

    // -- scope (§3) -----------------------------------------------------------

    /// The UI reports its selection/view-derived target list; the engine
    /// snapshots it into the ring. Returns the echoed snapshot. Image-only
    /// entry point (no subject) — kept for the bare callers/tests.
    pub fn set_scope(&mut self, targets: Vec<ContentHash>) -> &ScopeSnapshot {
        self.set_scope_with_subject(targets, None, None)
    }

    /// DESIGN-VOICE-SUBJECTS.md: the UI reports its targets AND (when a
    /// collection/topic detail is open with no image focused) a non-image
    /// subject. The subject rides onto the pushed snapshot so it is
    /// onset-bound and frozen for the utterance, exactly like image targets.
    pub fn set_scope_with_subject(
        &mut self,
        targets: Vec<ContentHash>,
        subject: Option<ScopeSubject>,
        subject_name: Option<String>,
    ) -> &ScopeSnapshot {
        let (m, w) = (self.clock.mono_ms(), self.clock.wall());
        // WHY union into open utterances: a single voice dictation that spans
        // an image swap (the user starts speaking on image A, then
        // arrow-navigates to B mid-sentence) belongs to EVERY image they were
        // looking at while speaking — there are no per-word ASR timestamps to
        // split the text on, so the verbatim note lands on every viewed image
        // (founder decision, June 13 2026). The ring still records each
        // snapshot for the normal onset lookup; this only ADDS the new
        // targets to any utterance already in flight across the swap.
        //
        // Order is the order the images were VIEWED (onset image first, later
        // images appended), and `event_targets.position` (which drives
        // select-journal-targets and the note's target ordering) follows it,
        // so we append first-seen and de-dup — A→B yields [A.., B..],
        // A→B→A stays [A.., B..]. Every open utterance spanned this swap, so
        // union into all of them.
        //
        // DESIGN-VOICE-SUBJECTS.md: the union is IMAGE-targets-only and never
        // touches `subject` — a subject snapshot has empty `targets`, so this
        // loop is a no-op for an utterance bound to a subject (the onset
        // subject stays frozen; focusing an image mid-utterance does NOT
        // retro-rebind it). And a subject's id never arrives through this
        // path's `targets`, so it cannot be unioned in.
        for u in &mut self.in_flight {
            for t in &targets {
                if !u.snapshot.targets.contains(t) {
                    u.snapshot.targets.push(t.clone());
                }
            }
            // The held snapshot's kind tracks its grown target count, so the
            // §11 indicator's bound-scope view stays accurate (single→multi
            // once a second image joins an open utterance).
            u.snapshot.kind = super::scope::ScopeKind::from_target_count(u.snapshot.targets.len());
        }
        self.ring.push_scope(targets, subject, subject_name, m, w)
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
                self.last_voiced_at = None; // new stream, new clock
                self.pre_roll.clear();
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
        // §6.3: the ASR owns endpointing, and its trailing-silence rules
        // fire on RECEIVED audio — fully gating silence starves the
        // endpointer and no final ever mints (founder dogfood: partials
        // forever, zero journal entries). Ship the gate-open frames PLUS
        // a trailing window long enough for the server-side rules to see
        // their silence; long armed silence still ships nothing.
        if result.gate_open {
            self.last_voiced_at = Some(frame.captured_at);
        }
        let ship = result.gate_open
            || self
                .last_voiced_at
                .is_some_and(|t| frame.captured_at.saturating_sub(t) <= TRAILING_SHIP_MS);
        if ship {
            if let Some(p) = &self.pipeline {
                // §6.2 pre-roll: the withheld frames immediately before
                // this one carry the chopped start of the word that opened
                // the gate — they ship FIRST, in order (their true
                // captured_at rides along, so the B72 ship-clock mapping
                // stays exact).
                for held in self.pre_roll.drain(..) {
                    p.feed.push(held);
                }
                p.feed.push(frame);
            }
        } else {
            // Withheld: retain for the pre-roll, oldest evicted past the cap.
            // The cap is the VOICE DIAL `tuning().voice.pre_roll_ms` (a feel
            // knob the founder sweeps), defaulting to `PRE_ROLL_MS` absent a
            // `[voice]` override — so the cap is unchanged by construction. The
            // per-engine override (set only by `pp-sweep voice`) wins when
            // present, since that sweep varies pre-roll per config in one
            // process where the tuning global can be installed only once.
            let pre_roll_ms = self
                .pre_roll_ms_override
                .unwrap_or_else(|| crate::tuning::tuning().voice.pre_roll_ms);
            self.pre_roll.push_back(frame);
            while let (Some(oldest), Some(newest)) = (self.pre_roll.front(), self.pre_roll.back()) {
                if newest.captured_at.saturating_sub(oldest.captured_at) <= pre_roll_ms {
                    break;
                }
                self.pre_roll.pop_front();
            }
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
    ///
    /// The §2.5/§6.4 drain deadline gates EVERY iteration, not just
    /// `Poll::Pending` (P6.2 hardening): a final that is merely *ready*
    /// after the 5 s cap does not mint, and a stream that never returns
    /// Pending (the real wire can keep a queue warm) cannot defeat the
    /// cap — the clock re-read per iteration bounds the loop.
    pub fn pump(&mut self, store: &EventStore) -> Vec<Event> {
        let mut committed = Vec::new();
        while self.pipeline.is_some() {
            let deadline_passed = self
                .pipeline
                .as_ref()
                .and_then(|p| p.drain_deadline)
                .is_some_and(|d| self.clock.mono_ms() >= d);
            if deadline_passed {
                self.finish_drain("5 s drain window elapsed");
                break;
            }
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
                Poll::Pending => break,
            }
        }
        committed
    }

    fn on_partial(&mut self, seg: &TranscriptSegment) {
        self.associate(seg.utterance_id, seg.onset);
        if self.mic.is_armed() {
            self.mic = MicState::ArmedSpeaking;
            self.touch_activity(); // §2.1: Partial while armed is activity
        }
        // Partials are NEVER persisted and never displayed in the
        // indicator; the DEV-BUILD debug panel MAY show them (§6.5) —
        // release builds keep the ring (timing diagnostics) but the TEXT
        // is cfg-gated out (P6.2 obligation: partials are dev-build
        // debug territory, in every memory the app holds).
        #[cfg(debug_assertions)]
        self.note(format!(
            "partial[{}] @{}ms: {:?}",
            seg.utterance_id, seg.onset, seg.text
        ));
        #[cfg(not(debug_assertions))]
        self.note(format!(
            "partial[{}] @{}ms: <text elided in release>",
            seg.utterance_id, seg.onset
        ));
    }

    /// Association by ONSET PROXIMITY (B72, amends B49's FIFO rule): a
    /// segment keeps its exact utterance-id match (ids are stable from
    /// first partial through final); otherwise it claims the unclaimed
    /// in-flight whose VAD onset is nearest the segment's own onset —
    /// both on the CAPTURE stream clock; the Transcriber contract makes
    /// the connector translate backend sample-count time back through
    /// the silence gate's withheld stretches (transcriber.rs). FIFO
    /// claiming ("first unclaimed wins") was wrong whenever the VAD split
    /// speech into more utterances than the ASR endpointer did — a
    /// ~0.8 s pause splits the VAD at its 480 ms hang but merges under
    /// the ASR's 1.2 s trailing-silence rule — so the NEXT final bound
    /// the leftover merged-away onset instead of its own (pp_voice_bench
    /// defect, June 2026). A claim must bridge no more than
    /// `ASSOCIATION_MAX_SKEW_MS`; farther apart, the segment gets no
    /// claim and a final falls through to §5.3 independent binding.
    /// Proximity only changes WHICH held snapshot a segment claims; the
    /// held VAD snapshot stays authoritative for binding (§5.1), and the
    /// segment onset is never used to rebind a claimed snapshot.
    fn associate(&mut self, utterance_id: u64, onset: StreamMs) -> Option<usize> {
        if let Some(i) = self
            .in_flight
            .iter()
            .position(|u| u.utterance_id == Some(utterance_id))
        {
            return Some(i);
        }
        let nearest = self
            .in_flight
            .iter()
            .enumerate()
            .filter(|(_, u)| u.utterance_id.is_none())
            .map(|(i, u)| (i, u.onset_stream.abs_diff(onset)))
            .filter(|&(_, skew)| skew <= ASSOCIATION_MAX_SKEW_MS)
            .min_by_key(|&(_, skew)| skew)
            .map(|(i, _)| i)?;
        self.in_flight[nearest].utterance_id = Some(utterance_id);
        Some(nearest)
    }

    /// The ASR endpointer can MERGE what the VAD split (§6.3: the ASR owns
    /// utterance ends; its trailing-silence rule outlasts the VAD hang).
    /// Any other unclaimed onset inside a final's span was consumed by that
    /// final: retire it now — settled, nothing minted, not counted
    /// abandoned (the third §6.5 lifecycle exit, B72). It will never get
    /// its own final, and leaving it in flight is what stranded the next
    /// final on the wrong onset and counted a phantom abandon at disarm
    /// (pp_voice_bench defect, June 2026). This runs for EVERY final,
    /// minting or not: a whitespace-only final consumed its merged onsets
    /// all the same, and stranding them keeps the mic ArmedSpeaking and
    /// the indicator streaming forever.
    ///
    /// Returns the farthest stream-clock extent of the retired onsets
    /// (their VAD ends; the segment's own end as fallback): the claiming
    /// utterance's durable span must grow to cover the speech it absorbed,
    /// or a stroke drawn during the merged tail loses the link that §9.2's
    /// in-flight suppression promised the utterance would carry.
    fn retire_merged(&mut self, seg: &TranscriptSegment) -> Option<StreamMs> {
        let mut merged_extent: Option<StreamMs> = None;
        let mut i = 0;
        while i < self.in_flight.len() {
            let u = &self.in_flight[i];
            if u.utterance_id.is_none() && u.onset_stream >= seg.onset && u.onset_stream <= seg.end
            {
                let u = self.in_flight.remove(i);
                let extent = u.end_stream.unwrap_or(seg.end).max(u.onset_stream);
                merged_extent = Some(merged_extent.map_or(extent, |m| m.max(extent)));
                self.note(format!(
                    "utterance onset at stream {} ms merged into final[{}]; \
                     settled without minting",
                    u.onset_stream, seg.utterance_id
                ));
            } else {
                i += 1;
            }
        }
        self.settle_speaking_state();
        merged_extent
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
            .associate(seg.utterance_id, seg.onset)
            .map(|i| self.in_flight.remove(i));

        // Any other unclaimed onset inside this final's span was merged
        // into it by the ASR endpointer — settle those, mint nothing for
        // them. BEFORE the empty-text return: a whitespace final consumed
        // its merged onsets just the same (retire_merged's WHY).
        let merged_extent = self.retire_merged(seg);

        // Empty/whitespace-only finals mint NOTHING (§6.5).
        // retire_merged already settled the speaking state.
        if seg.text.trim().is_empty() {
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
                    .unwrap_or(seg.end)
                    // Fold the retired merged onsets' extent in: this
                    // final's text covers their speech, so the durable
                    // span (dur_ms, §9 linking) must too — otherwise the
                    // interval between the held VAD end and the merged
                    // tail belongs to no committed span and a suppressed
                    // stroke link (§9.2) is dropped.
                    .max(merged_extent.unwrap_or(0));
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
                    // Same merged-extent fold as the held branch: the skew
                    // bound can leave associate() empty-handed while a
                    // farther unclaimed onset still sat inside this span.
                    anchor + seg.end.max(merged_extent.unwrap_or(0)),
                )
            }
        };

        let end_mono = end_mono.max(onset_mono);

        // DESIGN-VOICE-SUBJECTS.md routing: a snapshot carrying a SUBJECT
        // (collection/topic, frozen at onset) routes the verbatim text to
        // that subject's note log instead of minting an image event. WHY
        // before the image path: a subject final must NEVER mint an image
        // Remark (and a subject snapshot has empty targets, so it would
        // otherwise fall through as a zero-target SESSION note — exactly the
        // wrong place). The image-targets-win invariant is upheld upstream
        // (a subject only ever rides a targetless snapshot), so this branch
        // only fires when there are no image targets to honor.
        if let Some(subject) = snapshot.subject.clone() {
            // K14: machine routes verbatim user speech; never composes. Same
            // trim the image path applies — BPE word-boundary spacing is
            // tokenizer plumbing, not the user's words.
            let text = seg.text.trim().to_owned();
            let ts = minted.ts; // onset wall time, like the image Remark's ts
            let result = match (&subject, self.note_sink.as_ref()) {
                (ScopeSubject::Collection(id), Some(sink)) => {
                    sink.append_collection_note(id, &text, ts)
                }
                (ScopeSubject::Topic(id), Some(sink)) => sink.append_topic_note(id, &text, ts),
                // No sink wired (bare engine): nothing to do but log. We do
                // NOT fall through to an image/session mint — that would
                // misroute the user's words to the wrong target.
                (_, None) => Err("no subject-note sink wired".to_owned()),
            };
            match result {
                Ok(()) => {
                    self.audio.note_finalized(now_mono);
                    self.settle_speaking_state();
                }
                Err(e) => {
                    self.note(format!("subject note append failed: {e}"));
                    self.settle_speaking_state();
                }
            }
            // A subject final mints NO event (it is not in `event_targets`);
            // its note lives in collection_notes/topic_notes.
            return None;
        }

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
            // Trimmed, not raw: BPE-style ASR tokens carry their word-
            // boundary space, so every utterance's first token decodes as
            // " Slow" and untrimmed finals saved a leading " " on EVERY
            // voice note (founder dogfood, June 12 2026 — confirmed in the
            // store). §6.5 "verbatim" protects the user's WORDS from
            // paraphrase; tokenizer plumbing at the edges is not words.
            // Interior spacing is untouched.
            text: seg.text.trim().to_owned(),
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
        // The pre-roll holds raw samples: same §7/K10 hygiene as the ring.
        self.pre_roll.clear();
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
