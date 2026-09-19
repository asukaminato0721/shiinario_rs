use shiinario_assets::{
    Archive,
    project::{Cache, Project},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static ID: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "shiinario-test-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        // Filename-only catalog detection does not execute or inspect this file.
        std::fs::write(p.join("RANDL_.exe"), []).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
const FIXTURE: &[u8] = include_bytes!("fixtures/minimal.war");
#[test]
fn configuration_versions_and_wana_catalog_mapping() {
    use shiinario_assets::{profile::Catalog, project::Config};
    use shiinario_core::EngineVersion;
    for (version, expected) in [
        ("2.36", EngineVersion::V2_36),
        ("2.47", EngineVersion::V2_47),
    ] {
        let text = format!(
            "[椎名里緒 v{version}]\r\nWindowWidth=800\r\nWindowHeight=600\r\nArc=fixture.war\r\nScn=start.scn\r\n"
        );
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode(&text);
        let config = Config::parse("game.ini".into(), &bytes).unwrap();
        assert_eq!(config.version, expected);
    }
    assert!(EngineVersion::from_config("椎名里緒 v2.35").is_none());
    assert!(EngineVersion::from_scheme(2350).is_none());
    let t = Temp::new();
    std::fs::rename(t.0.join("RANDL_.exe"), t.0.join("WANA.EXE")).unwrap();
    let catalog = Catalog::builtin().unwrap();
    std::fs::write(t.0.join("SETUP.EXE"), b"not the game").unwrap();
    assert_eq!(
        catalog.game_executables(&t.0).unwrap(),
        [t.0.join("WANA.EXE")]
    );
    assert!(Arc::ptr_eq(
        &catalog.for_directory(&t.0).unwrap(),
        &catalog
            .profile("Wana ~Hakudaku Mamire no Houkago~")
            .unwrap()
    ));
}

#[test]
fn reference_archive_and_malformed_headers() {
    let t = Temp::new();
    let p = t.0.join("fixture.war");
    std::fs::write(&p, FIXTURE).unwrap();
    let a = Archive::open(&p).unwrap();
    assert_eq!(a.entries.len(), 1);
    let e = a.find("FIXTURE.TXT").unwrap();
    assert_eq!(a.read(e).unwrap(), b"synthetic asset fixture\n");
    for n in [0, 1, 8, 11, 12, 51, 55] {
        std::fs::write(&p, &FIXTURE[..n]).unwrap();
        assert!(Archive::open(&p).is_err(), "accepted truncation at {n}");
    }
    let mut bad = FIXTURE.to_vec();
    bad[8..12].copy_from_slice(&(0xf182ad82u32 ^ u32::MAX).to_le_bytes());
    std::fs::write(&p, &bad).unwrap();
    assert!(Archive::open(&p).is_err());
}
#[test]
fn project_lookup_and_cache_budget() {
    let t = Temp::new();
    std::fs::write(t.0.join("FiXtUrE.WAR"), FIXTURE).unwrap();
    let (ini,_,_)=encoding_rs::SHIFT_JIS.encode("[椎名里緒 v2.47]\r\nWindowWidth=800\r\nWindowHeight=600\r\nArc=fixture.war\r\nScn=fixture.txt\r\n");
    std::fs::write(t.0.join("GAME.INI"), ini).unwrap();
    std::fs::write(t.0.join("a"), b"12").unwrap();
    std::fs::write(t.0.join("b"), b"345").unwrap();
    std::fs::write(t.0.join("c"), b"6789").unwrap();
    let project = Project::open(&t.0).unwrap();
    assert_eq!(
        project.read("D\\FIXTURE.txt").unwrap(),
        b"synthetic asset fixture\n"
    );
    assert!(project.read("..\\fixture.txt").is_err());
    assert!(project.read_with_archives("fixture.txt", &[]).is_err());
    assert_eq!(
        project
            .read_with_archives(
                "D\\FIXTURE.TXT",
                &["missing.war".into(), "FIXTURE.war".into()]
            )
            .unwrap(),
        b"synthetic asset fixture\n"
    );
    assert!(
        project
            .read_with_archives("GAME.INI", &["fixture.war".into()])
            .is_err()
    );
    assert!(!project.loose_path_exists("fixture.txt").unwrap());
    assert!(!project.loose_path_exists("D\\GAME.INI").unwrap());
    assert!(project.loose_path_exists("game.ini").unwrap());
    assert!(project.loose_path_exists("..\\game.ini").is_err());
    std::fs::create_dir(t.0.join("Saves")).unwrap();
    std::fs::write(t.0.join("Saves/Slot.DAT"), b"state").unwrap();
    assert!(project.loose_path_exists("SAVES\\slot.dat").unwrap());
    assert!(project.loose_path_exists("saves").unwrap());
    let mut cache = Cache::new(5);
    let a = cache.read(&project, "A").unwrap();
    assert!(Arc::ptr_eq(&a, &cache.read(&project, "a").unwrap()));
    cache.read(&project, "b").unwrap();
    assert_eq!(cache.resident_bytes(), 5);
    cache.read(&project, "c").unwrap();
    assert_eq!(cache.resident_bytes(), 4);
    assert!(!Arc::ptr_eq(&a, &cache.read(&project, "a").unwrap()));
    let before = cache.resident_bytes();
    cache.read(&project, "fixture.txt").unwrap();
    assert_eq!(cache.resident_bytes(), before);
}
