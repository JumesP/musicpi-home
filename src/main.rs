use std::{
    cell::RefCell,
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    rc::Rc,
    sync::mpsc::{self, Receiver, SyncSender},
};
use std::io::BufRead;

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
    #[serde(default = "default_background_fit")]
    background_fit: String,
    #[serde(default)]
    selected_audio_sink: String,
    #[serde(default = "default_volume_percent")]
    volume_percent: i32,
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
fn default_volume_percent() -> i32 {
    58
}
fn default_background_fit() -> String {
    "Stretch".to_string()
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

fn apply_audio_volume(
    sink_name: &str,
    volume_percent: i32,
) -> Result<(), String> {
    let volume = format!("{}%", volume_percent.clamp(0, 100));

    let output = Command::new("amixer")
        .args(["-c", "1", "sset", "Headphone", &volume])
        .output()
        .map_err(|e| format!("Failed to run pactl: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "Failed to set volume: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(())
}

fn music_library_is_running() -> Result<bool, String> {
    let status = Command::new("pgrep")
        .args(["-f", "[/]home/pi/music-library/bin/ui"])
        .status()
        .map_err(|error| {
            format!("Failed to check whether Music Library is running: {error}")
        })?;

    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(code) => Err(format!(
            "Music Library process check exited with status {code}"
        )),
        None => Err(
            "Music Library process check was terminated by a signal".to_string()
        ),
    }
}

fn open_local_music() {
    match music_library_is_running() {
        Ok(true) => {
            println!("Music Library is already running.");
            return;
        }
        Ok(false) => {}
        Err(error) => {
            eprintln!("{error}");
            return;
        }
    }

    match Command::new("/home/pi/music-library/bin/ui")
        .current_dir("/home/pi/music-library")
        .env("WAYLAND_DISPLAY", "wayland-0")
        .env("XDG_RUNTIME_DIR", "/run/user/1000")
        .spawn()
    {
        Ok(_) => println!("Launching Music Library..."),
        Err(error) => eprintln!("Failed to launch Music Library: {error}"),
    }
}

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

#[derive(Clone, Debug)]
struct AudioSink {
    name: String,
    label: String,
}

#[derive(Clone, Debug)]
struct BluetoothDeviceInfo {
    address: String,
    name: String,
    paired: bool,
    connected: bool,
    status: String,
}

fn list_paired_bluetooth_devices() -> Vec<BluetoothDeviceInfo> {
    let output = match Command::new("bluetoothctl")
        .args(["devices"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            eprintln!(
                "Could not list paired Bluetooth devices: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return Vec::new();
        }
        Err(error) => {
            eprintln!("Could not run bluetoothctl: {error}");
            return Vec::new();
        }
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, ' ');
            if fields.next()? != "Device" {
                return None;
            }

            let address = fields.next()?.to_string();
            let name = fields.next()?.trim().to_string();

            if address.is_empty() {
                return None;
            }

            let info = Command::new("bluetoothctl")
                .args(["info", address.as_str()])
                .output()
                .ok();

            let info_text = info
                .as_ref()
                .filter(|result| result.status.success())
                .map(|result| String::from_utf8_lossy(&result.stdout).into_owned())
                .unwrap_or_default();

            let connected = info_text.lines().any(|line| {
                line.trim() == "Connected: yes"
            });

            let paired = info_text.lines().any(|line| {
                line.trim() == "Paired: yes"
            });

            let name = info_text
                .lines()
                .filter_map(|line| {
                    let line = line.trim();

                    line.strip_prefix("Alias:")
                        .or_else(|| line.strip_prefix("Name:"))
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                })
                .find(|name| !name.eq_ignore_ascii_case(&address))
                .or_else(|| {
                    (!name.is_empty() && !name.eq_ignore_ascii_case(&address))
                        .then_some(name.as_str())
                })
                .unwrap_or("Unknown Bluetooth device")
                .to_string();

            let status = if connected {
                "Connected"
            } else if paired {
                "Paired · Disconnected"
            } else {
                "Not paired"
            };

            Some(BluetoothDeviceInfo {
                address,
                name,
                paired,
                connected,
                status: status.to_string(),
            })
        })
        .collect()
}

fn toggle_bluetooth_device(address: &str) {
    let devices = list_paired_bluetooth_devices();

    let Some(device) = devices.iter().find(|device| device.address == address) else {
        eprintln!("Bluetooth device not found: {address}");
        return;
    };

    let action = if device.connected {
        "disconnect"
    } else {
        "connect"
    };

    match Command::new("bluetoothctl")
        .args([action, address])
        .status()
    {
        Ok(status) if status.success() => {
            println!("Bluetooth {action} requested for {address}");
        }
        Ok(status) => {
            eprintln!("Bluetooth {action} failed with status: {status}");
        }
        Err(error) => {
            eprintln!("Could not run bluetoothctl: {error}");
        }
    }
}

fn bluetooth_device_model() -> ModelRc<BluetoothDevice> {
    let devices = list_paired_bluetooth_devices()
        .into_iter()
        .map(|device| BluetoothDevice {
            address: SharedString::from(device.address),
            name: SharedString::from(device.name),
            paired: device.paired,
            connected: device.connected,
            status: SharedString::from(device.status),
        })
        .collect::<Vec<_>>();

    ModelRc::new(VecModel::from(devices))
}

fn start_bluetooth_discovery() {
    match Command::new("bluetoothctl")
        .args(["--timeout", "10", "scan", "on"])
        .status()
    {
        Ok(status) if status.success() => {
            println!("Bluetooth discovery completed");
        }
        Ok(status) => {
            eprintln!("Bluetooth discovery exited with status: {status}");
        }
        Err(error) => {
            eprintln!("Could not start Bluetooth discovery: {error}");
        }
    }
}

fn audio_sink_label(name: &str) -> String {
    let lower = name.to_ascii_lowercase();

    if lower.contains("bluez_output.") {
        // Extract the Bluetooth address from the PipeWire sink name.
        let address = name
            .strip_prefix("bluez_output.")
            .and_then(|rest| rest.rsplit_once('.').map(|(address, _)| address))
            .map(|address| address.replace('_', ":"));

        if let Some(address) = address {
            let output = Command::new("bluetoothctl")
                .args(["info", &address])
                .output();

            if let Ok(output) = output {
                let info = String::from_utf8_lossy(&output.stdout);

                if let Some(alias) = info.lines().find_map(|line| {
                    let line = line.trim();
                    line.strip_prefix("Alias:")
                        .map(str::trim)
                        .filter(|alias| !alias.is_empty())
                }) {
                    return alias.to_string();
                }
            }
        }

        return "Bluetooth headphones".to_string();
    }

    if lower.contains("hdmi") {
        "HDMI".to_string()
    } else if lower.contains("wm8960")
        || lower.contains("soc_sound")
        || lower.contains("stereo-fallback")
    {
        "WM8960 / Headphones".to_string()
    } else {
        name.strip_prefix("alsa_output.")
            .unwrap_or(name)
            .replace('_', " ")
            .replace('.', " ")
    }
}


fn list_audio_sinks() -> Vec<AudioSink> {
    let output = match Command::new("pactl").args(["list", "short", "sinks"]).output() {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            eprintln!("Could not list audio outputs: {}", String::from_utf8_lossy(&output.stderr).trim());
            return Vec::new();
        }
        Err(error) => {
            eprintln!("Could not run pactl to list audio outputs: {error}");
            return Vec::new();
        }
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _id = fields.next()?;
            let name = fields.next()?.to_string();
            Some(AudioSink {
                label: audio_sink_label(&name),
                name,
            })
        })
        .collect()
}

