use shiinario_runtime::{
    audio::Mixer,
    resources::{AudioStream, Sound, StreamVolume},
};
use shiinario_scenario::{BinaryVm, Event, PlatformRequest};
use std::sync::Arc;

fn stream(percent: u32) -> Arc<AudioStream> {
    let stream = Arc::new(AudioStream {
        sound: Sound {
            sample_rate: 48000,
            channels: 1,
            samples: vec![16384; 48000],
        },
        loop_start_frame: 0,
        first_block_frames: 16,
        volume: StreamVolume::default(),
    });
    stream.volume.set_percent(percent).unwrap();
    stream
}

#[test]
fn original_workers_match_rendered_and_simulated_timing() {
    // Original 2.36 and 2.47 start/worker routines executed with Unicorn.
    // Sleep, thread creation and DirectSound calls were stubbed; volume,
    // counters, termination and stop decisions ran as original x86 code.
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/audio-fade-probe.json")).unwrap();
    for version in fixture.as_array().unwrap() {
        for case in version["cases"].as_array().unwrap() {
            for rate in [0, 44100, 48000] {
                let stream = stream(case["initial"].as_u64().unwrap() as u32);
                let mut mixer = Mixer::default();
                mixer.play(1, stream.clone(), 2).unwrap();
                mixer
                    .fade(
                        1,
                        case["interval"].as_u64().unwrap() as u32,
                        case["step"].as_i64().unwrap() as i32,
                        case["target"].as_u64().unwrap() as u32,
                    )
                    .unwrap();
                let mut elapsed = 0;
                for sample in case["samples"].as_array().unwrap() {
                    let ms = sample[0].as_u64().unwrap() as u32;
                    if rate == 0 {
                        mixer.advance_ms(ms - elapsed);
                    } else {
                        // Observe the first output frame at or beyond each timestamp.
                        let frames = (u64::from(ms) * rate).div_ceil(1000)
                            - (u64::from(elapsed) * rate).div_ceil(1000);
                        mixer
                            .render(&mut vec![0.; frames as usize], rate as u32, 1)
                            .unwrap();
                    }
                    elapsed = ms;
                    assert_eq!(
                        stream.volume.percent(),
                        sample[1].as_u64().unwrap() as u32,
                        "{} at {ms} ms, rate={rate}: {case}",
                        version["version"]
                    );
                    assert_eq!(mixer.is_playing(1), !sample[2].as_bool().unwrap());
                }
            }
        }
    }
}

#[test]
fn fade_changes_samples_and_replacement_stop_and_replay_cancel_it() {
    let stream = stream(100);
    let mut mixer = Mixer::default();
    mixer.play(1, stream.clone(), 2).unwrap();
    mixer.fade(1, 30, -20, 0x80000000).unwrap();
    let mut out = vec![0.; 1680];
    mixer.render(&mut out, 48000, 1).unwrap();
    assert!(out.iter().all(|&x| x == 0.5));
    assert_eq!(stream.volume.percent(), 80);
    let mut next = [0.];
    mixer.render(&mut next, 48000, 1).unwrap();
    assert!(next[0] > 0. && next[0] < 0.5);
    // Equal target cancels the existing fade without stopping playback.
    mixer.fade(1, 0, -1, 80).unwrap();
    mixer.advance_ms(100);
    assert_eq!(stream.volume.percent(), 80);
    assert!(mixer.is_playing(1));
    // Replacement restarts the interval, and direct volume writes remain
    // visible to the next fade step, as in the original worker.
    mixer.fade(1, 30, -10, 0x80000000).unwrap();
    mixer.advance_ms(34);
    mixer.fade(1, 30, 10, 100).unwrap();
    stream.volume.set_percent(60).unwrap();
    mixer.advance_ms(34);
    assert_eq!(stream.volume.percent(), 60);
    mixer.advance_ms(1);
    assert_eq!(stream.volume.percent(), 70);
    mixer.stop(1);
    mixer.advance_ms(100);
    assert_eq!(stream.volume.percent(), 70);
    mixer.play(1, stream.clone(), 2).unwrap();
    mixer.advance_ms(100);
    assert_eq!(stream.volume.percent(), 70);
    // Null/stopped handles are harmless, and invalid targets fail before mutation.
    mixer.fade(0, 30, -1, 0).unwrap();
    assert!(mixer.fade(1, 30, -1, 101).is_err());
}

#[test]
fn strict_vm_accepts_fade_and_continues_without_waiting() {
    let mut code = 0x06ecu16.to_le_bytes().to_vec();
    for value in [7u32, 30, (-20i32) as u32, 0x80000000] {
        code.push(4);
        code.extend(value.to_le_bytes());
    }
    let next = code.len();
    code.extend(0x049du16.to_le_bytes());
    code.extend([4, 9, 0, 0, 0]);
    let mut vm = BinaryVm::new("fade.scn", code).unwrap();
    assert!(matches!(
        vm.step().unwrap(),
        Event::Platform {
            request: PlatformRequest::FadeAudioStream {
                handle: 7,
                interval: 30,
                step: -20,
                target: 0x80000000
            },
            ..
        }
    ));
    vm.respond(1).unwrap();
    assert_eq!(vm.location().offset, next);
    assert!(matches!(
        vm.step().unwrap(),
        Event::MouseButtonMapping { value: 9, .. }
    ));
}

#[test]
fn large_clock_jump_matches_small_ticks_and_stops_fading_at_eof() {
    for looping in [false, true] {
        let a = stream(100);
        let b = stream(100);
        let mut batch = Mixer::default();
        let mut ticks = Mixer::default();
        batch
            .play(1, a.clone(), if looping { 2 } else { 0 })
            .unwrap();
        ticks
            .play(1, b.clone(), if looping { 2 } else { 0 })
            .unwrap();
        batch.fade(1, 30, -1, 0).unwrap();
        ticks.fade(1, 30, -1, 0).unwrap();
        batch.advance_ms(2000);
        for _ in 0..2000 {
            ticks.advance_ms(1);
        }
        assert_eq!(a.volume.percent(), b.volume.percent());
        assert_eq!(a.volume.percent(), if looping { 43 } else { 72 });
        assert_eq!(batch.is_playing(1), looping);
    }
}
