use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

use crate::control::{
    button_message, capture_snapshot, key_message, paste_message, point_gesture_message,
    send_control_message, send_control_messages, touch_message, ControlAckOutput, ControlTarget,
    SnapshotOptions,
};
use crate::pool::{LeaseOptions, PoolConfig, PoolDevice, PoolService};
use crate::preview::{run_preview, PreviewOptions};
use crate::simctl::{Simctl, XcrunSimctl};
use crate::stream::{serve, ServeConfig, StreamControlMode, StreamStats, StreamTransport};
use crate::update::{self, UpdateHint, UpdateOptions};

#[derive(Debug, Parser)]
#[command(
    name = "simx",
    version,
    about = "Agent-friendly iOS Simulator device pool",
    after_help = "Agent quick start:
  simx doctor --json
  simx init --size 2
  simx lease --slug browser --ttl 10m --json
  simx serve --slug browser --port 8080

One-shot browser stream:
  simx lease --slug browser --serve --port 8080 --idle-timeout 5m

SwiftUI preview hot reload:
  simx preview --slug browser --package Package.swift --package-target App

Native control:
  simx control snapshot --slug browser --json
  simx control tap --slug browser --nx 0.5 --ny 0.5 --json
  simx screenshot --slug browser --output screenshot.png --json
  simx record-video --slug browser --output demo.mp4 --duration 10s --json

Open:
  http://127.0.0.1:8080/browser
  ws://127.0.0.1:8080/browser/stream       (stable JPEG)
  ws://127.0.0.1:8080/browser/h264-stream  (experimental H.264/WebCodecs)
  http://127.0.0.1:8080/browser?transport=webrtc
  http://127.0.0.1:8080/browser/webrtc
  http://127.0.0.1:8080/browser/stats

JPEG is the stable fallback. H.264/WebCodecs and WebRTC are experimental until WAN benchmark evidence is strong."
)]
struct Cli {
    /// Print CLI errors as stable JSON.
    #[arg(long, global = true)]
    json_errors: bool,
    /// Skip the cached GitHub release check used for update hints.
    #[arg(long, global = true)]
    no_update_check: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize or reconcile the fixed simulator pool.
    #[command(after_help = "Examples:
  simx init --size 2
  simx init --size 4 --device-type \"iPhone 16\"")]
    Init {
        /// Number of managed simulator devices to keep in the pool.
        #[arg(long)]
        size: usize,
        /// Simulator device type name to create when the pool needs devices.
        #[arg(long)]
        device_type: Option<String>,
        /// Simulator runtime name or identifier to create when the pool needs devices.
        #[arg(long)]
        runtime: Option<String>,
    },
    /// Show the current pool state.
    Status {
        /// Print machine-readable pool state.
        #[arg(long)]
        json: bool,
    },
    /// Lease a simulator for an agent.
    #[command(after_help = "Examples:
  simx lease --slug browser --ttl 10m --json
  simx lease --slug browser --serve --port 8080 --idle-timeout 5m
  simx lease --slug browser --serve --port 8080 --fps 120 --transport h264
  simx lease --slug browser --serve --port 8080 --transport webrtc
  simx lease --slug browser --new --json

Notes:
  Reusing a slug renews and reuses its active lease unless --new is set.
  With --serve, open http://<host>:<port>/<slug> in a browser.
  JPEG is the stable fallback; --transport h264 and /<slug>/h264-stream are experimental.")]
    Lease {
        /// Stable lease owner name. Reusing the same slug renews/reuses the lease.
        #[arg(long)]
        slug: String,
        /// How long to wait for an idle simulator before failing.
        #[arg(long, default_value = "60s", value_parser = parse_duration)]
        wait_timeout: Duration,
        /// Lease lifetime before the simulator can be reclaimed.
        #[arg(long, default_value = "30m", value_parser = parse_duration)]
        ttl: Duration,
        /// Print machine-readable lease details including URL, stream URL, and stats URL.
        #[arg(long)]
        json: bool,
        /// Start the browser/WebSocket server after acquiring the lease.
        #[arg(long)]
        serve: bool,
        /// Host interface for the browser/WebSocket server when --serve is set.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port for the browser/WebSocket server when --serve is set.
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// JPEG quality for streamed frames, from 0.0 to 1.0.
        #[arg(long, default_value_t = 0.7)]
        quality: f32,
        /// Target stream frames per second.
        #[arg(long, default_value_t = 60)]
        fps: u32,
        /// Stream transport to serve to browsers; jpeg is stable fallback, h264 is experimental.
        #[arg(long, value_enum, default_value = "jpeg")]
        transport: CliTransport,
        /// Browser input ownership policy for the served simulator.
        #[arg(long, value_enum, default_value = "read-only")]
        control_mode: CliControlMode,
        /// Stop the server after this much time without viewer activity.
        #[arg(long, default_value = "5m", value_parser = parse_duration)]
        idle_timeout: Duration,
        /// Force a fresh lease instead of reusing an active lease for the same slug.
        #[arg(long)]
        new: bool,
    },
    /// Release a simulator lease back to the pool.
    Release {
        /// Lease owner name to release.
        #[arg(long)]
        slug: String,
    },
    /// Serve an existing active lease.
    #[command(after_help = "Examples:
  simx serve --slug browser --port 8080
  simx serve --slug browser --transport h264 --fps 120
  simx serve --slug browser --transport webrtc
  simx serve --slug browser --control-mode single-controller

Viewer:
  http://127.0.0.1:<port>/<slug>
  ws://127.0.0.1:<port>/<slug>/stream       (stable JPEG)
  ws://127.0.0.1:<port>/<slug>/h264-stream  (experimental H.264/WebCodecs)
  http://127.0.0.1:<port>/<slug>?transport=webrtc
  http://127.0.0.1:<port>/<slug>/webrtc     (WebRTC prototype descriptor)
  http://127.0.0.1:<port>/<slug>/stats

JPEG is the stable fallback. H.264/WebCodecs and WebRTC are experimental until WAN benchmark evidence is strong.")]
    Serve {
        /// Active lease owner name to serve.
        #[arg(long)]
        slug: String,
        /// Host interface for the browser/WebSocket server.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port for the browser/WebSocket server.
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// JPEG quality for streamed frames, from 0.0 to 1.0.
        #[arg(long, default_value_t = 0.7)]
        quality: f32,
        /// Target stream frames per second.
        #[arg(long, default_value_t = 60)]
        fps: u32,
        /// Stream transport to serve to browsers; jpeg is stable fallback, h264 is experimental.
        #[arg(long, value_enum, default_value = "jpeg")]
        transport: CliTransport,
        /// Browser input ownership policy for the served simulator.
        #[arg(long, value_enum, default_value = "read-only")]
        control_mode: CliControlMode,
        /// Stop the server after this much time without viewer activity.
        #[arg(long, default_value = "5m", value_parser = parse_duration)]
        idle_timeout: Duration,
    },
    /// Build, install, and launch the app in the current Xcode project.
    #[command(after_help = "Examples:
  simx run --slug browser --json
  simx run --slug browser --scheme MyApp --configuration Debug --json
  simx run --slug browser --project path/to/App.xcodeproj --json

Notes:
  Run from a folder containing one .xcodeproj, or pass --project.
  Build logs are written under .simx/logs/ and run metadata under .simx/run.json.")]
    Run {
        /// Active lease owner name to build, install, and launch on.
        #[arg(long)]
        slug: String,
        /// Xcode project path. Defaults to the only .xcodeproj in the current folder.
        #[arg(long)]
        project: Option<PathBuf>,
        /// Xcode scheme to build. Defaults to the discovered project scheme.
        #[arg(long)]
        scheme: Option<String>,
        /// Xcode build configuration.
        #[arg(long, default_value = "Debug")]
        configuration: String,
        /// DerivedData directory to use for the build.
        #[arg(long)]
        derived_data_path: Option<PathBuf>,
        /// Bundle identifier to launch. Defaults to CFBundleIdentifier from Info.plist.
        #[arg(long)]
        bundle_id: Option<String>,
        /// Install the app but do not launch it.
        #[arg(long)]
        no_launch: bool,
        /// Print machine-readable build, install, and launch details.
        #[arg(long)]
        json: bool,
    },
    /// Render Swift Package SwiftUI previews with hot reload on an active lease.
    #[command(after_help = "Examples:
  simx preview --slug browser --package Package.swift --package-target App
  simx preview --slug browser --package Package.swift --package-target App --preview-filter StatusRow
  simx preview --slug browser --package Package.swift --package-target App --once --json

