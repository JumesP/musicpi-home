use std::{
    cell::RefCell,
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    rc::Rc,
    sync::mpsc::{self, Receiver, SyncSender},
};

use serde::Deserialize;
use slint::{Image, ModelRc, SharedPixelBuffer, SharedString, Timer, TimerMode, VecModel};

slint::include_modules!();

const BACKGROUNDS_JSON: &str = include_str!("../assets/backgrounds.json");

#[derive(Debug, Deserialize, Clone)]
struct BackgroundDefinition {
    name: String,
    source: String,
    animated: bool,
    #[serde(default)]
    category: Option<String>,
}

#[derive(Debug, Deserialize, serde::Serialize)]
struct DeviceSettings {
    #[serde(default)]
    selected_background: String,
    #[serde(default = "default_overlay_opacity")]
    background_overlay_opacity: i32,
    #[serde(default = "default_video_speed")]
    video_speed_percent: i32,
}

#[derive(Debug, Deserialize, Default)]
struct NowPlaying {
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    album: String,
    #[serde(default = "default_player")]
    player: String,
    #[serde(default)]
    artwork: String,
    #[serde(default)]
    progress: f32,
    #[serde(default)]
    playing: bool,
}

fn default_player() -> String {
    "cadence".to_string()
}

fn default_video_speed() -> i32 {
    100
}

struct BackgroundProcess {
    child: Child,
    frames: Option<Receiver<VideoFrame>>,
}

struct VideoFrame {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

fn open_local_music() {}

fn cadence_is_running() -> Result<bool, String> {
    let status = Command::new("pgrep")
        .args(["-f", "[c]adence"])
        .status()
        .map_err(|error| format!("Failed to check whether Cadence is running: {error}"))?;

    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(code) => Err(format!("Cadence process check exited with status {code}")),
        None => Err("Cadence process check was terminated by a signal".to_string()),
    }
}

fn open_spotify() {
    match cadence_is_running() {
        Ok(true) => {
            println!("Cadence is already running.");
            return;
        }
        Ok(false) => {}
        Err(error) => {
            eprintln!("{error}");
            return;
        }
    }

    match Command::new("/home/pi/cadence/launch-cadence.sh").spawn() {
        Ok(_) => println!("Launching Cadence..."),
        Err(error) => eprintln!("Failed to launch Cadence: {error}"),
    }
}
fn open_video() {}

fn default_overlay_opacity() -> i32 {
    25
}

fn load_backgrounds() -> Result<Vec<BackgroundDefinition>, String> {
    let backgrounds: Vec<BackgroundDefinition> = serde_json::from_str(BACKGROUNDS_JSON)
        .map_err(|error| format!("Invalid backgrounds.json: {error}"))?;

    if backgrounds.is_empty() {
        return Err("backgrounds.json does not contain any backgrounds".to_string());
    }

    if backgrounds
        .iter()
        .any(|background| background.name.trim().is_empty())
    {
        return Err("Every background must have a non-empty name".to_string());
    }

    if backgrounds
        .iter()
        .any(|background| background.animated != is_video(Path::new(&background.source)))
    {
        return Err("Each background's animated value must match its file type".to_string());
    }

    if backgrounds.iter().any(|background| {
        !background.animated
            && background
                .category
                .as_deref()
                .is_none_or(|category| category.trim().is_empty())
    }) {
        return Err("Still backgrounds must have a non-empty category".to_string());
    }

    Ok(backgrounds)
}

fn settings_path() -> Result<PathBuf, String> {
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| "Cannot determine the device config directory".to_string())?;

    Ok(config_dir.join("musicpi-home").join("settings.json"))
}

fn now_playing_path() -> Result<PathBuf, String> {
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| "Cannot determine the device config directory".to_string())?;

    Ok(config_dir.join("musicpi-home").join("now-playing.json"))
}

fn load_now_playing() -> Option<NowPlaying> {
    let path = now_playing_path().ok()?;
    let contents = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&contents) {
        Ok(now_playing) => Some(now_playing),
        Err(error) => {
            eprintln!("Could not parse now-playing.json: {error}");
            None
        }
    }
}

