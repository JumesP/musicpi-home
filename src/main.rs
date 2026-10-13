use std::process::Command;

slint::include_modules!();

fn open_local_music() {}

fn open_spotify() {
    match Command::new("/home/pi/cadence/launch-cadence.sh").spawn() {
        Ok(_) => println!("Launching Cadence..."),
        Err(error) => eprintln!("Failed to launch Cadence: {error}"),
    }
}
fn open_video() {}

fn main() -> Result<(), slint::PlatformError> {
    let backend = i_slint_backend_winit::Backend::builder()
        .with_renderer_name("software")
        .with_window_attributes_hook(|attributes| {
            attributes
                .with_transparent(true)
                .with_decorations(false)
                .with_maximized(true)
        })
        .build()?;

    slint::platform::set_platform(Box::new(backend))
        .map_err(|error| slint::PlatformError::from(error.to_string()))?;

    let window = MainWindow::new()?;

    window.on_open_local_music(open_local_music);
    window.on_open_spotify(open_spotify);
    window.on_open_video(open_video);

    window.run()
}