Notes:
  Requires an active lease. Generates a disposable host project outside the package,
  installs it on the leased simulator, then watches Swift source changes by default.
  On change, simx rebuilds a preview dylib, copies it into the host data container,
  and notifies the running host without relaunching it.")]
    Preview {
        /// Active lease owner name to preview on.
        #[arg(long)]
        slug: String,
        /// Path to the Swift package manifest.
        #[arg(long, default_value = "Package.swift")]
        package: PathBuf,
        /// Swift Package target whose previews should be rendered.
        #[arg(long)]
        package_target: String,
        /// Regex filter matched against preview type, group, and display names.
        #[arg(long)]
        preview_filter: Vec<String>,
        /// Build and launch the preview host once instead of watching for hot reloads.
        #[arg(long)]
        once: bool,
        /// Print machine-readable preview session details after launch.
        #[arg(long)]
        json: bool,
    },
    /// Install and launch an app bundle on an active lease.
    #[command(after_help = "Examples:
  simx install --slug browser --app path/to/App.app --json
  simx install --slug browser --app path/to/App.app --bundle-id com.example.App --json")]
    Install {
        /// Active lease owner name to install on.
        #[arg(long)]
        slug: String,
        /// Path to the .app bundle to install.
        #[arg(long)]
        app: PathBuf,
        /// Bundle identifier to launch. Defaults to CFBundleIdentifier from Info.plist.
        #[arg(long)]
        bundle_id: Option<String>,
        /// Install the app but do not launch it.
        #[arg(long)]
        no_launch: bool,
        /// Print machine-readable install and launch details.
        #[arg(long)]
        json: bool,
    },
    /// Save a PNG screenshot from an active leased simulator.
    #[command(after_help = "Example:
  simx screenshot --slug browser --output screenshot.png --json

Notes:
  Requires an active lease. This is a one-shot capture command for agents and
  does not use the browser streaming pipeline.")]
    Screenshot {
        /// Active lease owner name to capture.
        #[arg(long)]
        slug: String,
        /// File path where the PNG screenshot should be written.
        #[arg(long)]
        output: PathBuf,
        /// Overwrite the output file if it already exists.
        #[arg(long)]
        force: bool,
        /// Print machine-readable capture details.
        #[arg(long)]
        json: bool,
    },
    /// Record a bounded MP4 video from an active leased simulator.
    #[command(after_help = "Example:
  simx record-video --slug browser --output demo.mp4 --duration 10s --json

Notes:
  Requires an active lease. simx stops recording after --duration and waits for
  simctl to finalize the video file.")]
    RecordVideo {
        /// Active lease owner name to capture.
        #[arg(long)]
        slug: String,
        /// File path where the MP4 video should be written.
        #[arg(long)]
        output: PathBuf,
        /// Recording duration before simx stops simctl recordVideo.
        #[arg(long, default_value = "10s", value_parser = parse_duration)]
        duration: Duration,
        /// Overwrite the output file if it already exists.
        #[arg(long)]
        force: bool,
        /// Print machine-readable capture details.
        #[arg(long)]
        json: bool,
    },
    /// Observe and control an active leased simulator with native commands.
    #[command(after_help = "Examples:
  simx control snapshot --slug browser --json
  simx control snapshot --slug browser --output snapshot.jpg --json
  simx control tap --slug browser --nx 0.5 --ny 0.5 --json
  simx control swipe --slug browser --from-nx 0.5 --from-ny 0.8 --to-nx 0.5 --to-ny 0.2 --json
  simx control paste --slug browser --text \"hello\" --json
  simx control button --slug browser home --json
  simx control button --slug browser soft-keyboard --json

Notes:
  Control commands operate on an active lease by slug.
  They use a short-lived native SimulatorKit session and do not require `simx serve`.
  Normalized coordinates use 0.0..1.0 with (0,0) at the top-left.")]
    Control {
        #[command(subcommand)]
        command: ControlCommand,
    },
    /// Check for or install the latest simx release binary.
    Update {
        /// Only check whether an update is available.
        #[arg(long)]
        check: bool,
        /// Install a specific release version instead of the latest release.
        #[arg(long)]
        version: Option<String>,
        /// Directory where the simx binary should be installed.
        #[arg(long)]
        install_dir: Option<PathBuf>,
        /// Print machine-readable update details.
        #[arg(long)]
        json: bool,
    },
    /// Extend an active simulator lease.
    Renew {
        /// Active lease owner name to renew.
        #[arg(long)]
        slug: String,
        /// New lease lifetime from now.
        #[arg(long, default_value = "30m", value_parser = parse_duration)]
        ttl: Duration,
        /// Print machine-readable renewal details.
        #[arg(long)]
        json: bool,
    },
    /// Shut down and delete all devices in the simx pool.
    Clean,
    /// Check host tooling required by simx.
    Doctor {
        /// Print machine-readable host diagnostics.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Serialize)]
struct LeaseOutput<'a> {
    slug: &'a str,
    udid: &'a str,
    device_name: &'a str,
    lease_expires_at: Option<u64>,
    lease_expires_at_rfc3339: Option<String>,
    ttl_seconds: u64,
    serve: ServeOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<UpdateHint>,
}

#[derive(Debug, Serialize)]
struct ServeOutput {
    command: String,
    url: String,
    stream: String,
    h264_url: String,
    h264_stream: String,
    webrtc_url: String,
    webrtc_signaling: String,
    stats: String,
    control_mode: String,
}

struct LeasePrintOptions<'a> {
    ttl: Duration,
    host: &'a str,
    port: u16,
    transport: CliTransport,
    control_mode: CliControlMode,
    json: bool,
    update: Option<UpdateHint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliTransport {
    Jpeg,
    H264,
    Webrtc,
}

