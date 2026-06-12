# Interactive voice-chunking session: pick a reading card, record a take,
# and the recording runs through the REAL pipeline (pp_voice_bench: silero
# VAD + pp-asr-server + capture engine), printing the journal entries that
# would have been minted - right under what you read.
#
# Recordings persist in test-corpora/voice/ (gitignored) so every future
# engine/knob change re-runs against the same takes. Multiple speakers and
# mic setups are first-class: both ride every filename.
#
# Run with the spike venv:
#   /Users/bornman/projects/narrate/spike-p6.3/venv/bin/python \
#       /Users/bornman/spike-p7-embed/voice_session.py
#
# Optional: pass pp_voice_bench flags after "--" to sweep knobs against a
# voice, e.g.  ... voice_session.py -- --rule2 0.9 --enter 0.4

import subprocess
import sys
import time
from pathlib import Path

import numpy as np
import sounddevice as sd

REPO = Path("/Users/bornman/projects/narrate")
OUT = REPO / "test-corpora/voice"
BENCH = REPO / "target/debug/pp_voice_bench"
SR = 16_000

# Each card: (name, what it probes, direction, text). [PAUSE Ns] means stop
# talking and count the seconds silently, then resume. Do NOT worry about
# reading word-perfectly - speak naturally; the pauses and the energy
# directions are what the cards are really testing.
CARDS = [
    (
        "baseline",
        "normal speech, the reference take",
        "Normal voice, conversational pace. Take the marked pauses.",
        """This is the baseline card, spoken at a normal pace.
[PAUSE 1s]
The harbor series needs a tighter edit before printing.
[PAUSE 2s]
Keep the one with the yellow umbrella, drop the rest.
[PAUSE 1s]
Something about the light in the last frame keeps pulling me back.""",
    ),
    (
        "pause-ladder",
        "maps exactly where the split boundary falls for this voice",
        "Normal voice. Say each line, then pause for the marked time -"
        " count carefully, the ladder only works if the pauses differ.",
        """The first pause is half a second.
[PAUSE 0.5s]
The second pause is one second.
[PAUSE 1s]
The third pause is one and a half seconds.
[PAUSE 1.5s]
The fourth pause is two seconds.
[PAUSE 2s]
The fifth pause is three seconds.
[PAUSE 3s]
And this is the last line of the ladder.""",
    ),
    (
        "cold-starts",
        "first-word chopping after long silence",
        "After each long pause, start speaking ABRUPTLY - no breath sound,"
        " no throat-clearing, hit the first word cold.",
        """Keeper.
[PAUSE 4s]
Print this one for the show in October.
[PAUSE 4s]
Reject, the focus missed.
[PAUSE 4s]
Maybe. Ask me again next month.
[PAUSE 4s]
Best of the day.""",
    ),
    (
        "culling-cadence",
        "the real culling workflow: rapid single-word verdicts",
        "Like flipping through a contact sheet fast: one word per photo,"
        " about a second between words. Steady rhythm.",
        """Keep.
[PAUSE 1s]
Keep.
[PAUSE 1s]
Reject.
[PAUSE 1s]
Keep.
[PAUSE 1s]
Reject.
[PAUSE 1s]
Reject.
[PAUSE 1s]
Keep.
[PAUSE 1s]
Love it.""",
    ),
    (
        "short-bursts-energetic",
        "loud punchy fragments, clean splits",
        "LOUD and energetic, like you just found the best frame of the"
        " year. Short punchy fragments, full stops between them.",
        """Yes! This one!
[PAUSE 2s]
Print it large!
[PAUSE 1s]
Best frame of the whole trip, no contest.
[PAUSE 2s]
Wow.
[PAUSE 3s]
Absolutely the cover shot.""",
    ),
    (
        "somber-mumble-rambling",
        "quiet sustained speech - the mumble-dropout case",
        "Quiet, low energy, a bit mumbly - thinking out loud late at"
        " night. ONE flowing thought, no deliberate pauses, just breath.",
        """I keep coming back to this frame and I am honestly not sure why,
maybe it is the way the fog flattens the bridge into a gray shape, or the
single figure at the rail, and it reminds me of that winter series from
two years ago that I never finished editing, which is probably the real
reason it bothers me, because the work was good and I just walked away
from it, and this frame feels like an invitation to go back.""",
    ),
    (
        "whisper",
        "the extreme quiet case - does the VAD hear it at all",
        "An actual WHISPER, like someone is asleep in the next room."
        " Whisper the whole card.",
        """This one is special.
[PAUSE 2s]
Do not tell anyone, but I think this is the best thing I have ever shot.
[PAUSE 2s]
The whisper test ends here.""",
    ),
    (
        "mixed-register",
        "energy decay within one take",
        "Start energetic and loud, then trail off into quiet mumbling by"
        " the end - one continuous take with the marked pauses.",
        """Okay this contact sheet is actually incredible.
[PAUSE 1s]
Three keepers in one roll, that never happens.
[PAUSE 2s]
The dog one is the obvious crowd pleaser.
[PAUSE 1s]
But the quiet one at the end, the empty bench,
that is the one I will still care about in ten years.""",
    ),
    (
        "fast-talker",
        "dense rapid speech, no pauses",
        "As FAST as you can comfortably talk, one breath, no pauses -"
        " like you are double-parked and late.",
        """Right so the plan is keep the first three drop the blurry one
print the dog shot large get the bench one framed for mom and the rest go
in the maybe pile which we both know means never and that is fine.""",
    ),
    (
        "long-monologue",
        "the 20-second forced endpoint - where does it split",
        "Normal voice, keep talking continuously for at least thirty"
        " seconds. Loop the text again from the top if you run out before"
        " thirty seconds. No deliberate pauses.",
        """The thing about a contact sheet is that it shows your thinking,
not just your pictures, every frame you took on the way to the one that
worked, all the almosts and the misses sitting right next to the keeper,
and when you mark it up you are really annotating your own decision
making, which is why throwing away the outtakes always feels wrong to me,
because the path matters as much as the destination, and a year from now
the reject teaches you more than the print does.""",
    ),
    (
        "false-starts",
        "restarts, abandoned words, mid-sentence pivots",
        "Natural voice. Deliberately stumble where marked - cut yourself"
        " off mid-word and restart, like real thinking-out-loud.",
        """This one is the str- no wait, actually the second one is stronger.
[PAUSE 1s]
Keep the- hmm.
[PAUSE 2s]
What I mean is, the crop, no, the FRAMING is what sells it.
[PAUSE 1s]
Scratch all that. It is a keeper. Done.""",
    ),
    (
        "fillers-and-hums",
        "do ums and hums trigger or pollute entries",
        "Natural voice, lots of thinking sounds: long umm, uhh, hmm where"
        " marked. Let them drag.",
        """Ummmm.
[PAUSE 1s]
So this one is, uhhh, maybe a keeper.
[PAUSE 1s]
Hmmmmm.
[PAUSE 2s]
Yeah, uh, the light is, um, doing something interesting on the left.""",
    ),
    (
        "numbers-and-names",
        "the vocabulary a photo journal actually uses",
        "Normal voice - this card is about WORDS, not pauses: dates, gear,"
        " film stocks, places. Say them clearly but naturally.",
        """Shot on the 35 millimeter at f 2.8, October 14th, 2024.
[PAUSE 1s]
The Portra 400 rolls from the Reykjavik trip are back from the lab.
[PAUSE 1s]
Frame 27 beats frame 31, barely.
[PAUSE 1s]
This is from the Fuji X100 with the ND filter, 1 250th of a second.""",
    ),
    (
        "freeform",
        "natural unscripted speech - the truest test",
        "No script: pick a photo you actually took recently, picture it,"
        " and talk about it for 20-30 seconds the way you would to a"
        " friend. Pause whenever you naturally would.",
        """(no text - describe one of your own photographs, from memory,
in your own words, with your natural rhythm)""",
    ),
    (
        "arms-length",
        "lower signal: speaking away from the laptop",
        "Lean BACK from the laptop - arm's length or more, like you are"
        " sitting back in your chair reviewing. Normal voice.",
        """Reviewing from the couch now.
[PAUSE 2s]
This is roughly how I would actually use it, leaned back, thinking.
[PAUSE 1s]
The question is whether it still hears the quiet parts from here.""",
    ),
    (
        "background-noise",
        "real-world noise floor",
        "Put on music or a podcast at a normal listening volume nearby,"
        " then speak the card in a normal voice over it.",
        """There is music playing while I say this.
[PAUSE 2s]
Real rooms are never silent, so the gate has to cope with this.
[PAUSE 1s]
Keep the third frame, lose the rest.""",
    ),
]


