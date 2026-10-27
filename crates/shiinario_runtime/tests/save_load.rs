use shiinario_assets::project::Project;
use shiinario_runtime::resources::Resources;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Game(PathBuf);
impl Game {
    fn new() -> Self {
        static ID: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "shiinario-save-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("RANDL_.exe"), []).unwrap();
        std::fs::write(
            root.join("fixture.war"),
            include_bytes!("../../shiinario_assets/tests/fixtures/minimal.war"),
        )
        .unwrap();
        let (ini, _, _) = encoding_rs::SHIFT_JIS.encode("[椎名里緒 v2.47]\r\nWindowWidth=800\r\nWindowHeight=600\r\nArc=fixture.war\r\nScn=fixture.txt\r\n");
        std::fs::write(root.join("GAME.INI"), ini).unwrap();
        Self(root)
    }
    fn project(&self) -> Project {
        Project::open(&self.0).unwrap()
    }
}
impl Drop for Game {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn audio_release_stops_loaded_sounds_and_allows_reload_in_a_session() {
    use shiinario_scenario::{Event, PlatformRequest, SoundCommand};
    let game = Game::new();
    std::fs::write(
        game.0.join("sound.pad"),
        include_bytes!("../../shiinario_assets/tests/fixtures/garbro/pad-1.pad"),
    )
    .unwrap();
    let mut code = vec![0xa5, 6]; // Release before initialization is valid.
    code.extend(b"\xc9\0\x10sound.pad\0\x0c\0\0");
    for _ in 0..2 {
        code.extend([0xb1, 6, 4, 0, 0, 0, 0, 12, 0, 0]);
        code.extend([0xa7, 6, 4, 0, 0, 0, 0, 4, 1, 0, 0, 0]);
        code.extend([0xaf, 6, 4, 0, 0, 0, 0, 12, 1, 0]);
        code.extend([0xa5, 6, 0xa5, 6]);
        code.extend([0xaf, 6, 4, 0, 0, 0, 0, 12, 1, 0]);
    }
    code.extend([0, 0, 4, 0, 0, 0, 0]);
    std::fs::write(game.0.join("audio.scn"), code).unwrap();
    let mut status_pending = false;
    let mut statuses = Vec::new();
    shiinario_runtime::trace_with_platform(&game.project(), "audio.scn", 1000, true, |event| {
        match event {
            Event::Platform {
                request:
                    PlatformRequest::Sound {
                        command: SoundCommand::Status,
                        ..
                    },
                ..
            } => status_pending = true,
            Event::PlatformReply { value, .. } if status_pending => {
                statuses.push(*value);
                status_pending = false;
            }
            _ => {}
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(statuses, [5, u32::MAX, 5, u32::MAX]);
}

#[test]
fn deletion_removes_loose_files_and_reveals_registered_archive_entries() {
    for simulated in [false, true] {
        let game = Game::new();
        let project = game.project();
        project
            .write_file("fixture.txt", b"loose override")
            .unwrap();
        let mut resources = Resources::for_project(&project).unwrap();
        assert!(
            resources
                .delete_file(&project, "FIXTURE.TXT", simulated)
                .unwrap()
        );
        assert!(!resources.file_exists(&project, "fixture.txt").unwrap());
        assert!(
            resources
                .read_file_contents(&project, "fixture.txt")
                .is_err()
        );
        assert!(
            resources
                .open_file(&project, "fixture.txt", 0x80000000)
                .is_err()
        );
        assert_eq!(
            resources.read_asset(&project, "fixture.txt").unwrap(),
            b"synthetic asset fixture\n"
        );
        assert_eq!(
            resources.asset_sizes(&project, "fixture.txt").unwrap(),
            [24, 40]
        );
        assert!(
            !resources
                .delete_file(&project, "fixture.txt", simulated)
                .unwrap()
        );
        assert_eq!(game.0.join("fixture.txt").exists(), simulated);
        if simulated {
            assert_eq!(
                std::fs::read(game.0.join("fixture.txt")).unwrap(),
                b"loose override"
            );
        }
        resources
            .write_file(&project, "fixture.txt", b"new".to_vec(), simulated)
            .unwrap();
        assert!(resources.file_exists(&project, "fixture.txt").unwrap());
        assert_eq!(
            resources
                .read_file_contents(&project, "fixture.txt")
                .unwrap(),
            b"new"
        );
        assert_eq!(
            resources.asset_sizes(&project, "fixture.txt").unwrap(),
            [0, 3]
        );
        assert!(
            resources
                .delete_file(&project, "fixture.txt", simulated)
                .unwrap()
        );
    }
}

#[test]
fn deletion_keeps_directories_and_rejects_paths_outside_the_project() {
    for simulated in [false, true] {
        let game = Game::new();
        let project = game.project();
        let mut resources = Resources::default();
        std::fs::create_dir(game.0.join("Save")).unwrap();
        std::fs::write(game.0.join("Save/Slot.DAT"), b"state").unwrap();
        assert!(!resources.delete_file(&project, "SAVE", simulated).unwrap());
        assert!(
            !resources
                .delete_file(&project, "missing.dat", simulated)
                .unwrap()
        );
        for name in ["", ".", ".\\", "/"] {
            assert!(!resources.delete_file(&project, name, simulated).unwrap());
        }
        for name in ["../escape", "Save/../../escape", "/escape", "C:\\escape"] {
            assert!(resources.delete_file(&project, name, simulated).is_err());
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("Save/Slot.DAT", game.0.join("link.dat")).unwrap();
            std::os::unix::fs::symlink("Save", game.0.join("link-dir")).unwrap();
            for name in ["link.dat", "link-dir/Slot.DAT"] {
                assert!(resources.delete_file(&project, name, simulated).is_err());
            }
        }
        assert_eq!(
            std::fs::read(game.0.join("Save/Slot.DAT")).unwrap(),
            b"state"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let slot = game.0.join("Save/Slot.DAT");
            std::fs::set_permissions(&slot, std::fs::Permissions::from_mode(0o444)).unwrap();
            assert!(
                !resources
                    .delete_file(&project, "save/slot.dat", simulated)
                    .unwrap()
            );
            assert_eq!(std::fs::read(&slot).unwrap(), b"state");
            std::fs::set_permissions(&slot, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert!(
            resources
                .delete_file(&project, "save/slot.dat", simulated)
                .unwrap()
        );
        assert!(game.0.join("Save").is_dir());
        assert_eq!(game.0.join("Save/Slot.DAT").exists(), simulated);
        resources
            .write_file(&project, "new.dat", vec![7], simulated)
            .unwrap();
        assert!(
            resources
                .delete_file(&project, "NEW.DAT", simulated)
                .unwrap()
        );
        assert!(resources.read_asset(&project, "new.dat").is_err());
        assert!(resources.asset_sizes(&project, "new.dat").is_err());
    }
}

#[test]
fn deletion_instruction_reaches_the_runtime_without_changing_disk_during_trace() {
    use shiinario_scenario::{Event, PlatformRequest};
    let game = Game::new();
    std::fs::create_dir(game.0.join("Save")).unwrap();
    std::fs::write(game.0.join("Save/Slot.DAT"), b"state").unwrap();
    let mut code = b"\x23\x01\x10save\0\x10slot.dat\0".to_vec();
    code.extend(b"\x0e\x01\x10SAVE\\SLOT.DAT\0\x0c\0\0\x9d\x04\x0c\0\0");
    code.extend(b"\0\0\x04\0\0\0\0");
    std::fs::write(game.0.join("delete.scn"), code).unwrap();
    let project = game.project();
    let mut deleted = false;
    let mut exists = None;
    shiinario_runtime::trace_with_platform(&project, "delete.scn", 100, true, |event| {
        if let Event::Platform {
            request: PlatformRequest::DeleteFile { path },
            ..
        } = event
        {
            assert_eq!(path, "save\\slot.dat");
            deleted = true;
        }
        if let Event::MouseButtonMapping { value, .. } = event {
            exists = Some(*value);
        }
        Ok(())
    })
    .unwrap();
    assert!(deleted);
    assert_eq!(exists, Some(0));
    assert_eq!(
        std::fs::read(game.0.join("Save/Slot.DAT")).unwrap(),
        b"state"
    );
}

#[test]
fn startup_archive_is_registered_when_resources_are_created() {
    let game = Game::new();
    let project = game.project();
    let resources = Resources::for_project(&project).unwrap();
    assert_eq!(
        resources.read_asset(&project, "fixture.txt").unwrap(),
        b"synthetic asset fixture\n"
    );
}

#[test]
fn removed_archives_stop_resolving_assets_and_can_be_registered_again() {
    let game = Game::new();
    let project = game.project();
    let mut resources = Resources::for_project(&project).unwrap();
    resources.unregister_archive("MISSING.WAR");
    assert!(resources.read_asset(&project, "fixture.txt").is_ok());
    resources.unregister_archive("FIXTURE.WAR");
    assert!(resources.read_asset(&project, "fixture.txt").is_err());
    assert!(resources.asset_sizes(&project, "fixture.txt").is_err());
    resources.register_archive("fixture.war");
    assert_eq!(
        resources.read_asset(&project, "fixture.txt").unwrap(),
        b"synthetic asset fixture\n"
    );
    resources.unregister_archive("fixture.war");
    project
        .write_file("fixture.txt", b"loose override")
        .unwrap();
    assert_eq!(
        resources.read_asset(&project, "fixture.txt").unwrap(),
        b"loose override"
    );
}

#[test]
fn scenario_archive_removal_reaches_the_shared_runtime() {
    use shiinario_scenario::Event;
    let game = Game::new();
    for restore in [false, true] {
        let mut code = b"\xdf\0\x10FIXTURE.WAR\0".to_vec();
        if restore {
            code.extend(b"\xdd\0\x10fixture.war\0");
        }
        // AssetSizes fixture.txt -> two variables, then end of script.
        code.extend(b"\x58\x01\x10fixture.txt\0\x0c\0\0\x0c\x01\0\0\0\x04\x01\0\0\0");
        std::fs::write(game.0.join("switch.scn"), code).unwrap();
        let project = game.project();
        let mut removed = false;
        let mut sizes = false;
        let result = shiinario_runtime::trace_with_platform(
            &project,
            "switch.scn",
            30,
            true,
            |event| {
                removed |= matches!(event, Event::ArchiveSearchPathRemoved { name, .. } if name == "fixture.war");
                sizes |= matches!(event, Event::AssetSizesReply { .. });
                Ok(())
            },
        );
        assert!(removed);
        assert_eq!(sizes, restore);
        if restore {
            result.unwrap();
        } else {
            assert!(
                format!("{:#}", result.unwrap_err())
                    .contains("asset not found in registered archives")
            );
        }
    }
}

#[test]
fn asset_reads_reload_saves_and_respect_file_precedence() {
    let game = Game::new();
    let project = game.project();
    let mut resources = Resources::default();
    // Archive registration still controls fallback; Project's global index
    // must not make unregistered archive entries visible.
    assert!(resources.read_asset(&project, "fixture.txt").is_err());
    resources.register_archive("fixture.war");
    assert_eq!(
        resources.read_asset(&project, "fixture.txt").unwrap(),
        b"synthetic asset fixture\n"
    );
    project.write_file("fixture.txt", b"disk override").unwrap();
    assert_eq!(
        resources.read_asset(&project, "FIXTURE.TXT").unwrap(),
        b"disk override"
    );
    resources
        .write_file(&project, "fixture.txt", b"replay override".to_vec(), true)
        .unwrap();
    assert_eq!(
        resources.read_asset(&project, "Fixture.txt").unwrap(),
        b"replay override"
    );
    assert_eq!(project.read("fixture.txt").unwrap(), b"disk override");

    resources.create_directory(&project, "Save", false).unwrap();
    let save: Vec<u8> = (0..8192).map(|i| (i * 37) as u8).collect();
    resources
        .write_file(&project, "save/System.bin", save.clone(), false)
        .unwrap();
    // The file was created after opening the project and must be visible now
    // as well as after restarting the runtime.
    assert_eq!(
        resources.read_asset(&project, "SAVE\\system.BIN").unwrap(),
        save
    );
    let reopened = game.project();
    let mut restarted = Resources::default();
    assert_eq!(
        restarted.read_asset(&reopened, "save\\system.bin").unwrap(),
        save
    );
    restarted
        .write_file(&reopened, "SAVE/system.bin", vec![7; 12], true)
        .unwrap();
    assert_eq!(
        restarted.asset_sizes(&reopened, "save/system.bin").unwrap(),
        [0, 12]
    );
    assert_eq!(
        restarted.read_asset(&reopened, "save/system.bin").unwrap(),
        [7; 12]
    );
    assert_eq!(project.read("save/system.bin").unwrap(), save);
    assert!(restarted.read_asset(&reopened, "save/missing.bin").is_err());
    assert!(restarted.read_asset(&reopened, "../system.bin").is_err());
}

#[test]
fn directory_creation_matches_native_and_simulated_results() {
    for simulated in [false, true] {
        let game = Game::new();
        let project = game.project();
        let mut resources = Resources::default();
        assert!(
            !resources
                .create_directory(&project, "missing/child", simulated)
                .unwrap()
        );
        assert!(
            resources
                .create_directory(&project, "Save", simulated)
                .unwrap()
        );
        assert!(
            !resources
                .create_directory(&project, "SAVE", simulated)
                .unwrap()
        );
        assert!(resources.file_exists(&project, "save").unwrap());
        assert!(
            resources
                .create_directory(&project, "SAVE/slots", simulated)
                .unwrap()
        );
        assert_eq!(project.directory_exists("save/slots").unwrap(), !simulated);
        assert!(
            !resources
                .create_directory(&project, "fixture.war", simulated)
                .unwrap()
        );
        assert!(
            resources
                .create_directory(&project, "../escape", simulated)
                .is_err()
        );
        resources
            .write_file(&project, "save/slots/slot.dat", vec![1, 2], simulated)
            .unwrap();
        assert!(
            resources
                .file_exists(&project, "SAVE/SLOTS/SLOT.DAT")
                .unwrap()
        );
    }
}

#[test]
fn native_slots_survive_restart_and_case_insensitive_overwrite() {
    let game = Game::new();
    let project = game.project();
    let mut resources = Resources::default();
    let slot: Vec<u8> = (0..43132).map(|i| (i * 37) as u8).collect();
    resources
        .write_file(&project, "Save000.DAT", slot.clone(), false)
        .unwrap();
    assert!(resources.file_exists(&project, "SAVE000.dat").unwrap());
    assert_eq!(
        resources.asset_sizes(&project, "save000.dat").unwrap(),
        [0, 43132]
    );
    assert_eq!(project.read("save000.dat").unwrap(), slot);
    drop(resources);
    let mut restarted = Resources::default();
    let reopened = game.project();
    let metadata = restarted
        .open_file(&reopened, "SAVE000.DAT", 0x80000000)
        .unwrap();
    assert_eq!(&metadata[..3], &[0, 43132, 0]);
    let handle = metadata[3];
    assert_eq!(restarted.read_file(handle, 50000).unwrap(), slot);
    assert!(restarted.read_file(handle, 4).unwrap().is_empty());
    assert!(
        restarted
            .write_file_handle(&reopened, handle, vec![1], false)
            .is_err()
    );
    restarted.close_file(handle).unwrap();
    assert!(restarted.read_file(handle, 1).is_err());
    restarted
        .write_file(&reopened, "SAVE000.DAT", vec![9; 4], false)
        .unwrap();
    assert_eq!(project.read("save000.dat").unwrap(), [9; 4]);
    assert!(!game.0.join("SAVE000.DAT").exists());
    assert!(!std::fs::read_dir(&game.0).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}

#[test]
fn seek_partial_reads_sparse_writes_and_simulation() {
    let game = Game::new();
    let project = game.project();
    project.write_file("save.dat", &[1, 2, 3, 4]).unwrap();
    let mut resources = Resources::default();
    let handle = resources
        .open_file(&project, "save.dat", 0xc0000000)
        .unwrap()[3];
    assert_eq!(resources.seek_file(handle, -2, 2).unwrap(), 2);
    assert_eq!(resources.read_file(handle, 20).unwrap(), [3, 4]);
    assert!(resources.seek_file(handle, -10, 1).is_err());
    assert_eq!(resources.seek_file(handle, 2, 1).unwrap(), 6);
    assert_eq!(
        resources
            .write_file_handle(&project, handle, vec![], true)
            .unwrap(),
        0
    );
    assert_eq!(resources.asset_sizes(&project, "save.dat").unwrap(), [0, 4]);
    assert_eq!(
        resources
            .write_file_handle(&project, handle, vec![8, 9], true)
            .unwrap(),
        2
    );
    assert_eq!(resources.asset_sizes(&project, "save.dat").unwrap(), [0, 8]);
    resources.close_file(handle).unwrap();
    let handle = resources
        .open_file(&project, "SAVE.DAT", 0x80000000)
        .unwrap()[3];
    assert_eq!(
        resources.read_file(handle, 20).unwrap(),
        [1, 2, 3, 4, 0, 0, 8, 9]
    );
    assert_eq!(project.read("save.dat").unwrap(), [1, 2, 3, 4]);
    resources
        .write_file(&project, "new.dat", vec![7], true)
        .unwrap();
    assert!(resources.file_exists(&project, "new.dat").unwrap());
    assert!(!project.loose_path_exists("new.dat").unwrap());
}

#[test]
fn invalid_write_paths_preserve_existing_slots() {
    let game = Game::new();
    let project = game.project();
    project.write_file("save.dat", &[1, 2]).unwrap();
    for name in [
        "../save.dat",
        "/save.dat",
        "C:\\save.dat",
        "missing/save.dat",
        "fixture.war/child",
    ] {
        assert!(project.write_file(name, &[3]).is_err(), "{name}");
    }
    std::fs::create_dir(game.0.join("directory")).unwrap();
    assert!(project.write_file("directory", &[3]).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(game.0.join("save.dat"), game.0.join("link.dat")).unwrap();
        assert!(project.write_file("link.dat", &[3]).is_err());
    }
    assert_eq!(project.read("save.dat").unwrap(), [1, 2]);
}