impl From<CliTransport> for StreamTransport {
    fn from(value: CliTransport) -> Self {
        match value {
            CliTransport::Jpeg => Self::Jpeg,
            CliTransport::H264 => Self::H264,
            CliTransport::Webrtc => Self::Webrtc,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliControlMode {
    ReadOnly,
    SingleController,
    Claim,
    Shared,
}

#[derive(Debug, Subcommand)]
enum ControlCommand {
    /// Capture a simulator JPEG snapshot without printing image bytes by default.
    #[command(after_help = "Examples:
  simx control snapshot --slug browser --json
  simx control snapshot --slug browser --output snapshot.jpg --json
  simx control snapshot --slug browser --inline-base64 --json

Notes:
  The default JSON output is metadata-only and token-efficient.
  Use --output to write image bytes to disk, or --inline-base64 only when bytes are required.")]
    Snapshot {
        /// Active lease owner name to observe.
        #[arg(long)]
        slug: String,
        /// File path where the JPEG snapshot should be written.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Include JPEG bytes as base64 in JSON output.
        #[arg(long)]
        inline_base64: bool,
        /// How long to wait for the native snapshot frame.
        #[arg(long, default_value = "5s", value_parser = parse_duration)]
        wait_timeout: Duration,
        /// Print machine-readable snapshot metadata.
        #[arg(long)]
        json: bool,
    },
    /// Send a tap as touch began + touch ended.
    #[command(after_help = "Example:
  simx control tap --slug browser --nx 0.5 --ny 0.5 --json")]
    Tap {
        /// Active lease owner name to control.
        #[arg(long)]
        slug: String,
        /// Normalized horizontal coordinate, 0.0 left to 1.0 right.
        #[arg(long)]
        nx: f64,
        /// Normalized vertical coordinate, 0.0 top to 1.0 bottom.
        #[arg(long)]
        ny: f64,
        /// How long to wait for the native HID session.
        #[arg(long, default_value = "5s", value_parser = parse_duration)]
        wait_timeout: Duration,
        /// Print machine-readable acknowledgement details.
        #[arg(long)]
        json: bool,
    },
    /// Send one touch phase.
    #[command(after_help = "Examples:
  simx control touch --slug browser --phase began --nx 0.5 --ny 0.5 --json
  simx control touch --slug browser --phase ended --nx 0.5 --ny 0.5 --json")]
    Touch {
        /// Active lease owner name to control.
        #[arg(long)]
        slug: String,
        /// Touch phase to send.
        #[arg(long, value_enum)]
        phase: TouchPhase,
        /// Normalized horizontal coordinate, 0.0 left to 1.0 right.
        #[arg(long)]
        nx: f64,
        /// Normalized vertical coordinate, 0.0 top to 1.0 bottom.
        #[arg(long)]
        ny: f64,
        /// How long to wait for the native HID session.
        #[arg(long, default_value = "5s", value_parser = parse_duration)]
        wait_timeout: Duration,
        /// Print machine-readable acknowledgement details.
        #[arg(long)]
        json: bool,
    },
    /// Send a swipe helper message.
    #[command(after_help = "Example:
  simx control swipe --slug browser --from-nx 0.5 --from-ny 0.8 --to-nx 0.5 --to-ny 0.2 --json")]
    Swipe {
        /// Active lease owner name to control.
        #[arg(long)]
        slug: String,
        /// Normalized start X coordinate, 0.0 left to 1.0 right.
        #[arg(long)]
        from_nx: f64,
        /// Normalized start Y coordinate, 0.0 top to 1.0 bottom.
        #[arg(long)]
        from_ny: f64,
        /// Normalized end X coordinate, 0.0 left to 1.0 right.
        #[arg(long)]
        to_nx: f64,
        /// Normalized end Y coordinate, 0.0 top to 1.0 bottom.
        #[arg(long)]
        to_ny: f64,
        /// Number of intermediate move steps.
        #[arg(long)]
        steps: Option<u32>,
        /// How long to wait for the native HID session.
        #[arg(long, default_value = "5s", value_parser = parse_duration)]
        wait_timeout: Duration,
        /// Print machine-readable acknowledgement details.
        #[arg(long)]
        json: bool,
    },
    /// Send a drag helper message.
    #[command(after_help = "Example:
  simx control drag --slug browser --from-nx 0.2 --from-ny 0.2 --to-nx 0.8 --to-ny 0.8 --json")]
    Drag {
        /// Active lease owner name to control.
        #[arg(long)]
        slug: String,
        /// Normalized start X coordinate, 0.0 left to 1.0 right.
        #[arg(long)]
        from_nx: f64,
        /// Normalized start Y coordinate, 0.0 top to 1.0 bottom.
        #[arg(long)]
        from_ny: f64,
        /// Normalized end X coordinate, 0.0 left to 1.0 right.
        #[arg(long)]
        to_nx: f64,
        /// Normalized end Y coordinate, 0.0 top to 1.0 bottom.
        #[arg(long)]
        to_ny: f64,
        /// Number of intermediate move steps.
        #[arg(long)]
        steps: Option<u32>,
        /// How long to wait for the native HID session.
        #[arg(long, default_value = "5s", value_parser = parse_duration)]
        wait_timeout: Duration,
        /// Print machine-readable acknowledgement details.
        #[arg(long)]
        json: bool,
    },
    /// Send a key down/up pair for a browser KeyboardEvent.code value.
    #[command(after_help = "Example:
  simx control key --slug browser --code KeyA --json")]
    Key {
        /// Active lease owner name to control.
        #[arg(long)]
        slug: String,
        /// Browser KeyboardEvent.code value, for example KeyA or Enter.
        #[arg(long)]
        code: String,
        /// How long to wait for the native HID session.
        #[arg(long, default_value = "5s", value_parser = parse_duration)]
        wait_timeout: Duration,
        /// Print machine-readable acknowledgement details.
        #[arg(long)]
        json: bool,
    },
    /// Type supported text through simulated key events.
    #[command(after_help = "Example:
  simx control paste --slug browser --text \"hello\" --json")]
    Paste {
        /// Active lease owner name to control.
        #[arg(long)]
        slug: String,
        /// Text to type through simulated key events.
        #[arg(long)]
        text: String,
        /// How long to wait for the native HID session.
        #[arg(long, default_value = "5s", value_parser = parse_duration)]
        wait_timeout: Duration,
        /// Print machine-readable acknowledgement details.
        #[arg(long)]
        json: bool,
    },
    /// Press a hardware button or toggle the software keyboard.
    #[command(after_help = "Example:
  simx control button --slug browser home --json
  simx control button --slug browser soft-keyboard --json")]
    Button {
        /// Active lease owner name to control.
        #[arg(long)]
        slug: String,
        /// Hardware button to press.
        #[arg(value_enum)]
        button: ControlButton,
        /// How long to wait for the native HID session.
        #[arg(long, default_value = "5s", value_parser = parse_duration)]
        wait_timeout: Duration,
        /// Print machine-readable acknowledgement details.
        #[arg(long)]
        json: bool,
    },
    /// Return a simulator accessibility tree when a supported provider exists.
    #[command(after_help = "Example:
  simx control tree --slug browser --json

Notes:
  This command is reserved for a future accessibility provider and is not a stable data contract yet.")]
    Tree {
        /// Active lease owner name to inspect.
        #[arg(long)]
        slug: String,
        /// Print machine-readable tree details or unsupported-provider errors.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum TouchPhase {
    Began,
    Moved,
    Ended,
    Cancelled,
}

impl TouchPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Began => "began",
            Self::Moved => "moved",
            Self::Ended => "ended",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ControlButton {
    Home,
    #[value(name = "soft-keyboard", alias = "softKeyboard")]
    SoftKeyboard,
}

impl ControlButton {
    fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::SoftKeyboard => "softKeyboard",
        }
    }
}

impl From<CliControlMode> for StreamControlMode {
    fn from(value: CliControlMode) -> Self {
        match value {
            CliControlMode::ReadOnly => Self::ReadOnly,
            CliControlMode::SingleController => Self::SingleController,
            CliControlMode::Claim => Self::Claim,
            CliControlMode::Shared => Self::Shared,
        }
    }
}

#[derive(Debug, Serialize)]
struct StatusOutput {
    size: usize,
    device_type: String,
    runtime: String,
    devices: Vec<StatusDeviceOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<UpdateHint>,
}

#[derive(Debug, Serialize)]
struct StatusDeviceOutput {
    name: String,
    udid: String,
    slug: Option<String>,
    lease_expires_at: Option<u64>,
    lease_expires_at_rfc3339: Option<String>,
    serve_pid: Option<u32>,
    serve_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct RunOutput<'a> {
    slug: &'a str,
    udid: &'a str,
    run_state: String,
    log: String,
    project: String,
    scheme: String,
    configuration: String,
    derived_data_path: String,
    app: String,
    bundle_id: String,
    launched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<UpdateHint>,
}

#[derive(Debug, Serialize)]
struct InstallOutput<'a> {
    slug: &'a str,
    udid: &'a str,
    app: String,
    bundle_id: String,
    launched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<UpdateHint>,
}

#[derive(Debug, Serialize)]
struct MediaCaptureOutput<'a> {
    slug: &'a str,
    udid: &'a str,
    output: String,
    bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<UpdateHint>,
}

#[derive(Debug, Serialize)]
struct ErrorOutput<'a> {
    ok: bool,
    code: &'a str,
    message: String,
}

#[derive(Debug, Serialize)]
struct DoctorOutput {
    ok: bool,
    checks: Vec<DoctorCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<UpdateHint>,
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    name: &'static str,
    ok: bool,
    message: String,
}

pub fn main_entry() -> i32 {
    let wants_json_errors = std::env::args().any(|arg| arg == "--json-errors");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if wants_json_errors {
                let output = ErrorOutput {
                    ok: false,
                    code: "invalid_argument",
                    message: error.to_string(),
                };
                eprintln!(
                    "{}",
                    serde_json::to_string(&output).unwrap_or_else(|_| {
                        r#"{"ok":false,"code":"invalid_argument","message":"invalid arguments"}"#
                            .to_string()
                    })
                );
                return 2;
            }
            let _ = error.print();
            return error.exit_code();
        }
    };
    let json_errors = cli.json_errors;
    match default_state_path().and_then(|state_path| run_with(cli, state_path)) {
        Ok(()) => 0,
        Err(error) => {
            if json_errors {
                let output = ErrorOutput {
                    ok: false,
                    code: error_code(&error),
                    message: format!("{error:#}"),
                };
                eprintln!(
                    "{}",
                    serde_json::to_string(&output).unwrap_or_else(|_| {
                        r#"{"ok":false,"code":"internal","message":"failed to serialize error"}"#
                            .to_string()
                    })
                );
            } else {
                eprintln!("{error:#}");
            }
            exit_code(&error)
        }
    }
}

