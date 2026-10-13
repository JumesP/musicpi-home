use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Child, Command},
    rc::Rc,
};

use serde::Deserialize;
use slint::{Image, ModelRc, SharedPixelBuffer, SharedString, Timer, TimerMode, VecModel};

#[cfg(target_os = "linux")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
#[cfg(target_os = "linux")]
use slint::winit_030::WinitWindowAccessor;

#[cfg(target_os = "linux")]
use x11rb::connection::Connection;

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

struct BackgroundProcess {
    child: Child,
}

fn open_local_music() {}

fn open_spotify() {
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
            && !matches!(
                background.category.as_deref(),
                Some("pokemon" | "lifestyle" | "anime")
            )
    }) {
        return Err(
            "Still backgrounds must have a pokemon, lifestyle, or anime category".to_string(),
        );
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
        })
}

fn save_device_settings(name: &str, background_overlay_opacity: i32) {
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

fn mpv_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("MUSICPI_MPV") {
        candidates.push(PathBuf::from(path));
    }
    candidates.extend([
        PathBuf::from("mpv"),
        PathBuf::from("/opt/homebrew/bin/mpv"),
        PathBuf::from("/usr/local/bin/mpv"),
        PathBuf::from("/usr/bin/mpv"),
    ]);
    candidates
}

fn stop_background(process: &mut Option<BackgroundProcess>) {
    if let Some(mut process) = process.take() {
        if let Err(error) = process.child.kill() {
            eprintln!("Failed to stop background video: {error}");
        }
        if let Err(error) = process.child.wait() {
            eprintln!("Failed to reap background video process: {error}");
        }
    }
}

#[cfg(target_os = "linux")]
fn x11_root_window(window: &MainWindow) -> Result<u32, String> {
    let parent = window
        .window()
        .with_winit_window(|winit_window| {
            let handle = winit_window
                .window_handle()
                .map_err(|error| format!("Cannot get app window handle: {error}"))?;

            match handle.as_raw() {
                RawWindowHandle::Xlib(handle) => Ok(handle.window as u32),
                RawWindowHandle::Xcb(handle) => Ok(handle.window.get()),
                _ => Err("MPV background embedding requires an X11 window".to_string()),
            }
        })
        .ok_or_else(|| "The app window is not ready for video embedding".to_string())??;
    let (connection, screen_number) =
        x11rb::connect(None).map_err(|error| format!("Cannot connect to X11: {error}"))?;
    let screen = &connection.setup().roots[screen_number];
    let _ = parent;
    Ok(screen.root)
}

fn apply_background(
    window: &MainWindow,
    background: &BackgroundDefinition,
    process: &Rc<RefCell<Option<BackgroundProcess>>>,
    mpv_unavailable_reported: &Rc<RefCell<bool>>,
) {
    let path = match asset_path(&background.source) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return;
        }
    };

    stop_background(&mut process.borrow_mut());

    if is_video(&path) {
        window.set_background_image(Image::default());
        #[cfg(target_os = "linux")]
        let window_id = match x11_root_window(window) {
            Ok(window_id) => window_id.to_string(),
            Err(error) => {
                eprintln!("Unable to embed the video background: {error}");
                return;
            }
        };
        #[cfg(not(target_os = "linux"))]
        {
            eprintln!("Video backgrounds are only supported as embedded backgrounds on Linux/X11.");
            return;
        }

        let mut args = vec![
            "--no-audio",
            "--loop-file=inf",
            "--force-window=no",
            "--no-osc",
            "--cursor-autohide=always",
            "--input-conf=/dev/null",
            "--no-input-default-bindings",
        ];
        #[cfg(target_os = "linux")]
        args.push("--wid");

        let mut last_error = None;

        for candidate in mpv_candidates() {
            let mut command = Command::new(&candidate);
            command.args(&args);
            #[cfg(target_os = "linux")]
            command.arg(&window_id);
            let child = command.arg(&path).spawn();
            match child {
                Ok(child) => {
                    *process.borrow_mut() = Some(BackgroundProcess { child });
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    last_error = Some(error);
                }
                Err(error) => {
                    last_error = Some(error);
                    break;
                }
            }
        }

        if !*mpv_unavailable_reported.borrow() {
            let error = last_error.expect("mpv candidates should not be empty");
            eprintln!(
                "Unable to start the video background for {}: {error}. Install mpv or set MUSICPI_MPV to its executable path.",
                path.display()
            );
            *mpv_unavailable_reported.borrow_mut() = true;
        }
    } else {
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
    let backgrounds = load_backgrounds().map_err(slint::PlatformError::from)?;
    let background_by_name = backgrounds
        .iter()
        .cloned()
        .map(|background| (background.name.clone(), background))
        .collect::<HashMap<_, _>>();
    let background_process = Rc::new(RefCell::new(None));
    let mpv_unavailable_reported = Rc::new(RefCell::new(false));
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
                .filter(|path| !is_video(path))
                .as_ref()
                .and_then(|path| Image::load_from_path(path).ok())
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
    let category_entries = |entries: &[BackgroundEntry], category: &str| {
        entries
            .iter()
            .filter(|entry| entry.category == category)
            .cloned()
            .collect::<Vec<_>>()
    };
    let device_settings = load_device_settings();
    let selected_name = (!device_settings.selected_background.is_empty())
        .then_some(device_settings.selected_background.clone())
        .filter(|name| background_by_name.contains_key(name))
        .unwrap_or_else(|| backgrounds[0].name.clone());

    window.set_background_options(ModelRc::new(VecModel::from(background_names)));
    window.set_background_entries(ModelRc::new(VecModel::from(background_entries)));
    window.set_animated_background_entries(ModelRc::new(VecModel::from(animated_entries)));
    window.set_still_background_entries(ModelRc::new(VecModel::from(still_entries.clone())));
    window.set_pokemon_background_entries(ModelRc::new(VecModel::from(category_entries(
        &still_entries,
        "pokemon",
    ))));
    window.set_lifestyle_background_entries(ModelRc::new(VecModel::from(category_entries(
        &still_entries,
        "lifestyle",
    ))));
    window.set_anime_background_entries(ModelRc::new(VecModel::from(category_entries(
        &still_entries,
        "anime",
    ))));
    window.set_background_name(SharedString::from(selected_name.as_str()));
    window.set_background_overlay_opacity(device_settings.background_overlay_opacity.clamp(0, 100));
    update_now_playing(&window, &mut image_loader.borrow_mut());
    apply_background(
        &window,
        &background_by_name[&selected_name],
        &background_process,
        &mpv_unavailable_reported,
    );

    window.on_open_local_music(open_local_music);
    window.on_open_spotify(open_spotify);
    window.on_open_video(open_video);
    let background_by_name_for_callback = background_by_name.clone();
    let background_process_for_callback = background_process.clone();
    let mpv_unavailable_reported_for_callback = mpv_unavailable_reported.clone();
    let window_weak = window.as_weak();
    window.on_background_selected(move |name| {
        let name = name.to_string();
        if let Some(background) = background_by_name_for_callback.get(&name) {
            if let Some(window) = window_weak.upgrade() {
                apply_background(
                    &window,
                    background,
                    &background_process_for_callback,
                    &mpv_unavailable_reported_for_callback,
                );
                window.set_background_name(SharedString::from(name.as_str()));
                save_device_settings(&name, window.get_background_overlay_opacity());
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

    let result = window.run();
    stop_background(&mut background_process.borrow_mut());
    result
}