def record() -> np.ndarray:
    print("\n  recording - press Enter to STOP")
    chunks: list[np.ndarray] = []

    def cb(indata, _frames, _time, _status):
        chunks.append(indata.copy())

    with sd.InputStream(samplerate=SR, channels=1, dtype="float32", callback=cb):
        input()
    audio = np.concatenate(chunks)[:, 0] if chunks else np.zeros(0, dtype=np.float32)
    print(f"  captured {len(audio) / SR:.1f}s")
    return audio


def write_wav(path: Path, samples: np.ndarray):
    pcm = (np.clip(samples, -1, 1) * 32767).astype("<i2").tobytes()
    n = len(pcm)
    hdr = (
        b"RIFF" + (36 + n).to_bytes(4, "little") + b"WAVEfmt "
        + (16).to_bytes(4, "little") + (1).to_bytes(2, "little")
        + (1).to_bytes(2, "little") + SR.to_bytes(4, "little")
        + (SR * 2).to_bytes(4, "little") + (2).to_bytes(2, "little")
        + (16).to_bytes(2, "little") + b"data" + n.to_bytes(4, "little")
    )
    path.write_bytes(hdr + pcm)


def run_bench(wav: Path, extra: list[str]) -> str:
    cmd = [str(BENCH), "--wav", str(wav), *extra]
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    return (out.stdout + out.stderr).strip()