pub fn run() -> anyhow::Result<()> {
    run_with(Cli::parse(), default_state_path()?)
}

fn run_with(cli: Cli, state_path: PathBuf) -> anyhow::Result<()> {
    let update_hint = if cli.no_update_check || matches!(&cli.command, Command::Update { .. }) {
        None
    } else {
        update::maybe_update_hint()
    };
    if let Some(hint) = &update_hint {
        update::print_update_hint(hint);
    }

    let mut service = PoolService::new(state_path.clone());
    let mut simctl = XcrunSimctl;

    match cli.command {
        Command::Init {
            size,
            device_type,
            runtime,
        } => {
            let state = service.init(
                &mut simctl,
                PoolConfig {
                    size,
                    device_type,
                    runtime,
                },
            )?;
            println!("initialized {} simulator(s)", state.devices.len());
            for device in state.devices {
                println!("{} {}", device.name, device.udid);
            }
        }
        Command::Status { json } => {
            let state = service.status_with_simctl(&mut simctl)?;
            if json {
                let output = StatusOutput {
                    size: state.size,
                    device_type: state.device_type_id,
                    runtime: state.runtime_id,
                    update: update_hint,
                    devices: state
                        .devices
                        .into_iter()
                        .map(|device| {
                            let serve_url = match (
                                device.serve_host.as_deref(),
                                device.serve_port,
                                device.lease_id.as_deref(),
                            ) {
                                (Some(host), Some(port), Some(slug)) => {
                                    Some(format!("http://{host}:{port}/{slug}"))
                                }
                                _ => None,
                            };
                            StatusDeviceOutput {
                                name: device.name,
                                udid: device.udid,
                                slug: device.lease_id,
                                lease_expires_at: device.lease_expires_at,
                                lease_expires_at_rfc3339: device
                                    .lease_expires_at
                                    .map(format_unix_timestamp),
                                serve_pid: device.serve_pid,
                                serve_url,
                            }
                        })
                        .collect(),
                };
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("size: {}", state.size);
                println!("device_type: {}", state.device_type_id);
                println!("runtime: {}", state.runtime_id);
                for device in state.devices {
                    let owner = device.lease_id.as_deref().unwrap_or("idle");
                    let expires = device
                        .lease_expires_at
                        .map(format_unix_timestamp)
                        .unwrap_or_else(|| "-".to_string());
                    println!("{} {} {} {}", device.name, device.udid, owner, expires);
                }
            }
        }
        Command::Lease {
            slug,
            wait_timeout,
            ttl,
            json,
            serve: should_serve,
            host,
            port,
            quality,
            fps,
            transport,
            control_mode,
            idle_timeout,
            new: _new,
        } => {
            let device = service.lease(&mut simctl, &slug, LeaseOptions { wait_timeout, ttl })?;
            print_lease(
                &slug,
                &device,
                LeasePrintOptions {
                    ttl,
                    host: &host,
                    port,
                    transport,
                    control_mode,
                    json,
                    update: update_hint.clone(),
                },
            )?;
            if should_serve {
                run_serve(
                    &mut service,
                    ServeCommand {
                        slug,
                        host,
                        port,
                        quality,
                        fps,
                        transport: transport.into(),
                        control_mode: control_mode.into(),
                        idle_timeout,
                        udid: device.udid,
                    },
                    state_path,
                )?;
            }
        }
        Command::Serve {
            slug,
            host,
            port,
            quality,
            fps,
            transport,
            control_mode,
            idle_timeout,
        } => {
            let device = service.active_lease(&slug)?;
            run_serve(
                &mut service,
                ServeCommand {
                    slug,
                    host,
                    port,
                    quality,
                    fps,
                    transport: transport.into(),
                    control_mode: control_mode.into(),
                    idle_timeout,
                    udid: device.udid,
                },
                state_path,
            )?;
        }
        Command::Run {
            slug,
            project,
            scheme,
            configuration,
            derived_data_path,
            bundle_id,
            no_launch,
            json,
        } => {
            let device = service.active_lease(&slug)?;
            simctl
                .boot_if_needed(&device.udid)
                .with_context(|| format!("failed to boot {}", device.udid))?;
            run_xcode_app(RunAppCommand {
                slug,
                udid: device.udid,
                project,
                scheme,
                configuration,
                derived_data_path,
                bundle_id,
                launch: !no_launch,
                json,
                update: update_hint,
            })?;
        }
        Command::Preview {
            slug,
            package,
            package_target,
            preview_filter,
            once,
            json,
        } => {
            let device = service.active_lease(&slug)?;
            simctl
                .boot_if_needed(&device.udid)
                .with_context(|| format!("failed to boot {}", device.udid))?;
            run_preview(PreviewOptions {
                slug,
                udid: device.udid,
                state_path,
                package_swift: package,
                package_target,
                preview_filters: preview_filter,
                watch: !once,
                json,
            })?;
        }
        Command::Install {
            slug,
            app,
            bundle_id,
            no_launch,
            json,
        } => {
            let device = service.active_lease(&slug)?;
            simctl
                .boot_if_needed(&device.udid)
                .with_context(|| format!("failed to boot {}", device.udid))?;
            install_app_command(
                &slug,
                &device.udid,
                &app,
                bundle_id,
                !no_launch,
                json,
                update_hint,
            )?;
        }
        Command::Screenshot {
            slug,
            output,
            force,
            json,
        } => {
            let device = service.active_lease(&slug)?;
            simctl
                .boot_if_needed(&device.udid)
                .with_context(|| format!("failed to boot {}", device.udid))?;
            screenshot_command(&slug, &device.udid, &output, force, json, update_hint)?;
        }
        Command::RecordVideo {
            slug,
            output,
            duration,
            force,
            json,
        } => {
            let device = service.active_lease(&slug)?;
            simctl
                .boot_if_needed(&device.udid)
                .with_context(|| format!("failed to boot {}", device.udid))?;
            record_video_command(
                &slug,
                &device.udid,
                &output,
                duration,
                force,
                json,
                update_hint,
            )?;
        }
        Command::Control { command } => {
            run_control_command(&mut service, &mut simctl, command)?;
        }
        Command::Update {
            check,
            version,
            install_dir,
            json,
        } => {
            let output = update::run_update(UpdateOptions {
                check,
                version,
                install_dir,
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if output.installed {
                let installed_version = output
                    .installed_version
                    .as_deref()
                    .unwrap_or(&output.latest_version);
                println!(
                    "updated simx {} -> {}",
                    output.current_version, installed_version
                );
                if let Some(path) = output.install_path {
                    println!("installed {path}");
                }
            } else if output.update_available {
                println!(
                    "simx {} is available; current version is {}. Run `simx update` to upgrade.",
                    output.latest_version, output.current_version
                );
            } else {
                println!("simx {} is already current", output.current_version);
            }
        }
        Command::Release { slug } => {
            let released = service.release(&slug)?;
            for process in &released.serve_processes {
                stop_process(process.pid);
            }
            if released.released {
                println!("released {slug}");
            } else {
                println!("no lease found for {slug}");
            }
        }
        Command::Renew { slug, ttl, json } => {
            let device = service.renew(&slug, ttl)?;
            print_lease(
                &slug,
                &device,
                LeasePrintOptions {
                    ttl,
                    host: "127.0.0.1",
                    port: 8080,
                    transport: CliTransport::Jpeg,
                    control_mode: CliControlMode::ReadOnly,
                    json,
                    update: update_hint,
                },
            )?;
        }
        Command::Clean => {
            let state = service.status().ok();
            if let Some(state) = state {
                for device in state.devices {
                    if let Some(pid) = device.serve_pid {
                        stop_process(pid);
                    }
                }
            }
            let devices = service.clean(&mut simctl)?;
            println!("removed {} simulator(s)", devices.len());
            for device in devices {
                println!("{} {}", device.name, device.udid);
            }
        }
        Command::Doctor { json } => {
            let output = doctor(default_state_path()?.as_path(), update_hint);
            if json {
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                for check in &output.checks {
                    println!(
                        "{} {} - {}",
                        if check.ok { "ok" } else { "fail" },
                        check.name,
                        check.message
                    );
                }
            }
            if !output.ok {
                anyhow::bail!("doctor found failing checks");
            }
        }
    }

    Ok(())
}

struct ServeCommand {
    slug: String,
    host: String,
    port: u16,
    quality: f32,
    fps: u32,
    transport: StreamTransport,
    control_mode: StreamControlMode,
    idle_timeout: Duration,
    udid: String,
}

#[derive(Debug, Serialize)]
struct TapOutput {
    ok: bool,
    slug: String,
    udid: String,
    source: &'static str,
    command: &'static str,
    began: serde_json::Value,
    ended: serde_json::Value,
}

fn run_control_command(
    service: &mut PoolService,
    simctl: &mut impl Simctl,
    command: ControlCommand,
) -> anyhow::Result<()> {
    match command {
        ControlCommand::Snapshot {
            slug,
            output,
            inline_base64,
            wait_timeout,
            json,
        } => {
            let target = control_target(service, simctl, &slug)?;
            let snapshot = capture_snapshot(
                &target,
                SnapshotOptions {
                    output: output.as_deref(),
                    inline_base64,
                    wait_timeout,
                },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&snapshot)?);
            } else if let Some(path) = snapshot.path {
                println!("wrote {path}");
                println!(
                    "{} bytes; estimated inline base64 tokens: {}; metadata tokens: {}",
                    snapshot.metadata.bytes,
                    snapshot.metadata.estimated_base64_tokens,
                    snapshot.metadata.estimated_metadata_tokens
                );
            } else if inline_base64 {
                if let Some(base64) = snapshot.base64 {
                    println!("{base64}");
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&snapshot.metadata)?);
            }
        }
        ControlCommand::Tap {
            slug,
            nx,
            ny,
            wait_timeout,
            json,
        } => {
            validate_normalized_point(nx, ny)?;
            let target = control_target(service, simctl, &slug)?;
            let outputs = send_control_messages(
                &target,
                "tap",
                vec![
                    with_message_id(touch_message("began", nx, ny), "simx-control-tap-began"),
                    with_message_id(touch_message("ended", nx, ny), "simx-control-tap-ended"),
                ],
                wait_timeout,
            )?;
            let began = outputs
                .first()
                .with_context(|| "tap began acknowledgement missing")?;
            let ended = outputs
                .get(1)
                .with_context(|| "tap ended acknowledgement missing")?;
            let output = TapOutput {
                ok: began.ok && ended.ok,
                slug: target.slug,
                udid: target.udid,
                source: "native-hid",
                command: "tap",
                began: began.ack.clone(),
                ended: ended.ack.clone(),
            };
            print_control_json_or_summary(json, &output.ok, "tap", &output)?;
        }
        ControlCommand::Touch {
            slug,
            phase,
            nx,
            ny,
            wait_timeout,
            json,
        } => {
            validate_normalized_point(nx, ny)?;
            let target = control_target(service, simctl, &slug)?;
            let output = send_control_message(
                &target,
                "touch",
                touch_message(phase.as_str(), nx, ny),
                wait_timeout,
            )?;
            print_control_output(json, output)?;
        }
        ControlCommand::Swipe {
            slug,
            from_nx,
            from_ny,
            to_nx,
            to_ny,
            steps,
            wait_timeout,
            json,
        } => {
            validate_normalized_point(from_nx, from_ny)?;
            validate_normalized_point(to_nx, to_ny)?;
            let target = control_target(service, simctl, &slug)?;
            let output = send_control_message(
                &target,
                "swipe",
                point_gesture_message("swipe", from_nx, from_ny, to_nx, to_ny, steps),
                wait_timeout,
            )?;
            print_control_output(json, output)?;
        }
        ControlCommand::Drag {
            slug,
            from_nx,
            from_ny,
            to_nx,
            to_ny,
            steps,
            wait_timeout,
            json,
        } => {
            validate_normalized_point(from_nx, from_ny)?;
            validate_normalized_point(to_nx, to_ny)?;
            let target = control_target(service, simctl, &slug)?;
            let output = send_control_message(
                &target,
                "drag",
                point_gesture_message("drag", from_nx, from_ny, to_nx, to_ny, steps),
                wait_timeout,
            )?;
            print_control_output(json, output)?;
        }
        ControlCommand::Key {
            slug,
            code,
            wait_timeout,
            json,
        } => {
            let target = control_target(service, simctl, &slug)?;
            let outputs = send_control_messages(
                &target,
                "key",
                vec![
                    with_message_id(key_message(&code, "down"), "simx-control-key-down"),
                    with_message_id(key_message(&code, "up"), "simx-control-key-up"),
                ],
                wait_timeout,
            )?;
            let down = outputs
                .first()
                .with_context(|| "key down acknowledgement missing")?;
            let up = outputs
                .get(1)
                .with_context(|| "key up acknowledgement missing")?;
            let output = TapOutput {
                ok: down.ok && up.ok,
                slug: target.slug,
                udid: target.udid,
                source: "native-hid",
                command: "key",
                began: down.ack.clone(),
                ended: up.ack.clone(),
            };
            print_control_json_or_summary(json, &output.ok, "key", &output)?;
        }
        ControlCommand::Paste {
            slug,
            text,
            wait_timeout,
            json,
        } => {
            let target = control_target(service, simctl, &slug)?;
            let output =
                send_control_message(&target, "paste", paste_message(&text), wait_timeout)?;
            print_control_output(json, output)?;
        }
        ControlCommand::Button {
            slug,
            button,
            wait_timeout,
            json,
        } => {
            let target = control_target(service, simctl, &slug)?;
            let output = send_control_message(
                &target,
                "button",
                button_message(button.as_str()),
                wait_timeout,
            )?;
            print_control_output(json, output)?;
        }
        ControlCommand::Tree { slug, json } => {
            let target = control_target(service, simctl, &slug)?;
            let output = serde_json::json!({
                "ok": false,
                "slug": target.slug,
                "udid": target.udid,
                "source": "accessibility-tree",
                "error": "unsupported",
                "message": "simx control tree is reserved for an accessibility snapshot provider; no provider is implemented yet"
            });
            if json {
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                anyhow::bail!("{}", output["message"].as_str().unwrap_or("unsupported"));
            }
            anyhow::bail!("accessibility tree provider is not implemented");
        }
    }
    Ok(())
}