struct ImageLoader {
    cache: HashMap<String, Image>,
    client: reqwest::blocking::Client,
}

impl ImageLoader {
    fn new() -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|error| format!("Could not create image client: {error}"))?;
        Ok(Self {
            cache: HashMap::new(),
            client,
        })
    }

    fn load(&mut self, source: &str) -> Result<Image, String> {
        if let Some(image) = self.cache.get(source) {
            return Ok(image.clone());
        }

        let bytes = if source.starts_with("http://") || source.starts_with("https://") {
            self.client
                .get(source)
                .send()
                .map_err(|error| format!("Could not download image {source}: {error}"))?
                .error_for_status()
                .map_err(|error| format!("Image request failed for {source}: {error}"))?
                .bytes()
                .map_err(|error| format!("Could not read image response {source}: {error}"))?
                .to_vec()
        } else {
            let path = resolve_image_path(source)?;
            std::fs::read(&path)
                .map_err(|error| format!("Could not read image {}: {error}", path.display()))?
        };

        let decoded = image::load_from_memory(&bytes)
            .map_err(|error| format!("Could not decode image {source}: {error}"))?
            .into_rgba8();
        let buffer = SharedPixelBuffer::clone_from_slice(
            decoded.as_raw(),
            decoded.width(),
            decoded.height(),
        );
        let image = Image::from_rgba8(buffer);
        self.cache.insert(source.to_string(), image.clone());
        Ok(image)
    }
}

fn resolve_image_path(source: &str) -> Result<PathBuf, String> {
    let path = Path::new(source);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    asset_path(source)
}

fn update_now_playing(window: &MainWindow, image_loader: &mut ImageLoader) {
    let now_playing = load_now_playing().unwrap_or_default();
    window.set_now_playing_title(SharedString::from(if now_playing.title.is_empty() {
        "Nothing playing"
    } else {
        &now_playing.title
    }));
    window.set_now_playing_artist(SharedString::from(if now_playing.artist.is_empty() {
        "Cadence"
    } else {
        &now_playing.artist
    }));
    window.set_now_playing_album(SharedString::from(now_playing.album));
    window.set_now_playing_player(SharedString::from(now_playing.player));
    let artwork = if now_playing.artwork.is_empty() {
        Image::default()
    } else {
        match image_loader.load(&now_playing.artwork) {
            Ok(image) => image,
            Err(error) => {
                eprintln!("Failed to load artwork: {error}");
                Image::default()
            }
        }
    };
    window.set_now_playing_artwork(artwork);
    window.set_now_playing_progress(now_playing.progress.clamp(0.0, 1.0));
    window.set_now_playing(now_playing.playing);
}

fn load_device_settings() -> DeviceSettings {
    let contents = settings_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok());

    contents
        .and_then(|contents| serde_json::from_str::<DeviceSettings>(&contents).ok())
        .unwrap_or(DeviceSettings {
            selected_background: String::new(),
            background_overlay_opacity: default_overlay_opacity(),
            video_speed_percent: default_video_speed(),
        })
}

fn save_device_settings(name: &str, background_overlay_opacity: i32, video_speed_percent: i32) {
    let path = match settings_path() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("Could not determine settings path: {error}");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            eprintln!(
                "Could not create settings directory {}: {error}",
                parent.display()
            );
            return;
        }
    }
    let contents = match serde_json::to_string_pretty(&DeviceSettings {
        selected_background: name.to_string(),
        background_overlay_opacity,
        video_speed_percent,
    }) {
        Ok(contents) => contents,
        Err(error) => {
            eprintln!("Could not serialize background settings: {error}");
            return;
        }
    };
    if let Err(error) = std::fs::write(&path, contents) {
        eprintln!(
            "Could not save background settings {}: {error}",
            path.display()
        );
    }
}