SETUPS = [
    ("builtin", "MacBook built-in microphone"),
    ("usb", "External USB / XLR microphone"),
    ("headset", "AirPods / bluetooth headset"),
    ("other", "Something else (you will be asked to describe it)"),
]


def ask_setup() -> tuple[str, str]:
    # The setup tag rides every filename: VAD thresholds that suit the
    # noise-suppressed built-in may not suit a condenser, so takes must
    # never be compared across setups unknowingly.
    device = sd.query_devices(sd.default.device[0])["name"]
    print(f"Default input device: {device}\n")
    for i, (_, label) in enumerate(SETUPS, 1):
        print(f"  {i}. {label}")
    choice = input("Which setup is this? [1] > ").strip() or "1"
    tag = SETUPS[int(choice) - 1][0] if choice in "1234" else "other"
    desc = SETUPS[int(choice) - 1][1] if choice in "1234" else ""
    if tag == "other":
        desc = input("Describe it briefly > ").strip() or "unspecified"
    return tag, f"{desc} (device: {device})"


def ask_speaker() -> str:
    # Friends help record: the speaker tag keeps voices separable so a
    # threshold tuned on one voice is never silently validated on another.
    name = input("Speaker name or initials (e.g. caleb, jt) > ").strip().lower()
    return "".join(c for c in name if c.isalnum()) or "anon"


def take_count(name: str, setup: str, speaker: str) -> int:
    return len(list(OUT.glob(f"{name}-{setup}-{speaker}-take*.wav")))


def menu(setup: str, speaker: str):
    print("\n" + "=" * 72)
    print(f"CARDS  (speaker: {speaker}, setup: {setup} - takes you already recorded)")
    for i, (name, probes, _, _) in enumerate(CARDS, 1):
        n = take_count(name, setup, speaker)
        marker = f"  [{n} take{'s' if n != 1 else ''}]" if n else ""
        print(f"  {i:2}. {name:24} - {probes}{marker}")
    print("\n  number = record that card | a = all cards in order | q = quit")


def run_card(idx: int, setup: str, speaker: str, extra: list[str], log: list):
    name, probes, direction, text = CARDS[idx]
    take = take_count(name, setup, speaker) + 1
    print("=" * 72)
    print(f"CARD: {name}   (take {take})")
    print(f"PROBES: {probes}")
    print(f"DIRECTION: {direction}\n")
    print(text)
    print("\n  Enter = start recording | s = skip")
    if input("  > ").strip().lower() == "s":
        return
    audio = record()
    if len(audio) < SR:
        print("  under a second - discarded, run the card again")
        return
    wav = OUT / f"{name}-{setup}-{speaker}-take{take}.wav"
    write_wav(wav, audio)
    print(f"  saved {wav.relative_to(REPO)}")
    print("  running the pipeline (model load + decode, ~10-30s)...\n")
    t0 = time.time()
    result = run_bench(wav, extra)
    print("  " + "\n  ".join(result.splitlines()))
    print(f"\n  ({time.time() - t0:.0f}s)")
    log.append((str(wav), result))


def main():
    extra = []
    if "--" in sys.argv:
        extra = sys.argv[sys.argv.index("--") + 1 :]
    if not BENCH.exists():
        sys.exit(f"build it first: cargo build --bin pp_voice_bench (missing {BENCH})")
    OUT.mkdir(parents=True, exist_ok=True)
    print("Voice chunking session. Speak naturally - word-perfect reading")
    print("does NOT matter; the pauses and energy directions do.")
    print(f"Recordings persist in {OUT}\n")
    setup_tag, setup_desc = ask_setup()
    speaker = ask_speaker()
    if extra:
        print(f"bench knobs this session: {' '.join(extra)}")

    log = [("session", f"speaker={speaker} setup={setup_tag}: {setup_desc}")]
    while True:
        menu(setup_tag, speaker)
        choice = input("  > ").strip().lower()
        if choice == "q":
            break
        if choice == "a":
            for i in range(len(CARDS)):
                run_card(i, setup_tag, speaker, extra, log)
            continue
        if choice.isdigit() and 1 <= int(choice) <= len(CARDS):
            run_card(int(choice) - 1, setup_tag, speaker, extra, log)

    if len(log) > 1:
        session = OUT / f"session-{speaker}-{time.strftime('%Y%m%d-%H%M%S')}.log"
        session.write_text(
            "\n\n".join(f"== {w}\n{r}" for w, r in log) + "\n", encoding="utf-8"
        )
        print(f"\nsession log: {session}")


if __name__ == "__main__":
    main()