fn control_target(
    service: &mut PoolService,
    simctl: &mut impl Simctl,
    slug: &str,
) -> anyhow::Result<ControlTarget> {
    let device = service.active_lease(slug)?;
    simctl
        .boot_if_needed(&device.udid)
        .with_context(|| format!("failed to boot {}", device.udid))?;
    Ok(ControlTarget {
        slug: slug.to_string(),
        udid: device.udid,
    })
}

fn validate_normalized_point(nx: f64, ny: f64) -> anyhow::Result<()> {
    if !(0.0..=1.0).contains(&nx) || !(0.0..=1.0).contains(&ny) {
        anyhow::bail!("normalized coordinates must be within 0.0..=1.0");
    }
    Ok(())
}

fn with_message_id(mut message: serde_json::Value, id: &str) -> serde_json::Value {
    if let Some(object) = message.as_object_mut() {
        object.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    }
    message
}

fn print_control_output(json: bool, output: ControlAckOutput) -> anyhow::Result<()> {
    let ok = output.ok;
    print_control_json_or_summary(json, &ok, &output.command.clone(), &output)
}

fn print_control_json_or_summary<T: Serialize>(
    json: bool,
    ok: &bool,
    command: &str,
    output: &T,
) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(output)?);
    } else if *ok {
        println!("{command} ok");
    } else {
        println!("{command} rejected");
    }
    if !*ok {
        anyhow::bail!("{command} rejected");
    }
    Ok(())
}

