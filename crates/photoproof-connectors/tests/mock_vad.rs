//! The scripted mock VAD: onset events carry the span start (estimated
//! onset, not detection time), the silence gate opens only over speech,
//! detection delay shifts emission but never the onset value, and reset
//! replays identically (determinism).

use photoproof_connectors::mock::{MockVad, SpeechSpan, silent_frame};
use photoproof_connectors::{VadEvent, VoiceActivityDetector};

const RATE: u32 = 16_000;

#[test]
fn onset_and_end_events_fire_once_with_span_values() {
    let mut vad = MockVad::new(
        RATE,
        vec![
            SpeechSpan {
                onset: 250,
                end: 900,
            },
            SpeechSpan {
                onset: 1500,
                end: 1800,
            },
        ],
    );
    assert_eq!(vad.sample_rate(), RATE);

    let mut all_events = Vec::new();
    for i in 0..20 {
        let result = vad.process_frame(&silent_frame(i * 100, 100, RATE));
        all_events.extend(result.events);
    }
    assert_eq!(
        all_events,
        vec![
            VadEvent::SpeechStart { onset: 250 },
            VadEvent::SpeechEnd { end: 900 },
            VadEvent::SpeechStart { onset: 1500 },
            VadEvent::SpeechEnd { end: 1800 },
        ]
    );
}

#[test]
fn silence_gate_opens_only_over_speech() {
    let mut vad = MockVad::new(
        RATE,
        vec![SpeechSpan {
            onset: 250,
            end: 450,
        }],
    );
    let gates: Vec<bool> = (0..6)
        .map(|i| {
            vad.process_frame(&silent_frame(i * 100, 100, RATE))
                .gate_open
        })
        .collect();
    // Frames: [0,100) [100,200) [200,300) [300,400) [400,500) [500,600)
    assert_eq!(gates, vec![false, false, true, true, true, false]);
}

#[test]
fn detection_delay_shifts_emission_frame_but_not_onset_value() {
    // CAPTURE §6.2: `t_start_ms` is the *estimated onset*, not detection
    // time; detection latency must be ≤ 300 ms. Model a 200 ms lag.
    let mut vad = MockVad::new(
        RATE,
        vec![SpeechSpan {
            onset: 250,
            end: 900,
        }],
    )
    .with_detection_delay(200);

    // Frame [200,300) covers the onset itself, but detection point is
    // 450 ms → no event yet.
    for i in 0..4 {
        let result = vad.process_frame(&silent_frame(i * 100, 100, RATE));
        assert!(result.events.is_empty(), "frame {i}: {:?}", result.events);
    }
    // Frame [400,500) crosses 450 ms → SpeechStart, onset still 250.
    let result = vad.process_frame(&silent_frame(400, 100, RATE));
    assert_eq!(result.events, vec![VadEvent::SpeechStart { onset: 250 }]);
}

#[test]
fn one_frame_can_carry_multiple_events_in_order() {
    let mut vad = MockVad::new(
        RATE,
        vec![
            SpeechSpan {
                onset: 100,
                end: 200,
            },
            SpeechSpan {
                onset: 300,
                end: 400,
            },
        ],
    );
    // One 1 s frame covers both spans entirely.
    let result = vad.process_frame(&silent_frame(0, 1000, RATE));
    assert_eq!(
        result.events,
        vec![
            VadEvent::SpeechStart { onset: 100 },
            VadEvent::SpeechEnd { end: 200 },
            VadEvent::SpeechStart { onset: 300 },
            VadEvent::SpeechEnd { end: 400 },
        ]
    );
    assert!(result.gate_open);
}

#[test]
fn reset_replays_identically() {
    let spans = vec![SpeechSpan {
        onset: 250,
        end: 900,
    }];
    let mut vad = MockVad::new(RATE, spans);
    let run = |vad: &mut MockVad| {
        (0..12)
            .map(|i| vad.process_frame(&silent_frame(i * 100, 100, RATE)))
            .collect::<Vec<_>>()
    };
    let first = run(&mut vad);
    vad.reset();
    let second = run(&mut vad);
    assert_eq!(first, second);
}

#[test]
#[should_panic(expected = "non-overlapping")]
fn overlapping_spans_are_a_test_bug() {
    let _ = MockVad::new(
        RATE,
        vec![
            SpeechSpan { onset: 0, end: 500 },
            SpeechSpan {
                onset: 400,
                end: 600,
            },
        ],
    );
}