fn asset_path(source: &str) -> Result<PathBuf, String> {
    let mut assets_directories = Vec::new();
    if let Some(path) = std::env::var_os("MUSICPI_ASSETS") {
        assets_directories.push(PathBuf::from(path));
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(application_directory) = executable.parent() {
            assets_directories.push(application_directory.join("../assets"));
        }
    }
    if let Ok(current_directory) = std::env::current_dir() {
        assets_directories.push(current_directory.join("assets"));
    }
    assets_directories.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets"));

    for assets_dir in assets_directories {
        let canonical_assets_dir = match assets_dir.canonicalize() {
            Ok(path) => path,
            Err(_) => continue,
        };
        let canonical_path = match canonical_assets_dir.join(source).canonicalize() {
            Ok(path) => path,
            Err(_) => continue,
        };
        if canonical_path.starts_with(&canonical_assets_dir) {
            return Ok(canonical_path);
        }
    }

    Err(format!("Cannot access background asset {source}"))
}

fn is_video(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("avi" | "mkv" | "mov" | "mp4" | "webm")
    )
}

fn ffmpeg_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("MUSICPI_FFMPEG") {
        candidates.push(PathBuf::from(path));
    }
    candidates.extend([
        PathBuf::from("ffmpeg"),
        PathBuf::from("/opt/homebrew/bin/ffmpeg"),
        PathBuf::from("/usr/local/bin/ffmpeg"),
        PathBuf::from("/usr/bin/ffmpeg"),
    ]);
    candidates
}

fn stop_background(process: &mut Option<BackgroundProcess>) {
    if let Some(mut process) = process.take() {
        drop(process.frames.take());
        if let Err(error) = process.child.kill() {
            eprintln!("Failed to stop background video: {error}");
        }
        if let Err(error) = process.child.wait() {
            eprintln!("Failed to reap background video process: {error}");
        }
    }
}

fn start_video(path: &Path, video_speed_percent: i32) -> Result<(Child, Receiver<VideoFrame>), String> {
    const WIDTH: u32 = 854;
    const HEIGHT: u32 = 480;
    const FRAME_BYTES: usize = WIDTH as usize * HEIGHT as usize * 4;

    let (sender, receiver): (SyncSender<VideoFrame>, Receiver<VideoFrame>) = mpsc::sync_channel(2);
    let mut last_error = None;

    for candidate in ffmpeg_candidates() {
        let mut command = Command::new(&candidate);
        let child = command
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-stream_loop",
                "-1",
                "-i",
            ])
            .arg(path)
            .args([
                "-vf",
                "scale=854:480,fps=24",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();

        let mut child = match child {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                last_error = Some(error);
                continue;
            }
            Err(error) => return Err(format!("Failed to start FFmpeg: {error}")),
        };

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "FFmpeg did not provide a video output stream".to_string())?;
        std::thread::spawn(move || {
            loop {
                let mut pixels = vec![0; FRAME_BYTES];
                if stdout.read_exact(&mut pixels).is_err() {
                    break;
                }
                if sender
                    .send(VideoFrame {
                        pixels,
                        width: WIDTH,
                        height: HEIGHT,
                    })
                    .is_err()
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs_f64(
                    100.0 / (24.0 * f64::from(video_speed_percent)),
                ));
            }
        });

        return Ok((child, receiver));
    }

    let error = last_error.expect("FFmpeg candidates should not be empty");
    Err(format!(
        "Unable to start FFmpeg: {error}. Install ffmpeg or set MUSICPI_FFMPEG to its executable path."
    ))
}

fn video_preview(path: &Path) -> Result<Image, String> {
    let mut last_error = None;
    for candidate in ffmpeg_candidates() {
        let output = Command::new(&candidate)
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(path)
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale=854:480",
                "-f",
                "image2pipe",
                "-vcodec",
                "png",
                "-",
            ])
            .output();

        match output {
            Ok(output) if output.status.success() && !output.stdout.is_empty() => {
                let decoded = image::load_from_memory(&output.stdout)
                    .map_err(|error| format!("Could not decode video preview: {error}"))?
                    .into_rgba8();
                return Ok(Image::from_rgba8(SharedPixelBuffer::clone_from_slice(
                    decoded.as_raw(),
                    decoded.width(),
                    decoded.height(),
                )));
            }
            Ok(output) => {
                return Err(format!(
                    "FFmpeg could not create a preview for {} (status {})",
                    path.display(),
                    output.status
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                last_error = Some(error);
            }
            Err(error) => return Err(format!("Failed to create video preview: {error}")),
        }
    }

    let error = last_error.expect("FFmpeg candidates should not be empty");
    Err(format!(
        "Unable to create video preview: {error}. Install ffmpeg or set MUSICPI_FFMPEG to its executable path."
    ))
}