fn default_audio_sink() -> Option<String> {
    let output = Command::new("pactl").args(["get-default-sink"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Sets the system default and moves every currently active PipeWire/PulseAudio
/// sink-input to the selected sink. Applications using direct ALSA bypass this layer.
fn apply_audio_output(sink_name: &str) -> Result<(), String> {
    let output = Command::new("pactl")
        .args(["set-default-sink", sink_name])
        .output()
        .map_err(|error| format!("Could not run pactl: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Could not set default audio output to {sink_name}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let inputs = Command::new("pactl")
        .args(["list", "short", "sink-inputs"])
        .output()
        .map_err(|error| format!("Could not list active audio streams: {error}"))?;
    if !inputs.status.success() {
        return Err(format!(
            "Could not list active audio streams: {}",
            String::from_utf8_lossy(&inputs.stderr).trim()
        ));
    }

    for line in String::from_utf8_lossy(&inputs.stdout).lines() {
        let Some(stream_id) = line.split_whitespace().next() else {
            continue;
        };
        let moved = Command::new("pactl")
            .args(["move-sink-input", stream_id, sink_name])
            .output();
        match moved {
            Ok(result) if result.status.success() => {}
            Ok(result) => eprintln!(
                "Could not move audio stream {stream_id} to {sink_name}: {}",
                String::from_utf8_lossy(&result.stderr).trim()
            ),
            Err(error) => eprintln!("Could not move audio stream {stream_id}: {error}"),
        }
    }
    Ok(())
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
            background_fit: default_background_fit(),
            selected_audio_sink: String::new(),
            volume_percent: 58,
        })
}

fn save_device_settings(
    name: &str,
    background_overlay_opacity: i32,
    video_speed_percent: i32,
    background_fit: &str,
    selected_audio_sink: Option<&str>,
) {
    let existing_audio_sink = load_device_settings().selected_audio_sink;
    let selected_audio_sink = selected_audio_sink
        .map(str::to_string)
        .unwrap_or(existing_audio_sink);
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
        background_fit: background_fit.to_string(),
        selected_audio_sink,
        volume_percent: 58,
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
    window.set_bluetooth_devices(bluetooth_device_model());
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
    let background_fit = match device_settings.background_fit.as_str() {
        "Crop" => "Crop",
        _ => "Stretch",
    };
    window.set_background_fit(SharedString::from(background_fit));

    let audio_sinks = list_audio_sinks();
    let saved_sink = device_settings.selected_audio_sink.clone();
    let selected_sink = audio_sinks
        .iter()
        .find(|sink| sink.name == saved_sink)
        .map(|sink| sink.name.clone())
        .or_else(default_audio_sink)
        .or_else(|| audio_sinks.first().map(|sink| sink.name.clone()));
    let current_audio_sink = Rc::new(RefCell::new(selected_sink.clone().unwrap_or_default()));
    window.set_audio_outputs(
        std::rc::Rc::new(slint::VecModel::from(
            audio_sinks
                .iter()
                .map(|sink| AudioOutput {
                    name: SharedString::from(sink.name.as_str()),
                    label: SharedString::from(sink.label.as_str()),
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    let audio_monitor_window = window.as_weak();

    std::thread::spawn(move || {
    loop {
        let mut child = match Command::new("pactl")
            .arg("subscribe")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                eprintln!("Could not monitor audio device changes: {error}");
                std::thread::sleep(std::time::Duration::from_secs(3));
                continue;
            }
        };

        if let Some(stdout) = child.stdout.take() {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };

                if !line.contains(" on sink ") {
                    continue;
                }

                let window = audio_monitor_window.clone();

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = window.upgrade() {
                        let sinks = list_audio_sinks();

                        let outputs = sinks
                            .into_iter()
                            .map(|sink| AudioOutput {
                                name: SharedString::from(sink.name),
                                label: SharedString::from(sink.label),
                            })
                            .collect::<Vec<_>>();

                        window.set_audio_outputs(
                            std::rc::Rc::new(slint::VecModel::from(outputs)).into(),
                        );
                    }
                });
            }
        }

        let _ = child.kill();
        let _ = child.wait();

        std::thread::sleep(std::time::Duration::from_secs(2));
    }
});
    if let Some(selected_name) = selected_sink.as_deref() {
        if !saved_sink.is_empty() && saved_sink == selected_name {
            if let Err(error) = apply_audio_output(selected_name) {
                eprintln!("Could not restore saved audio output: {error}");
            }
        }
        let label = audio_sinks
            .iter()
            .find(|sink| sink.name == selected_name)
            .map(|sink| sink.label.as_str())
            .unwrap_or(selected_name);
        window.set_audio_output(SharedString::from(label));
    } else {
        window.set_audio_output(SharedString::from("No outputs found"));
    }

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
                    &window.get_background_fit(),
                    None,
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
                &window.get_background_fit(),
                None,
            );
        }
    });
    window.on_bluetooth_device_selected({
        let window = window.as_weak();

        move |address| {
            toggle_bluetooth_device(address.as_str());

            if let Some(window) = window.upgrade() {
                window.set_bluetooth_devices(bluetooth_device_model());
            }
        }
    });
    let weak = window.as_weak();
    window.on_system_volume_changed(move |volume| {
        let Some(window) = weak.upgrade() else {
            return;
        };

        let sink_name = window.get_audio_output().to_string();

        if let Err(error) = apply_audio_volume(&sink_name, volume) {
            eprintln!("Failed to change system volume: {error}");
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
                &window.get_background_fit(),
                None,
            );
        }
    });
    let fit_window_weak = window.as_weak();
    window.on_background_fit_changed(move |fit| {
        if let Some(window) = fit_window_weak.upgrade() {
            let fit = if fit == "Crop" { "Crop" } else { "Stretch" };
            window.set_background_fit(SharedString::from(fit));
            save_device_settings(
                &window.get_background_name().to_string(),
                window.get_background_overlay_opacity(),
                window.get_video_speed_percent(),
                fit,
                None,
            );
        }
    });
    window.on_bluetooth_scan_requested({
        let window = window.as_weak();

        move || {
            let window = window.clone();

            std::thread::spawn(move || {
                start_bluetooth_discovery();

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = window.upgrade() {
                        window.set_bluetooth_devices(bluetooth_device_model());
                    }
                });
            });
        }
    });
    let audio_window_weak = window.as_weak();
    let current_audio_sink_for_callback = current_audio_sink.clone();
    window.on_audio_output_requested(move || {
        let Some(window) = audio_window_weak.upgrade() else {
            return;
        };
        let available_sinks = list_audio_sinks();
        if available_sinks.is_empty() {
            eprintln!("No audio outputs are available");
            window.set_audio_output(SharedString::from("No outputs found"));
            return;
        }

        let current = current_audio_sink_for_callback.borrow().clone();
        let current_index = available_sinks
            .iter()
            .position(|sink| sink.name == current)
            .unwrap_or(available_sinks.len() - 1);
        let next = &available_sinks[(current_index + 1) % available_sinks.len()];
        match apply_audio_output(&next.name) {
            Ok(()) => {
                *current_audio_sink_for_callback.borrow_mut() = next.name.clone();
                window.set_audio_output(SharedString::from(next.label.as_str()));
                save_device_settings(
                    &window.get_background_name().to_string(),
                    window.get_background_overlay_opacity(),
                    window.get_video_speed_percent(),
                    &window.get_background_fit(),
                    Some(&next.name),
                );
            }
            Err(error) => eprintln!("Could not change audio output: {error}"),
        }
    });
    window.on_bluetooth_devices_refresh_requested({
        let window = window.as_weak();

        move || {
            if let Some(window) = window.upgrade() {
                window.set_bluetooth_devices(bluetooth_device_model());
            }
        }
    });

    let audio_select_window_weak = window.as_weak();
    let current_audio_sink_for_select = current_audio_sink.clone();

    window.on_audio_output_selected(move |selected_name| {
        let Some(window) = audio_select_window_weak.upgrade() else {
            return;
        };

        let selected_name = selected_name.to_string();
        let available_sinks = list_audio_sinks();

        let Some(sink) = available_sinks
            .iter()
            .find(|sink| sink.name == selected_name)
        else {
            eprintln!("Audio output not found: {selected_name}");
            return;
        };

        match apply_audio_output(&sink.name) {
            Ok(()) => {
                *current_audio_sink_for_select.borrow_mut() = sink.name.clone();
                window.set_audio_output(SharedString::from(sink.label.as_str()));

                save_device_settings(
                    &window.get_background_name().to_string(),
                    window.get_background_overlay_opacity(),
                    window.get_video_speed_percent(),
                    &window.get_background_fit(),
                    Some(&sink.name),
                );
            }
            Err(error) => eprintln!("Could not change audio output: {error}"),
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
