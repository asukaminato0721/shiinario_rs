//! NativeActivity entry point; the launcher stores the selected original directory.
use anyhow::{Context, Result};
use winit::{
    event_loop::EventLoop,
    platform::android::{EventLoopBuilderExtAndroid, activity::AndroidApp},
};

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("shiinario"),
    );
    let data = app.internal_data_path();
    let run = || -> Result<()> {
        let data = data
            .as_ref()
            .context("Android internal storage unavailable")?;
        let root = std::fs::read_to_string(data.join("game-path.txt"))?;
        let project = crate::open(root.trim())?;
        let event_loop = EventLoop::builder().with_android_app(app).build()?;
        crate::native::run_with_event_loop(&project, Default::default(), event_loop)
    };
    if let Err(error) = run() {
        log::error!("{error:#}");
        if let Some(data) = data {
            let _ = std::fs::write(data.join("last-error.txt"), format!("{error:#}"));
        }
    }
}