fn update_video_frame(window: &MainWindow, process: &Rc<RefCell<Option<BackgroundProcess>>>) {
    let frame = process
        .borrow()
        .as_ref()
        .and_then(|process| process.frames.as_ref())
        .and_then(|receiver| receiver.try_iter().last());

    if let Some(frame) = frame {
        let buffer = SharedPixelBuffer::clone_from_slice(&frame.pixels, frame.width, frame.height);
        window.set_background_image(Image::from_rgba8(buffer));
    }
}

fn apply_background(
    window: &MainWindow,
    background: &BackgroundDefinition,
    process: &Rc<RefCell<Option<BackgroundProcess>>>,
    video_speed_percent: i32,
) {
    let path = match asset_path(&background.source) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return;
        }
    };

    if is_video(&path) {
        stop_background(&mut process.borrow_mut());
        match start_video(&path, video_speed_percent) {
            Ok((child, frames)) => {
                *process.borrow_mut() = Some(BackgroundProcess {
                    child,
                    frames: Some(frames),
                });
            }
            Err(error) => {
                eprintln!("Unable to play video background {}: {error}", path.display());
            }
        }
    } else {
        stop_background(&mut process.borrow_mut());
        match Image::load_from_path(&path) {
            Ok(image) => window.set_background_image(image),
            Err(error) => eprintln!(
                "Failed to load background image {}: {error}",
                path.display()
            ),
        }
    }
}

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
    let phrase_count = 15u32;
    let phrase_index = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| (duration.as_nanos() % u128::from(phrase_count)) as i32)
        .unwrap_or(0);
    window.set_subtitle_index(phrase_index);
    let backgrounds = load_backgrounds().map_err(slint::PlatformError::from)?;
    let background_by_name = backgrounds
        .iter()
        .cloned()
        .map(|background| (background.name.clone(), background))
        .collect::<HashMap<_, _>>();
    let background_process = Rc::new(RefCell::new(None));
    let image_loader = Rc::new(RefCell::new(
        ImageLoader::new().map_err(slint::PlatformError::from)?,
    ));
    let background_names = backgrounds
        .iter()
        .map(|background| SharedString::from(background.name.as_str()))
        .collect::<Vec<_>>();
    let background_entries = backgrounds
        .iter()
        .map(|background| {
            let path = asset_path(&background.source).ok();
            let preview = path
                .as_ref()
                .and_then(|path| {
                    if is_video(path) {
                        video_preview(path).ok()
                    } else {
                        Image::load_from_path(path).ok()
                    }
                })
                .unwrap_or_default();

            BackgroundEntry {
                name: SharedString::from(background.name.as_str()),
                preview,
                is_video: background.animated,
                category: SharedString::from(background.category.as_deref().unwrap_or("")),
            }
        })
        .collect::<Vec<_>>();
    let animated_entries = background_entries
        .iter()
        .filter(|entry| entry.is_video)
        .cloned()
        .collect::<Vec<_>>();
    let still_entries = background_entries
        .iter()
        .filter(|entry| !entry.is_video)
        .cloned()
        .collect::<Vec<_>>();
    let mut still_categories = Vec::new();
    for entry in &still_entries {
        if !still_categories
            .iter()
            .any(|category: &BackgroundCategory| category.name == entry.category)
        {
            let entries = still_entries
                .iter()
                .filter(|candidate| candidate.category == entry.category)
                .cloned()
                .collect::<Vec<_>>();
            still_categories.push(BackgroundCategory {
                name: entry.category.clone(),
                entries: ModelRc::new(VecModel::from(entries)),
            });
        }
    }
    let device_settings = load_device_settings();
    let selected_name = (!device_settings.selected_background.is_empty())
        .then_some(device_settings.selected_background.clone())
        .filter(|name| background_by_name.contains_key(name))
        .unwrap_or_else(|| backgrounds[0].name.clone());

    window.set_background_options(ModelRc::new(VecModel::from(background_names)));
    window.set_background_entries(ModelRc::new(VecModel::from(background_entries)));
    window.set_animated_background_entries(ModelRc::new(VecModel::from(animated_entries)));
    window.set_still_background_entries(ModelRc::new(VecModel::from(still_entries.clone())));
    window.set_still_categories(ModelRc::new(VecModel::from(still_categories)));
    window.set_background_name(SharedString::from(selected_name.as_str()));
    window.set_background_overlay_opacity(device_settings.background_overlay_opacity.clamp(0, 100));
    window.set_video_speed_percent(device_settings.video_speed_percent.clamp(25, 200));
    update_now_playing(&window, &mut image_loader.borrow_mut());
    apply_background(
        &window,
        &background_by_name[&selected_name],
        &background_process,
        device_settings.video_speed_percent.clamp(25, 200),
    );

    window.on_open_local_music(open_local_music);
    window.on_open_spotify(open_spotify);
    window.on_open_video(open_video);
    let background_by_name_for_callback = background_by_name.clone();
    let background_process_for_callback = background_process.clone();
    let window_weak = window.as_weak();
    window.on_background_selected(move |name| {
        let name = name.to_string();
        if let Some(background) = background_by_name_for_callback.get(&name) {
            if let Some(window) = window_weak.upgrade() {
                apply_background(
                    &window,
                    background,
                    &background_process_for_callback,
                    window.get_video_speed_percent().clamp(25, 200),
                );
                window.set_background_name(SharedString::from(name.as_str()));
                save_device_settings(
                    &name,
                    window.get_background_overlay_opacity(),
                    window.get_video_speed_percent(),
                );
            }
        } else {
            eprintln!("Unknown background selected: {name}");
        }
    });
    let settings_window_weak = window.as_weak();
    window.on_background_overlay_opacity_changed(move |opacity| {
        if let Some(window) = settings_window_weak.upgrade() {
            save_device_settings(
                &window.get_background_name().to_string(),
                opacity.clamp(0, 100),
                window.get_video_speed_percent(),
            );
        }
    });
    let speed_window_weak = window.as_weak();
    let speed_background_by_name = background_by_name.clone();
    let speed_process = background_process.clone();
    window.on_video_speed_percent_changed(move |speed| {
        if let Some(window) = speed_window_weak.upgrade() {
            let speed = speed.clamp(25, 200);
            window.set_video_speed_percent(speed);
            let background_name = window.get_background_name().to_string();
            if let Some(background) = speed_background_by_name.get(&background_name) {
                apply_background(&window, background, &speed_process, speed);
            }
            save_device_settings(
                &background_name,
                window.get_background_overlay_opacity(),
                speed,
            );
        }
    });

    let now_playing_window = window.as_weak();
    let now_playing_image_loader = image_loader.clone();
    let now_playing_timer = Timer::default();
    now_playing_timer.start(
        TimerMode::Repeated,
        std::time::Duration::from_secs(1),
        move || {
            if let Some(window) = now_playing_window.upgrade() {
                update_now_playing(&window, &mut now_playing_image_loader.borrow_mut());
            }
        },
    );
    let video_window = window.as_weak();
    let video_process = background_process.clone();
    let video_timer = Timer::default();
    video_timer.start(
        TimerMode::Repeated,
        std::time::Duration::from_millis(42),
        move || {
            if let Some(window) = video_window.upgrade() {
                update_video_frame(&window, &video_process);
            }
        },
    );

    let result = window.run();
    stop_background(&mut background_process.borrow_mut());
    result
}
