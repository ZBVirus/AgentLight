#!/usr/bin/env python3
"""Regenerate `src-tauri/assets/alarm.wav`, AgentLight's default alarm.

Pure stdlib, no dependencies. The sound is a short two-note rising chime
(E5 -> B5) with a soft attack and exponential decay, so the default alarm is
pleasant instead of a console beep. Run from the repo root:

    python3 scripts/make_alarm.py
"""

from __future__ import annotations

import math
import pathlib
import struct
import wave

SAMPLE_RATE = 22050
AMPLITUDE = 0.78
# (frequency Hz, duration seconds); the notes overlap slightly.
NOTES = [(659.25, 0.24), (987.77, 0.42)]
GAP = 0.02


def note(frequency: float, duration: float) -> list[float]:
    samples = []
    total = int(duration * SAMPLE_RATE)
    for i in range(total):
        t = i / SAMPLE_RATE
        # Fundamental plus two quiet harmonics give the note some body.
        value = (
            math.sin(2 * math.pi * frequency * t)
            + 0.35 * math.sin(4 * math.pi * frequency * t)
            + 0.12 * math.sin(6 * math.pi * frequency * t)
        )
        # 5 ms attack, exponential decay, 8 ms release at the tail.
        attack = min(1.0, t / 0.005)
        decay = math.exp(-t * 6.0)
        release = min(1.0, (duration - t) / 0.008)
        samples.append(value * attack * decay * release)
    return samples


def main() -> None:
    track: list[float] = []
    for frequency, duration in NOTES:
        track.extend(note(frequency, duration))
        track.extend([0.0] * int(GAP * SAMPLE_RATE))

    peak = max(abs(value) for value in track) or 1.0
    frames = b"".join(
        struct.pack("<h", int(value / peak * AMPLITUDE * 32767)) for value in track
    )

    out = pathlib.Path(__file__).resolve().parent.parent / "src-tauri" / "assets" / "alarm.wav"
    out.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(out), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(SAMPLE_RATE)
        wav.writeframes(frames)
    print(f"wrote {out} ({out.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
