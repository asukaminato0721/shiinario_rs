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
