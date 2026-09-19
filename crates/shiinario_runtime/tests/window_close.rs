use shiinario_assets::project::Project;
use shiinario_scenario::{Event, PlatformRequest};

#[test]
fn posted_close_ends_session_through_the_shared_native_and_trace_path() {
    let root = std::env::temp_dir().join(format!("shiinario-close-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let result = (|| -> anyhow::Result<()> {
        std::fs::write(root.join("RANDL_.exe"), [])?;
        std::fs::write(
            root.join("fixture.war"),
            include_bytes!("../../shiinario_assets/tests/fixtures/minimal.war"),
        )?;
        let (ini, _, _) = encoding_rs::SHIFT_JIS.encode("[椎名里緒 v2.47]\r\nWindowWidth=800\r\nWindowHeight=600\r\nArc=fixture.war\r\nScn=close.scn\r\n");
        std::fs::write(root.join("GAME.INI"), ini)?;
        // The exact failing instruction, followed by message mode and a pump.
        let mut code = vec![0xbc, 0x07];
        for value in [0u32, 0x10, 0, 0] {
            code.push(4);
            code.extend(value.to_le_bytes());
        }
        code.extend([0x33, 0, 0x34, 0, 0xff, 0xff]);
        std::fs::write(root.join("close.scn"), code)?;
        let project = Project::open(&root)?;
        let mut posted = false;
        let mut ended = false;
        shiinario_runtime::trace_with_platform(&project, "close.scn", 30, true, |event| {
            if matches!(
                event,
                Event::Platform {
                    request: PlatformRequest::PostWindowMessage { message: 0x10, .. },
                    ..
                }
            ) {
                posted = true;
            }
            assert!(!matches!(event, Event::CompatibilitySkip { .. }));
            if *event == Event::End {
                ended = true;
            }
            Ok(())
        })?;
        assert!(posted && ended);
        Ok(())
    })();
    std::fs::remove_dir_all(root).unwrap();
    result.unwrap();
}