fn run_serve(
    service: &mut PoolService,
    command: ServeCommand,
    state_path: PathBuf,
) -> anyhow::Result<()> {
    warn_if_non_local_serve_host(&command.host);
    let pid = std::process::id();
    service.register_serve(
        &command.slug,
        &command.udid,
        pid,
        &command.host,
        command.port,
    )?;
    let result = serve(ServeConfig {
        host: command.host,
        port: command.port,
        quality: command.quality,
        fps: command.fps,
        transport: command.transport,
        control_mode: command.control_mode,
        idle_timeout: command.idle_timeout,
        slug: command.slug.clone(),
        udid: command.udid,
        state_path,
        stats: Arc::new(Mutex::new(StreamStats::default())),
        controllers: Arc::new(Mutex::new(None)),
    });
    let clear_result = service.clear_serve(&command.slug, pid);
    result.and(clear_result)
}

fn warn_if_non_local_serve_host(host: &str) {
    if !is_local_serve_host(host) {
        eprintln!(
            "warning: simx browser streaming is unauthenticated; binding to {host} may expose simulator streaming and input control on public networks"
        );
    }
}

fn is_local_serve_host(host: &str) -> bool {
    matches!(
        host.trim().to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "::1"
    )
}

struct RunAppCommand {
    slug: String,
    udid: String,
    project: Option<PathBuf>,
    scheme: Option<String>,
    configuration: String,
    derived_data_path: Option<PathBuf>,
    bundle_id: Option<String>,
    launch: bool,
    json: bool,
    update: Option<UpdateHint>,
}

