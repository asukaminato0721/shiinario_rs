`movie.mpg` is a synthetic 32×24 MPEG-1 program stream with 12 pictures,
B-frame reordering and mono MP2 sine audio. It contains no game assets.

Generated with:

```sh
ffmpeg -f lavfi -i 'testsrc2=size=32x24:rate=25:duration=0.48' \
  -f lavfi -i 'sine=frequency=440:sample_rate=44100:duration=0.48' \
  -c:v mpeg1video -g 6 -bf 2 -c:a mp2 -b:a 64k -f mpeg movie.mpg
```

Tests read the checked-in fixture; FFmpeg is not a build or runtime dependency.

`audio-fade-probe.json` records 11 synthetic fade cases from each original
engine (2.36 and 2.47), including 5 ms worker samples, zero/default step,
interval rounding, overshoot, equal targets, and the keep-playing flag.
Unicorn executes the original start/worker code; Sleep, thread creation,
thread shutdown, and DirectSound calls are stubbed. Executable SHA-256 hashes
and entry addresses are recorded in the fixture. No game media is included.
The Rust tests compare both simulated time and 44.1/48 kHz mixer output clocks.