fn run_xcode_app(command: RunAppCommand) -> anyhow::Result<()> {
    let project = resolve_xcode_project(command.project.as_deref())?;
    let scheme = command
        .scheme
        .unwrap_or_else(|| default_scheme_for_project(&project));
    let derived_data_path = command
        .derived_data_path
        .unwrap_or_else(|| default_derived_data_path(&command.slug));
    let log_path = default_run_log_path(&command.slug)?;
    build_xcode_app(
        &project,
        &scheme,
        &command.configuration,
        &command.udid,
        &derived_data_path,
        &log_path,
    )?;
    let app = find_built_app(&derived_data_path, &command.configuration, &scheme)?;
    let bundle_id = install_app(&command.udid, &app, command.bundle_id, command.launch)?;
    let run_state_path = write_run_state(RunStateInput {
        slug: &command.slug,
        udid: &command.udid,
        project: &project,
        scheme: &scheme,
        configuration: &command.configuration,
        derived_data_path: &derived_data_path,
        app: &app,
        bundle_id: &bundle_id,
        log: &log_path,
        launched: command.launch,
    })?;

    if command.json {
        let output = RunOutput {
            slug: &command.slug,
            udid: &command.udid,
            run_state: run_state_path.display().to_string(),
            log: log_path.display().to_string(),
            project: project.display().to_string(),
            scheme,
            configuration: command.configuration,
            derived_data_path: derived_data_path.display().to_string(),
            app: app.display().to_string(),
            bundle_id,
            launched: command.launch,
            update: command.update,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("built {}", project.display());
        println!("installed {}", app.display());
        println!("wrote {}", run_state_path.display());
        println!("log {}", log_path.display());
        if command.launch {
            println!("launched {bundle_id}");
        }
    }
    Ok(())
}

fn install_app_command(
    slug: &str,
    udid: &str,
    app: &Path,
    bundle_id: Option<String>,
    launch: bool,
    json: bool,
    update: Option<UpdateHint>,
) -> anyhow::Result<()> {
    let bundle_id = install_app(udid, app, bundle_id, launch)?;
    if json {
        let output = InstallOutput {
            slug,
            udid,
            app: app.display().to_string(),
            bundle_id,
            launched: launch,
            update,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("installed {}", app.display());
        if launch {
            println!("launched {bundle_id}");
        }
    }
    Ok(())
}

fn screenshot_command(
    slug: &str,
    udid: &str,
    output: &Path,
    force: bool,
    json: bool,
    update: Option<UpdateHint>,
) -> anyhow::Result<()> {
    prepare_media_output(output, force)?;
    let output_string = output.display().to_string();
    if json {
        let process_output = ProcessCommand::new("/usr/bin/xcrun")
            .args(["simctl", "io", udid, "screenshot"])
            .arg(output)
            .output()
            .context("failed to run xcrun simctl io screenshot")?;
        if !process_output.status.success() {
            anyhow::bail!(
                "simctl io screenshot failed: {}",
                String::from_utf8_lossy(&process_output.stderr).trim()
            );
        }
    } else {
        let status = ProcessCommand::new("/usr/bin/xcrun")
            .args(["simctl", "io", udid, "screenshot"])
            .arg(output)
            .status()
            .context("failed to run xcrun simctl io screenshot")?;
        if !status.success() {
            anyhow::bail!("simctl io screenshot failed");
        }
    }
    print_media_capture_output(slug, udid, &output_string, None, json, update)
}

fn record_video_command(
    slug: &str,
    udid: &str,
    output: &Path,
    duration: Duration,
    force: bool,
    json: bool,
    update: Option<UpdateHint>,
) -> anyhow::Result<()> {
    if duration.is_zero() {
        anyhow::bail!("duration must be greater than zero");
    }
    prepare_media_output(output, force)?;
    let output_string = output.display().to_string();
    let mut command = ProcessCommand::new("/usr/bin/xcrun");
    command
        .args(["simctl", "io", udid, "recordVideo"])
        .arg(output);
    if json {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let mut child = command
        .spawn()
        .context("failed to run xcrun simctl io recordVideo")?;
    let started = Instant::now();
    while started.elapsed() < duration {
        thread::sleep((duration - started.elapsed()).min(Duration::from_millis(250)));
        if let Some(status) = child
            .try_wait()
            .context("failed to poll simctl io recordVideo")?
        {
            if status.success() {
                break;
            }
            anyhow::bail!("simctl io recordVideo failed");
        }
    }
    if child
        .try_wait()
        .context("failed to poll simctl io recordVideo")?
        .is_none()
    {
        let kill_status = ProcessCommand::new("/bin/kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .context("failed to stop simctl io recordVideo")?;
        if !kill_status.success() {
            let _ = child.kill();
            anyhow::bail!("failed to stop simctl io recordVideo");
        }
    }
    let status = child
        .wait()
        .context("failed to wait for simctl io recordVideo")?;
    if !status.success() {
        anyhow::bail!("simctl io recordVideo failed");
    }
    print_media_capture_output(
        slug,
        udid,
        &output_string,
        Some(duration.as_secs()),
        json,
        update,
    )
}

fn prepare_media_output(output: &Path, force: bool) -> anyhow::Result<()> {
    if output.exists() {
        if force {
            fs::remove_file(output)
                .with_context(|| format!("failed to remove {}", output.display()))?;
        } else {
            anyhow::bail!("output file already exists: {}", output.display());
        }
    }
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

fn print_media_capture_output(
    slug: &str,
    udid: &str,
    output: &str,
    duration_seconds: Option<u64>,
    json: bool,
    update: Option<UpdateHint>,
) -> anyhow::Result<()> {
    let bytes = fs::metadata(output)
        .with_context(|| format!("failed to inspect {output}"))?
        .len();
    if json {
        let output = MediaCaptureOutput {
            slug,
            udid,
            output: output.to_string(),
            bytes,
            duration_seconds,
            update,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("saved {output} ({bytes} bytes)");
    }
    Ok(())
}

struct RunStateInput<'a> {
    slug: &'a str,
    udid: &'a str,
    project: &'a Path,
    scheme: &'a str,
    configuration: &'a str,
    derived_data_path: &'a Path,
    app: &'a Path,
    bundle_id: &'a str,
    log: &'a Path,
    launched: bool,
}

#[derive(Debug, Serialize)]
struct RunState<'a> {
    version: u32,
    slug: &'a str,
    udid: &'a str,
    project: String,
    scheme: &'a str,
    configuration: &'a str,
    derived_data_path: String,
    app: String,
    bundle_id: &'a str,
    log: String,
    launched: bool,
    updated_at: String,
}

fn write_run_state(input: RunStateInput<'_>) -> anyhow::Result<PathBuf> {
    let path = PathBuf::from(".simx").join("run.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let state = RunState {
        version: 1,
        slug: input.slug,
        udid: input.udid,
        project: input.project.display().to_string(),
        scheme: input.scheme,
        configuration: input.configuration,
        derived_data_path: input.derived_data_path.display().to_string(),
        app: input.app.display().to_string(),
        bundle_id: input.bundle_id,
        log: input.log.display().to_string(),
        launched: input.launched,
        updated_at: format_unix_timestamp(now_unix_seconds()?),
    };
    let json = serde_json::to_string_pretty(&state)?;
    fs::write(&path, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn resolve_xcode_project(project: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(project) = project {
        validate_xcode_project(project)?;
        return Ok(project.to_path_buf());
    }

    let mut projects = Vec::new();
    for entry in fs::read_dir(".").context("failed to read current directory")? {
        let path = entry?.path();
        if is_xcode_project(&path) {
            projects.push(path);
        }
    }
    projects.sort();
    match projects.len() {
        0 => anyhow::bail!("no .xcodeproj found in the current directory"),
        1 => Ok(projects.remove(0)),
        _ => anyhow::bail!(
            "multiple .xcodeproj files found; pass --project explicitly: {}",
            projects
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn validate_xcode_project(project: &Path) -> anyhow::Result<()> {
    if !is_xcode_project(project) {
        anyhow::bail!(
            "project path must point to a .xcodeproj: {}",
            project.display()
        );
    }
    Ok(())
}

fn is_xcode_project(path: &Path) -> bool {
    path.exists()
        && path.is_dir()
        && path.extension().and_then(|value| value.to_str()) == Some("xcodeproj")
}

fn default_scheme_for_project(project: &Path) -> String {
    project
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("App")
        .to_string()
}

fn default_derived_data_path(slug: &str) -> PathBuf {
    PathBuf::from(".simx")
        .join("DerivedData")
        .join(safe_path_component(slug))
}

fn default_run_log_path(slug: &str) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(".simx").join("logs").join(format!(
        "{}-{}-xcodebuild.log",
        now_unix_seconds()?,
        safe_path_component(slug)
    )))
}

fn safe_path_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn build_xcode_app(
    project: &Path,
    scheme: &str,
    configuration: &str,
    udid: &str,
    derived_data_path: &Path,
    log_path: &Path,
) -> anyhow::Result<()> {
    let output = ProcessCommand::new("/usr/bin/xcodebuild")
        .arg("-project")
        .arg(project)
        .arg("-scheme")
        .arg(scheme)
        .arg("-configuration")
        .arg(configuration)
        .arg("-destination")
        .arg(format!("platform=iOS Simulator,id={udid}"))
        .arg("-derivedDataPath")
        .arg(derived_data_path)
        .arg("build")
        .output()
        .context("failed to run xcodebuild")?;
    write_command_log(log_path, "xcodebuild", &output.stdout, &output.stderr)?;
    if output.status.success() {
        return Ok(());
    }
    anyhow::bail!(
        "xcodebuild failed; log: {}\n{}",
        log_path.display(),
        command_failure_summary(&output.stdout, &output.stderr)
    );
}

fn write_command_log(
    log_path: &Path,
    command_name: &str,
    stdout: &[u8],
    stderr: &[u8],
) -> anyhow::Result<()> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut log = String::new();
    log.push_str("Command: ");
    log.push_str(command_name);
    log.push_str("\n\n--- stdout ---\n");
    log.push_str(&String::from_utf8_lossy(stdout));
    log.push_str("\n--- stderr ---\n");
    log.push_str(&String::from_utf8_lossy(stderr));
    fs::write(log_path, log).with_context(|| format!("failed to write {}", log_path.display()))
}

fn command_failure_summary(stdout: &[u8], stderr: &[u8]) -> String {
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    let lines = combined
        .lines()
        .filter(|line| {
            line.contains("error:")
                || line.contains("fatal error:")
                || line.contains("BUILD FAILED")
                || line.contains("Testing failed")
        })
        .take(20)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        combined
            .lines()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        lines.join("\n")
    }
}

fn find_built_app(
    derived_data_path: &Path,
    configuration: &str,
    scheme: &str,
) -> anyhow::Result<PathBuf> {
    let products_dir = derived_data_path
        .join("Build")
        .join("Products")
        .join(format!("{configuration}-iphonesimulator"));
    let expected = products_dir.join(format!("{scheme}.app"));
    if expected.exists() {
        return Ok(expected);
    }

    let mut apps = Vec::new();
    for entry in fs::read_dir(&products_dir).with_context(|| {
        format!(
            "failed to read build products at {}",
            products_dir.display()
        )
    })? {
        let path = entry?.path();
        if validate_app_path(&path).is_ok() {
            apps.push(path);
        }
    }
    apps.sort();
    match apps.len() {
        0 => anyhow::bail!("no built .app found under {}", products_dir.display()),
        1 => Ok(apps.remove(0)),
        _ => anyhow::bail!(
            "multiple .app bundles found; pass --scheme to disambiguate: {}",
            apps.iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn install_app(
    udid: &str,
    app: &Path,
    bundle_id: Option<String>,
    launch: bool,
) -> anyhow::Result<String> {
    validate_app_path(app)?;
    let bundle_id = match bundle_id {
        Some(bundle_id) => bundle_id,
        None => infer_bundle_id(app)?,
    };
    run_simctl(["install", udid, path_as_str(app)?])?;
    if launch {
        run_simctl(["launch", udid, &bundle_id])?;
    }
    Ok(bundle_id)
}

fn validate_app_path(app: &Path) -> anyhow::Result<()> {
    if !app.exists() {
        anyhow::bail!("app path does not exist: {}", app.display());
    }
    if !app.is_dir() || app.extension().and_then(|value| value.to_str()) != Some("app") {
        anyhow::bail!("app path must point to a .app bundle: {}", app.display());
    }
    Ok(())
}

fn infer_bundle_id(app: &Path) -> anyhow::Result<String> {
    let info_plist = app.join("Info.plist");
    if !info_plist.exists() {
        anyhow::bail!(
            "could not infer bundle id because Info.plist is missing: {}",
            info_plist.display()
        );
    }
    let output = ProcessCommand::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
        .arg(&info_plist)
        .output()
        .context("failed to run plutil")?;
    if !output.status.success() {
        anyhow::bail!(
            "could not infer bundle id: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let bundle_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if bundle_id.is_empty() {
        anyhow::bail!("could not infer bundle id because CFBundleIdentifier is empty");
    }
    Ok(bundle_id)
}

fn run_simctl<const N: usize>(args: [&str; N]) -> anyhow::Result<()> {
    let output = ProcessCommand::new("/usr/bin/xcrun")
        .arg("simctl")
        .args(args)
        .output()
        .context("failed to run xcrun simctl")?;
    if output.status.success() {
        return Ok(());
    }
    anyhow::bail!(
        "simctl {} failed: {}",
        args.first().copied().unwrap_or("command"),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn path_as_str(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn print_lease(
    slug: &str,
    device: &PoolDevice,
    options: LeasePrintOptions<'_>,
) -> anyhow::Result<()> {
    let LeasePrintOptions {
        ttl,
        host,
        port,
        transport,
        control_mode,
        json,
        update,
    } = options;
    let transport_arg = match transport {
        CliTransport::Jpeg => String::new(),
        CliTransport::H264 => " --transport h264".to_string(),
        CliTransport::Webrtc => " --transport webrtc".to_string(),
    };
    let control_mode_arg = match control_mode {
        CliControlMode::ReadOnly => String::new(),
        CliControlMode::SingleController => " --control-mode single-controller".to_string(),
        CliControlMode::Claim => " --control-mode claim".to_string(),
        CliControlMode::Shared => " --control-mode shared".to_string(),
    };
    if json {
        let output = LeaseOutput {
            slug,
            udid: &device.udid,
            device_name: &device.name,
            lease_expires_at: device.lease_expires_at,
            lease_expires_at_rfc3339: device.lease_expires_at.map(format_unix_timestamp),
            ttl_seconds: ttl.as_secs(),
            serve: ServeOutput {
                command: format!(
                    "simx serve --slug {slug} --host {host} --port {port}{transport_arg}{control_mode_arg}"
                ),
                url: match transport {
                    CliTransport::Jpeg => format!("http://{host}:{port}/{slug}"),
                    CliTransport::H264 => format!("http://{host}:{port}/{slug}?transport=h264"),
                    CliTransport::Webrtc => {
                        format!("http://{host}:{port}/{slug}?transport=webrtc")
                    }
                },
                stream: format!("ws://{host}:{port}/{slug}/stream"),
                h264_url: format!("http://{host}:{port}/{slug}?transport=h264"),
                h264_stream: format!("ws://{host}:{port}/{slug}/h264-stream"),
                webrtc_url: format!("http://{host}:{port}/{slug}?transport=webrtc"),
                webrtc_signaling: format!("http://{host}:{port}/{slug}/webrtc-offer"),
                stats: format!("http://{host}:{port}/{slug}/stats"),
                control_mode: StreamControlMode::from(control_mode).as_str().to_string(),
            },
            update,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", device.udid);
        if let Some(expires_at) = device.lease_expires_at {
            println!("lease expires at {}", format_unix_timestamp(expires_at));
        }
        println!("serve with: simx serve --slug {slug} --host {host} --port {port}{transport_arg}{control_mode_arg}");
        match transport {
            CliTransport::Jpeg => {}
            CliTransport::H264 => println!("viewer: http://{host}:{port}/{slug}?transport=h264"),
            CliTransport::Webrtc => {
                println!("viewer: http://{host}:{port}/{slug}?transport=webrtc");
                println!("signaling: http://{host}:{port}/{slug}/webrtc-offer");
            }
        }
    }
    Ok(())
}

fn format_unix_timestamp(timestamp: u64) -> String {
    humantime::format_rfc3339_seconds(UNIX_EPOCH + Duration::from_secs(timestamp)).to_string()
}

fn now_unix_seconds() -> anyhow::Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}

fn parse_duration(raw: &str) -> Result<Duration, String> {
    humantime::parse_duration(raw).map_err(|error| error.to_string())
}

fn stop_process(pid: u32) {
    let _ = ProcessCommand::new("/bin/kill")
        .args(["-TERM", &pid.to_string()])
        .status();
}

fn error_code(error: &anyhow::Error) -> &'static str {
    let message = format!("{error:#}");
    if message.contains("pool is not initialized") {
        "pool_not_initialized"
    } else if message.contains("timed out waiting") || message.contains("pool is full") {
        "pool_full"
    } else if message.contains("no active lease") || message.contains("no lease found") {
        "lease_not_found"
    } else if message.contains("doctor found failing") {
        "doctor_failed"
    } else if message.contains("ttl must")
        || message.contains("lease id must")
        || message.contains("app path")
        || message.contains(".xcodeproj")
        || message.contains("xcodebuild failed")
        || message.contains("could not infer bundle id")
        || message.contains("path is not valid UTF-8")
        || message.contains("release version must")
    {
        "invalid_argument"
    } else {
        "internal"
    }
}

fn exit_code(error: &anyhow::Error) -> i32 {
    match error_code(error) {
        "invalid_argument" => 2,
        "pool_not_initialized" | "pool_full" | "lease_not_found" => 3,
        "doctor_failed" => 4,
        _ => 1,
    }
}

fn doctor(state_path: &Path, update: Option<UpdateHint>) -> DoctorOutput {
    let mut checks = Vec::new();
    checks.push(command_check(
        "xcode-select",
        "/usr/bin/xcode-select",
        &["-p"],
    ));
    checks.push(command_check(
        "xcrun",
        "/usr/bin/xcrun",
        &["simctl", "help"],
    ));
    checks.push(path_check(
        "CoreSimulator",
        Path::new("/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator"),
    ));
    let simulator_kit = developer_dir_for_doctor()
        .map(|developer_dir| {
            PathBuf::from(developer_dir)
                .join("Library/PrivateFrameworks/SimulatorKit.framework/SimulatorKit")
        })
        .unwrap_or_else(|| {
            PathBuf::from(
                "/Applications/Xcode.app/Contents/Developer/Library/PrivateFrameworks/SimulatorKit.framework/SimulatorKit",
            )
        });
    checks.push(path_check("SimulatorKit", &simulator_kit));
    checks.push(command_check(
        "iOS runtime",
        "/usr/bin/xcrun",
        &["simctl", "list", "runtimes", "-j"],
    ));
    checks.push(DoctorCheck {
        name: "state directory",
        ok: state_path.parent().is_some(),
        message: state_path
            .parent()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "state path has no parent".to_string()),
    });
    DoctorOutput {
        ok: checks.iter().all(|check| check.ok),
        checks,
        update,
    }
}

fn command_check(name: &'static str, command: &str, args: &[&str]) -> DoctorCheck {
    match ProcessCommand::new(command).args(args).output() {
        Ok(output) if output.status.success() => DoctorCheck {
            name,
            ok: true,
            message: String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("available")
                .to_string(),
        },
        Ok(output) => DoctorCheck {
            name,
            ok: false,
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        },
        Err(error) => DoctorCheck {
            name,
            ok: false,
            message: error.to_string(),
        },
    }
}

fn path_check(name: &'static str, path: &Path) -> DoctorCheck {
    DoctorCheck {
        name,
        ok: path.exists(),
        message: path.display().to_string(),
    }
}

fn developer_dir_for_doctor() -> Option<String> {
    let output = ProcessCommand::new("/usr/bin/xcode-select")
        .arg("-p")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn default_state_path() -> anyhow::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let base = directories::BaseDirs::new().context("could not resolve home directory")?;
        Ok(base
            .home_dir()
            .join("Library/Application Support/simx/pool.json"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!("simx currently supports macOS only");
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::CommandFactory;

    use super::*;

    fn help_for_subcommand(name: &str) -> String {
        let mut command = Cli::command();
        command
            .find_subcommand_mut(name)
            .unwrap_or_else(|| panic!("missing {name} subcommand"))
            .render_long_help()
            .to_string()
    }

    #[test]
    fn help_labels_h264_transport_as_experimental() {
        let root_help = Cli::command().render_long_help().to_string();
        assert!(root_help.contains("stream       (stable JPEG)"));
        assert!(root_help.contains("h264-stream  (experimental H.264/WebCodecs)"));
        assert!(root_help.contains("browser?transport=webrtc"));
        assert!(root_help.contains("browser/webrtc"));
        assert!(root_help.contains("JPEG is the stable fallback"));
        assert!(root_help.contains("H.264/WebCodecs and WebRTC are experimental"));

        for subcommand in ["lease", "serve"] {
            let help = help_for_subcommand(subcommand);
            assert!(help.contains("--transport h264"));
            assert!(help.contains("--transport webrtc"));
            assert!(help.contains("jpeg is stable fallback, h264 is experimental"));
            assert!(help.contains("JPEG is the stable fallback"));
        }
    }

    #[cfg_attr(not(target_os = "macos"), ignore = "plutil is only available on macOS")]
    #[test]
    fn infer_bundle_id_reads_app_info_plist() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Example.app");
        fs::create_dir(&app).unwrap();
        fs::write(
            app.join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>
  <string>ai.boncasa.example</string>
</dict>
</plist>
"#,
        )
        .unwrap();

        assert_eq!(infer_bundle_id(&app).unwrap(), "ai.boncasa.example");
    }

    #[test]
    fn validate_app_path_requires_app_bundle_directory() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("Example.txt");
        fs::write(&file, "not an app").unwrap();

        assert!(validate_app_path(&file).is_err());
    }

    #[test]
    fn validate_xcode_project_requires_xcodeproj_directory() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("Example.xcodeproj");
        fs::create_dir(&project).unwrap();
        let file = temp.path().join("Example.txt");
        fs::write(&file, "not a project").unwrap();

        assert!(validate_xcode_project(&project).is_ok());
        assert!(validate_xcode_project(&file).is_err());
    }

    #[test]
    fn default_scheme_uses_project_name() {
        assert_eq!(
            default_scheme_for_project(Path::new("Lumi.xcodeproj")),
            "Lumi"
        );
    }

    #[test]
    fn safe_path_component_replaces_unsafe_characters() {
        assert_eq!(safe_path_component("agent/one two"), "agent-one-two");
    }

    #[test]
    fn local_serve_hosts_do_not_warn() {
        assert!(is_local_serve_host("127.0.0.1"));
        assert!(is_local_serve_host("localhost"));
        assert!(is_local_serve_host("::1"));
        assert!(!is_local_serve_host("0.0.0.0"));
        assert!(!is_local_serve_host("192.168.1.10"));
    }
}
