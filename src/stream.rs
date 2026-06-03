use std::collections::VecDeque;
#[cfg(target_os = "macos")]
use std::ffi::{c_char, c_uchar, c_ulong, c_void, CStr, CString};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
#[cfg(target_os = "macos")]
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use base64::Engine;
use serde::Serialize;
use sha1::{Digest, Sha1};

use crate::pool::PoolService;

const VIEWER_HTML: &str = include_str!("../viewer/index.html");
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub host: String,
    pub port: u16,
    pub quality: f32,
    pub fps: u32,
    pub idle_timeout: Duration,
    pub slug: String,
    pub udid: String,
    pub state_path: std::path::PathBuf,
    pub stats: Arc<Mutex<StreamStats>>,
    pub controllers: Arc<Mutex<Option<u64>>>,
}

#[derive(Debug, Serialize)]
struct Health<'a> {
    status: &'a str,
    slug: &'a str,
    udid: &'a str,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct StreamStats {
    pub target_fps: u32,
    pub connected_clients: u64,
    pub source_frames: u64,
    pub sent_frames: u64,
    pub dropped_frames: u64,
    pub last_frame_bytes: usize,
    pub uptime_ms: u128,
    pub source_fps: f64,
    pub sent_fps: f64,
    pub source_fps_1s: f64,
    pub source_fps_5s: f64,
    pub sent_fps_1s: f64,
    pub sent_fps_5s: f64,
    pub bytes_per_second_1s: f64,
    pub bytes_per_second_5s: f64,
    pub encode_latency_ms_p50: Option<u128>,
    pub encode_latency_ms_p95: Option<u128>,
    pub last_frame_age_ms: Option<u128>,
    pub last_send_age_ms: Option<u128>,
    pub last_delivery_latency_ms: Option<u128>,
    pub paused: bool,
    pub controller_connected: bool,
    #[serde(skip_serializing)]
    started_at: Option<Instant>,
    #[serde(skip_serializing)]
    last_source_at: Option<Instant>,
    #[serde(skip_serializing)]
    last_sent_at: Option<Instant>,
    #[serde(skip_serializing)]
    source_samples: VecDeque<Instant>,
    #[serde(skip_serializing)]
    sent_samples: VecDeque<Instant>,
    #[serde(skip_serializing)]
    byte_samples: VecDeque<(Instant, usize)>,
    #[serde(skip_serializing)]
    latency_samples: VecDeque<u128>,
}

pub fn serve(config: ServeConfig) -> anyhow::Result<()> {
    validate_config(&config)?;
    let listener = TcpListener::bind((config.host.as_str(), config.port))
        .with_context(|| format!("failed to bind {}:{}", config.host, config.port))?;
    listener.set_nonblocking(true)?;
    println!("HTTP viewer at http://{}:{}/", config.host, config.port);
    println!(
        "WebSocket stream at ws://{}:{}/{}/stream",
        config.host, config.port, config.slug
    );

    loop {
        if !lease_is_active(&config)? {
            println!("lease {} released; stopping stream", config.slug);
            break;
        }
        match listener.accept() {
            Ok((stream, _addr)) => {
                let config = config.clone();
                thread::spawn(move || {
                    if let Err(error) = handle_connection(stream, config) {
                        eprintln!("connection error: {error:#}");
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(200));
            }
            Err(error) => eprintln!("accept error: {error}"),
        }
    }
    Ok(())
}

fn validate_config(config: &ServeConfig) -> anyhow::Result<()> {
    if config.host.trim().is_empty() {
        bail!("host must not be empty");
    }
    if !(0.0..=1.0).contains(&config.quality) {
        bail!("quality must be between 0.0 and 1.0");
    }
    if config.fps == 0 || config.fps > 240 {
        bail!("fps must be between 1 and 240");
    }
    Ok(())
}

fn lease_is_active(config: &ServeConfig) -> anyhow::Result<bool> {
    let mut service = PoolService::new(config.state_path.clone());
    let state = service.status()?;
    Ok(state.devices.iter().any(|device| {
        device.udid == config.udid && device.lease_id.as_deref() == Some(&config.slug)
    }))
}

fn handle_connection(mut stream: TcpStream, config: ServeConfig) -> anyhow::Result<()> {
    stream.set_nonblocking(false)?;
    let request = read_http_request(&mut stream)?;
    let target = request_path(&request).unwrap_or("/");
    let path = target.split('?').next().unwrap_or(target);
    if is_ws_upgrade(&request) && path == stream_path(&config.slug) {
        let key = header_value(&request, "sec-websocket-key")
            .context("missing Sec-WebSocket-Key")?
            .to_string();
        write_ws_upgrade(&mut stream, &key)?;
        stream.set_read_timeout(Some(Duration::from_millis(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        stream_frames(stream, config)?;
        return Ok(());
    }

    match path {
        "/" => {
            let target = format!("/{}/", config.slug);
            write_http_redirect(&mut stream, &target)
        }
        path if path == slug_path(&config.slug) || path == slug_path_slash(&config.slug) => {
            let html = VIEWER_HTML.replace("__SIMX_SLUG__", &config.slug);
            write_http_response(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                html.as_bytes(),
            )
        }
        "/health" => {
            let body = serde_json::to_vec(&Health {
                status: "ok",
                slug: &config.slug,
                udid: &config.udid,
            })?;
            write_http_response(&mut stream, "200 OK", "application/json", &body)
        }
        path if path == stats_path(&config.slug) => {
            let body = serde_json::to_vec(&snapshot_stats(&config))?;
            write_http_response(&mut stream, "200 OK", "application/json", &body)
        }
        _ => write_http_response(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"Not found\n",
        ),
    }
}

fn stream_path(slug: &str) -> String {
    format!("/{slug}/stream")
}

fn stats_path(slug: &str) -> String {
    format!("/{slug}/stats")
}

fn slug_path(slug: &str) -> String {
    format!("/{slug}")
}

fn slug_path_slash(slug: &str) -> String {
    format!("/{slug}/")
}

fn stream_frames(mut stream: TcpStream, config: ServeConfig) -> anyhow::Result<()> {
    let frame_source = NativeFrameSource::start(&config)?;
    let client_id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
    let is_controller = acquire_controller(&config, client_id);
    let frame_interval = Duration::from_secs_f64(1.0 / config.fps as f64);
    let mut last_lease_check = Instant::now();
    let mut last_activity = Instant::now();
    let mut paused = false;
    let mut input_buffer = Vec::new();
    let mut last_sent_generation = 0_u64;
    update_stats(&config, |stats| {
        stats.connected_clients = stats.connected_clients.saturating_add(1);
        stats.target_fps = config.fps;
        stats.paused = false;
        stats.controller_connected = is_controller;
    });
    let _client_stats = ClientStatsGuard::new(config.stats.clone());
    let _controller = ControllerGuard::new(
        config.controllers.clone(),
        config.stats.clone(),
        client_id,
        is_controller,
    );
    write_ws_text(
        &mut stream,
        if is_controller {
            r#"{"type":"client","role":"controller"}"#
        } else {
            r#"{"type":"client","role":"viewer"}"#
        },
    )?;
    loop {
        let events = read_ws_events(&mut stream, &mut input_buffer);
        let mut should_close = false;
        for event in events {
            match event {
                WsEvent::Text(text) => {
                    if is_resume_message(&text) {
                        paused = false;
                        last_activity = Instant::now();
                        update_stats(&config, |stats| stats.paused = false);
                        write_ws_text(&mut stream, r#"{"type":"resumed"}"#)?;
                    } else if is_controller {
                        last_activity = Instant::now();
                        match frame_source.handle_input(&text) {
                            Ok(acks) => {
                                for ack in acks {
                                    write_ws_text(&mut stream, &ack)?;
                                }
                            }
                            Err(error) => {
                                if let Some(ack) = input_ack(&text, false, &error.to_string()) {
                                    write_ws_text(&mut stream, &ack)?;
                                }
                                eprintln!("input error: {error:#}");
                            }
                        }
                    } else {
                        if let Some(ack) = input_ack(&text, false, "client is viewer-only") {
                            write_ws_text(&mut stream, &ack)?;
                        }
                    }
                }
                WsEvent::Ping(payload) => {
                    write_ws_frame(&mut stream, 0xA, &payload)?;
                }
                WsEvent::Close => {
                    let _ = write_ws_frame(&mut stream, 0x8, &[]);
                    should_close = true;
                    break;
                }
            }
        }
        if should_close {
            break;
        }
        if last_lease_check.elapsed() >= Duration::from_secs(1) {
            if !lease_is_active(&config)? {
                break;
            }
            last_lease_check = Instant::now();
        }
        if !paused && last_activity.elapsed() >= config.idle_timeout {
            paused = true;
            update_stats(&config, |stats| stats.paused = true);
            write_ws_text(&mut stream, r#"{"type":"paused","reason":"idle_timeout"}"#)?;
        }
        if paused {
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        if let Some((generation, frame, received_at)) =
            frame_source.latest_frame_after(last_sent_generation)
        {
            let dropped = generation
                .saturating_sub(last_sent_generation)
                .saturating_sub(1);
            last_sent_generation = generation;
            write_ws_frame(&mut stream, 0x2, &frame)?;
            update_stats(&config, |stats| {
                let now = Instant::now();
                stats.sent_frames = stats.sent_frames.saturating_add(1);
                stats.dropped_frames = stats.dropped_frames.saturating_add(dropped);
                stats.last_frame_bytes = frame.len();
                stats.last_sent_at = Some(now);
                let latency = now.duration_since(received_at).as_millis();
                stats.last_delivery_latency_ms = Some(latency);
                push_sample(&mut stats.sent_samples, now, Duration::from_secs(5));
                push_byte_sample(
                    &mut stats.byte_samples,
                    now,
                    frame.len(),
                    Duration::from_secs(5),
                );
                stats.latency_samples.push_back(latency);
                while stats.latency_samples.len() > 240 {
                    stats.latency_samples.pop_front();
                }
            });
        }
        thread::sleep(frame_interval);
    }
    let _ = write_ws_frame(&mut stream, 0x8, &[]);
    Ok(())
}

struct ClientStatsGuard {
    stats: Arc<Mutex<StreamStats>>,
}

impl ClientStatsGuard {
    fn new(stats: Arc<Mutex<StreamStats>>) -> Self {
        Self { stats }
    }
}

impl Drop for ClientStatsGuard {
    fn drop(&mut self) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.connected_clients = stats.connected_clients.saturating_sub(1);
            if stats.connected_clients == 0 {
                stats.paused = false;
            }
        }
    }
}

struct ControllerGuard {
    controllers: Arc<Mutex<Option<u64>>>,
    stats: Arc<Mutex<StreamStats>>,
    client_id: u64,
    active: bool,
}

impl ControllerGuard {
    fn new(
        controllers: Arc<Mutex<Option<u64>>>,
        stats: Arc<Mutex<StreamStats>>,
        client_id: u64,
        active: bool,
    ) -> Self {
        Self {
            controllers,
            stats,
            client_id,
            active,
        }
    }
}

impl Drop for ControllerGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Ok(mut controller) = self.controllers.lock() {
            if controller.as_ref() == Some(&self.client_id) {
                *controller = None;
                if let Ok(mut stats) = self.stats.lock() {
                    stats.controller_connected = false;
                }
            }
        }
    }
}

fn acquire_controller(config: &ServeConfig, client_id: u64) -> bool {
    if let Ok(mut controller) = config.controllers.lock() {
        if controller.is_none() {
            *controller = Some(client_id);
            return true;
        }
    }
    false
}

fn snapshot_stats(config: &ServeConfig) -> StreamStats {
    let mut stats = config
        .stats
        .lock()
        .map(|stats| stats.clone())
        .unwrap_or_default();
    let now = Instant::now();
    if let Some(started_at) = stats.started_at {
        stats.uptime_ms = now.duration_since(started_at).as_millis();
        let elapsed_secs = now.duration_since(started_at).as_secs_f64();
        if elapsed_secs > 0.0 {
            stats.source_fps = stats.source_frames as f64 / elapsed_secs;
            stats.sent_fps = stats.sent_frames as f64 / elapsed_secs;
        }
    }
    prune_samples(&mut stats.source_samples, now, Duration::from_secs(5));
    prune_samples(&mut stats.sent_samples, now, Duration::from_secs(5));
    prune_byte_samples(&mut stats.byte_samples, now, Duration::from_secs(5));
    stats.source_fps_1s = count_since(&stats.source_samples, now, Duration::from_secs(1)) as f64;
    stats.source_fps_5s =
        count_since(&stats.source_samples, now, Duration::from_secs(5)) as f64 / 5.0;
    stats.sent_fps_1s = count_since(&stats.sent_samples, now, Duration::from_secs(1)) as f64;
    stats.sent_fps_5s = count_since(&stats.sent_samples, now, Duration::from_secs(5)) as f64 / 5.0;
    stats.bytes_per_second_1s =
        bytes_since(&stats.byte_samples, now, Duration::from_secs(1)) as f64;
    stats.bytes_per_second_5s =
        bytes_since(&stats.byte_samples, now, Duration::from_secs(5)) as f64 / 5.0;
    let mut latencies: Vec<_> = stats.latency_samples.iter().copied().collect();
    latencies.sort_unstable();
    stats.encode_latency_ms_p50 = percentile(&latencies, 50);
    stats.encode_latency_ms_p95 = percentile(&latencies, 95);
    stats.last_frame_age_ms = stats
        .last_source_at
        .map(|last_source_at| now.duration_since(last_source_at).as_millis());
    stats.last_send_age_ms = stats
        .last_sent_at
        .map(|last_sent_at| now.duration_since(last_sent_at).as_millis());
    stats
}

fn update_stats(config: &ServeConfig, update: impl FnOnce(&mut StreamStats)) {
    if let Ok(mut stats) = config.stats.lock() {
        if stats.started_at.is_none() {
            stats.started_at = Some(Instant::now());
        }
        update(&mut stats);
    }
}

fn push_sample(samples: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    samples.push_back(now);
    prune_samples(samples, now, window);
}

fn prune_samples(samples: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while samples
        .front()
        .is_some_and(|sample| now.duration_since(*sample) > window)
    {
        samples.pop_front();
    }
}

fn count_since(samples: &VecDeque<Instant>, now: Instant, window: Duration) -> usize {
    samples
        .iter()
        .filter(|sample| now.duration_since(**sample) <= window)
        .count()
}

fn push_byte_sample(
    samples: &mut VecDeque<(Instant, usize)>,
    now: Instant,
    bytes: usize,
    window: Duration,
) {
    samples.push_back((now, bytes));
    prune_byte_samples(samples, now, window);
}

fn prune_byte_samples(samples: &mut VecDeque<(Instant, usize)>, now: Instant, window: Duration) {
    while samples
        .front()
        .is_some_and(|(sample, _)| now.duration_since(*sample) > window)
    {
        samples.pop_front();
    }
}

fn bytes_since(samples: &VecDeque<(Instant, usize)>, now: Instant, window: Duration) -> usize {
    samples
        .iter()
        .filter(|(sample, _)| now.duration_since(*sample) <= window)
        .map(|(_, bytes)| *bytes)
        .sum()
}

fn percentile(values: &[u128], percentile: usize) -> Option<u128> {
    if values.is_empty() {
        return None;
    }
    let index = ((values.len() - 1) * percentile) / 100;
    values.get(index).copied()
}

fn read_http_request(stream: &mut TcpStream) -> anyhow::Result<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > 16 * 1024 {
            bail!("request header too large");
        }
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

#[derive(Default)]
struct LatestFrame {
    generation: u64,
    frame: Vec<u8>,
    received_at: Option<Instant>,
}

struct FrameContext {
    latest: Mutex<LatestFrame>,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    stats: Arc<Mutex<StreamStats>>,
}

struct NativeFrameSource {
    #[cfg(target_os = "macos")]
    handle: *mut c_void,
    context: Arc<FrameContext>,
}

impl NativeFrameSource {
    #[cfg(target_os = "macos")]
    fn start(config: &ServeConfig) -> anyhow::Result<Self> {
        let developer_dir = developer_dir()?;
        let developer_dir = CString::new(developer_dir)?;
        let udid = CString::new(config.udid.clone())?;
        let context = Arc::new(FrameContext {
            latest: Mutex::new(LatestFrame::default()),
            stats: config.stats.clone(),
        });
        let raw_context = Arc::into_raw(context.clone()) as *mut c_void;
        let mut error: *mut c_char = ptr::null_mut();
        let handle = unsafe {
            simx_frame_stream_start(
                developer_dir.as_ptr(),
                udid.as_ptr(),
                config.quality,
                native_frame_callback,
                raw_context,
                &mut error,
            )
        };
        unsafe {
            let _ = Arc::from_raw(raw_context as *const FrameContext);
        }
        if handle.is_null() {
            let message = unsafe {
                if error.is_null() {
                    "native framebuffer bridge failed".to_string()
                } else {
                    let message = CStr::from_ptr(error).to_string_lossy().into_owned();
                    simx_bridge_free_string(error);
                    message
                }
            };
            bail!("{message}");
        }
        Ok(Self { handle, context })
    }

    #[cfg(not(target_os = "macos"))]
    fn start(config: &ServeConfig) -> anyhow::Result<Self> {
        let _ = config;
        bail!("streaming requires macOS private Simulator APIs");
    }

    fn latest_frame_after(&self, generation: u64) -> Option<(u64, Vec<u8>, Instant)> {
        let latest = self.context.latest.lock().ok()?;
        if latest.generation <= generation || latest.frame.is_empty() {
            return None;
        }
        Some((
            latest.generation,
            latest.frame.clone(),
            latest.received_at.unwrap_or_else(Instant::now),
        ))
    }

    fn handle_input(&self, text: &str) -> anyhow::Result<Vec<String>> {
        let value: serde_json::Value = serde_json::from_str(text)?;
        let result = match value.get("type").and_then(|value| value.as_str()) {
            Some("touch") => {
                let nx = value
                    .get("nx")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0);
                let ny = value
                    .get("ny")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0);
                let phase = value
                    .get("phase")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let down = matches!(phase, "began" | "moved");
                self.send_touch(nx, ny, down)
            }
            Some("swipe") | Some("drag") => self.send_drag_or_swipe(&value),
            Some("key") => {
                let phase = value
                    .get("phase")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let down = phase == "down";
                let code = value
                    .get("code")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                if let Some(key_code) = browser_code_to_hid(code) {
                    self.send_key_with_modifiers(key_code, down, &value)
                } else {
                    Ok(())
                }
            }
            Some("paste") => {
                let text = value
                    .get("text")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                self.send_text(text)
            }
            Some("button")
                if value.get("button").and_then(|value| value.as_str()) == Some("home") =>
            {
                self.press_home()
            }
            _ => Ok(()),
        };
        result?;
        Ok(input_ack(text, true, "ok").into_iter().collect())
    }

    fn send_drag_or_swipe(&self, value: &serde_json::Value) -> anyhow::Result<()> {
        let from = value.get("from").unwrap_or(value);
        let to = value.get("to").unwrap_or(value);
        let from_x = from
            .get("nx")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.5);
        let from_y = from
            .get("ny")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.5);
        let to_x = to
            .get("nx")
            .and_then(|value| value.as_f64())
            .unwrap_or(from_x);
        let to_y = to
            .get("ny")
            .and_then(|value| value.as_f64())
            .unwrap_or(from_y);
        let steps = value
            .get("steps")
            .and_then(|value| value.as_u64())
            .unwrap_or(8)
            .clamp(2, 60);
        self.send_touch(from_x, from_y, true)?;
        for step in 1..steps {
            let ratio = step as f64 / steps as f64;
            self.send_touch(
                from_x + (to_x - from_x) * ratio,
                from_y + (to_y - from_y) * ratio,
                true,
            )?;
            thread::sleep(Duration::from_millis(8));
        }
        self.send_touch(to_x, to_y, false)
    }

    fn send_key_with_modifiers(
        &self,
        key_code: u16,
        down: bool,
        value: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let modifiers = modifier_key_codes(value);
        if down {
            for modifier in &modifiers {
                self.send_key(*modifier, true)?;
            }
            self.send_key(key_code, true)
        } else {
            self.send_key(key_code, false)?;
            for modifier in modifiers.iter().rev() {
                self.send_key(*modifier, false)?;
            }
            Ok(())
        }
    }

    fn send_text(&self, text: &str) -> anyhow::Result<()> {
        for ch in text.chars() {
            let Some((key_code, shift)) = char_to_hid(ch) else {
                continue;
            };
            if shift {
                self.send_key(0xe1, true)?;
            }
            self.send_key(key_code, true)?;
            self.send_key(key_code, false)?;
            if shift {
                self.send_key(0xe1, false)?;
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn send_touch(&self, nx: f64, ny: f64, down: bool) -> anyhow::Result<()> {
        let mut error: *mut c_char = ptr::null_mut();
        let ok = unsafe { simx_hid_touch(self.handle, nx, ny, i32::from(down), &mut error) };
        native_bool_result(ok, error)
    }

    #[cfg(not(target_os = "macos"))]
    fn send_touch(&self, nx: f64, ny: f64, down: bool) -> anyhow::Result<()> {
        let _ = (nx, ny, down);
        bail!("HID input requires macOS private Simulator APIs");
    }

    #[cfg(target_os = "macos")]
    fn send_key(&self, key_code: u16, down: bool) -> anyhow::Result<()> {
        let mut error: *mut c_char = ptr::null_mut();
        let ok = unsafe { simx_hid_key(self.handle, key_code, i32::from(down), &mut error) };
        native_bool_result(ok, error)
    }

    #[cfg(not(target_os = "macos"))]
    fn send_key(&self, key_code: u16, down: bool) -> anyhow::Result<()> {
        let _ = (key_code, down);
        bail!("HID input requires macOS private Simulator APIs");
    }

    #[cfg(target_os = "macos")]
    fn press_home(&self) -> anyhow::Result<()> {
        let mut error: *mut c_char = ptr::null_mut();
        let ok = unsafe { simx_hid_home(self.handle, &mut error) };
        native_bool_result(ok, error)
    }

    #[cfg(not(target_os = "macos"))]
    fn press_home(&self) -> anyhow::Result<()> {
        bail!("HID input requires macOS private Simulator APIs");
    }
}

#[cfg(target_os = "macos")]
impl Drop for NativeFrameSource {
    fn drop(&mut self) {
        unsafe { simx_frame_stream_stop(self.handle) };
    }
}

#[cfg(target_os = "macos")]
extern "C" fn native_frame_callback(bytes: *const c_uchar, length: c_ulong, context: *mut c_void) {
    if bytes.is_null() || context.is_null() || length == 0 {
        return;
    }
    let context = unsafe { &*(context as *const FrameContext) };
    if let Ok(mut latest) = context.latest.lock() {
        let frame = unsafe { std::slice::from_raw_parts(bytes, length as usize) };
        let now = Instant::now();
        latest.frame.clear();
        latest.frame.extend_from_slice(frame);
        latest.generation = latest.generation.saturating_add(1);
        latest.received_at = Some(now);
        if let Ok(mut stats) = context.stats.lock() {
            if stats.started_at.is_none() {
                stats.started_at = Some(now);
            }
            stats.source_frames = stats.source_frames.saturating_add(1);
            stats.last_frame_bytes = frame.len();
            stats.last_source_at = Some(now);
            push_sample(&mut stats.source_samples, now, Duration::from_secs(5));
        }
    }
}

fn input_ack(text: &str, ok: bool, message: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    if value.get("ack").and_then(|value| value.as_bool()) != Some(true) {
        return None;
    }
    let id = value.get("id").cloned().unwrap_or(serde_json::Value::Null);
    Some(
        serde_json::json!({
            "type": "ack",
            "id": id,
            "ok": ok,
            "message": message
        })
        .to_string(),
    )
}

fn modifier_key_codes(value: &serde_json::Value) -> Vec<u16> {
    let Some(modifiers) = value.get("modifiers") else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    if modifiers
        .get("control")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        keys.push(0xe0);
    }
    if modifiers
        .get("shift")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        keys.push(0xe1);
    }
    if modifiers
        .get("option")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        keys.push(0xe2);
    }
    if modifiers
        .get("command")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        keys.push(0xe3);
    }
    keys
}

fn char_to_hid(ch: char) -> Option<(u16, bool)> {
    match ch {
        'a'..='z' => Some((((ch as u8 - b'a') + 0x04) as u16, false)),
        'A'..='Z' => Some((((ch as u8 - b'A') + 0x04) as u16, true)),
        '1' => Some((0x1e, false)),
        '2' => Some((0x1f, false)),
        '3' => Some((0x20, false)),
        '4' => Some((0x21, false)),
        '5' => Some((0x22, false)),
        '6' => Some((0x23, false)),
        '7' => Some((0x24, false)),
        '8' => Some((0x25, false)),
        '9' => Some((0x26, false)),
        '0' => Some((0x27, false)),
        ' ' => Some((0x2c, false)),
        '\n' => Some((0x28, false)),
        '-' => Some((0x2d, false)),
        '_' => Some((0x2d, true)),
        '=' => Some((0x2e, false)),
        '+' => Some((0x2e, true)),
        ',' => Some((0x36, false)),
        '<' => Some((0x36, true)),
        '.' => Some((0x37, false)),
        '>' => Some((0x37, true)),
        '/' => Some((0x38, false)),
        '?' => Some((0x38, true)),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn developer_dir() -> anyhow::Result<String> {
    if let Ok(value) = std::env::var("DEVELOPER_DIR") {
        if !value.trim().is_empty() {
            return Ok(value);
        }
    }
    let output = std::process::Command::new("/usr/bin/xcode-select")
        .arg("-p")
        .output()
        .context("failed to run xcode-select -p")?;
    if !output.status.success() {
        bail!(
            "xcode-select -p failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

#[cfg(target_os = "macos")]
fn native_bool_result(ok: i32, error: *mut c_char) -> anyhow::Result<()> {
    if ok != 0 {
        return Ok(());
    }
    let message = unsafe {
        if error.is_null() {
            "native HID bridge failed".to_string()
        } else {
            let message = CStr::from_ptr(error).to_string_lossy().into_owned();
            simx_bridge_free_string(error);
            message
        }
    };
    bail!("{message}");
}

fn browser_code_to_hid(code: &str) -> Option<u16> {
    match code {
        "KeyA" => Some(0x04),
        "KeyB" => Some(0x05),
        "KeyC" => Some(0x06),
        "KeyD" => Some(0x07),
        "KeyE" => Some(0x08),
        "KeyF" => Some(0x09),
        "KeyG" => Some(0x0a),
        "KeyH" => Some(0x0b),
        "KeyI" => Some(0x0c),
        "KeyJ" => Some(0x0d),
        "KeyK" => Some(0x0e),
        "KeyL" => Some(0x0f),
        "KeyM" => Some(0x10),
        "KeyN" => Some(0x11),
        "KeyO" => Some(0x12),
        "KeyP" => Some(0x13),
        "KeyQ" => Some(0x14),
        "KeyR" => Some(0x15),
        "KeyS" => Some(0x16),
        "KeyT" => Some(0x17),
        "KeyU" => Some(0x18),
        "KeyV" => Some(0x19),
        "KeyW" => Some(0x1a),
        "KeyX" => Some(0x1b),
        "KeyY" => Some(0x1c),
        "KeyZ" => Some(0x1d),
        "Digit1" => Some(0x1e),
        "Digit2" => Some(0x1f),
        "Digit3" => Some(0x20),
        "Digit4" => Some(0x21),
        "Digit5" => Some(0x22),
        "Digit6" => Some(0x23),
        "Digit7" => Some(0x24),
        "Digit8" => Some(0x25),
        "Digit9" => Some(0x26),
        "Digit0" => Some(0x27),
        "Enter" => Some(0x28),
        "Escape" => Some(0x29),
        "Backspace" => Some(0x2a),
        "Tab" => Some(0x2b),
        "Space" => Some(0x2c),
        "Minus" => Some(0x2d),
        "Equal" => Some(0x2e),
        "BracketLeft" => Some(0x2f),
        "BracketRight" => Some(0x30),
        "Backslash" => Some(0x31),
        "Semicolon" => Some(0x33),
        "Quote" => Some(0x34),
        "Backquote" => Some(0x35),
        "Comma" => Some(0x36),
        "Period" => Some(0x37),
        "Slash" => Some(0x38),
        "ArrowRight" => Some(0x4f),
        "ArrowLeft" => Some(0x50),
        "ArrowDown" => Some(0x51),
        "ArrowUp" => Some(0x52),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    fn simx_frame_stream_start(
        developer_dir: *const c_char,
        udid: *const c_char,
        quality: f32,
        callback: extern "C" fn(*const c_uchar, c_ulong, *mut c_void),
        callback_context: *mut c_void,
        error: *mut *mut c_char,
    ) -> *mut c_void;
    fn simx_frame_stream_stop(handle: *mut c_void);
    fn simx_bridge_free_string(value: *mut c_char);
    fn simx_hid_touch(
        handle: *mut c_void,
        nx: f64,
        ny: f64,
        down: i32,
        error: *mut *mut c_char,
    ) -> i32;
    fn simx_hid_key(handle: *mut c_void, key_code: u16, down: i32, error: *mut *mut c_char) -> i32;
    fn simx_hid_home(handle: *mut c_void, error: *mut *mut c_char) -> i32;
}

fn request_path(request: &str) -> Option<&str> {
    let first = request.lines().next()?;
    let mut parts = first.split_whitespace();
    let _method = parts.next()?;
    parts.next()
}

fn is_ws_upgrade(request: &str) -> bool {
    header_value(request, "upgrade")
        .map(|value| value.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
}

fn header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key.eq_ignore_ascii_case(name) {
            Some(value.trim())
        } else {
            None
        }
    })
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    Ok(())
}

fn write_http_redirect(stream: &mut TcpStream, location: &str) -> anyhow::Result<()> {
    write!(
        stream,
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    Ok(())
}

fn write_ws_upgrade(stream: &mut TcpStream, key: &str) -> anyhow::Result<()> {
    let accept = websocket_accept(key);
    write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    )?;
    Ok(())
}

fn websocket_accept(key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

fn write_ws_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> anyhow::Result<()> {
    let mut header = Vec::with_capacity(10);
    header.push(0x80 | opcode);
    match payload.len() {
        len @ 0..=125 => header.push(len as u8),
        len @ 126..=65535 => {
            header.push(126);
            header.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            header.push(127);
            header.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    stream.write_all(&header)?;
    stream.write_all(payload)?;
    Ok(())
}

fn write_ws_text(stream: &mut TcpStream, text: &str) -> anyhow::Result<()> {
    write_ws_frame(stream, 0x1, text.as_bytes())
}

enum WsEvent {
    Text(String),
    Ping(Vec<u8>),
    Close,
}

fn read_ws_events(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Vec<WsEvent> {
    let mut chunk = [0; 4096];
    let mut read_buffer = Vec::new();
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read_buffer.extend_from_slice(&chunk[..read]),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                break
            }
            Err(_) => break,
        }
    }

    let mut events = Vec::new();
    if !read_buffer.is_empty() {
        buffer.extend_from_slice(&read_buffer);
    }
    while let Some(event) = parse_next_ws_event(buffer) {
        events.push(event);
    }
    events
}

fn parse_next_ws_event(buffer: &mut Vec<u8>) -> Option<WsEvent> {
    if buffer.len() < 2 {
        return None;
    }

    let first = buffer[0];
    let second = buffer[1];
    let opcode = first & 0x0f;
    let masked = (second & 0x80) != 0;
    let length_code = (second & 0x7f) as usize;
    let mut offset = 2;
    let payload_len = match length_code {
        126 => {
            if buffer.len() < 4 {
                return None;
            }
            offset = 4;
            u16::from_be_bytes([buffer[2], buffer[3]]) as usize
        }
        127 => {
            if buffer.len() < 10 {
                return None;
            }
            offset = 10;
            u64::from_be_bytes([
                buffer[2], buffer[3], buffer[4], buffer[5], buffer[6], buffer[7], buffer[8],
                buffer[9],
            ]) as usize
        }
        len => len,
    };

    let mask_len = if masked { 4 } else { 0 };
    let frame_len = offset + mask_len + payload_len;
    if buffer.len() < frame_len {
        return None;
    }

    let mask = if masked {
        Some([
            buffer[offset],
            buffer[offset + 1],
            buffer[offset + 2],
            buffer[offset + 3],
        ])
    } else {
        None
    };
    let payload_start = offset + mask_len;
    let mut payload = buffer[payload_start..payload_start + payload_len].to_vec();
    if let Some(mask) = mask {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
    }
    buffer.drain(0..frame_len);

    match opcode {
        0x1 => String::from_utf8(payload).ok().map(WsEvent::Text),
        0x8 => Some(WsEvent::Close),
        0x9 => Some(WsEvent::Ping(payload)),
        _ => None,
    }
}

fn is_resume_message(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(|value| value.as_str())
                .map(|message_type| message_type == "resume")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{
        acquire_controller, char_to_hid, input_ack, is_resume_message, lease_is_active,
        parse_next_ws_event, slug_path, slug_path_slash, stats_path, stream_path, websocket_accept,
        ServeConfig, StreamStats, WsEvent,
    };
    use tempfile::TempDir;

    #[test]
    fn computes_websocket_accept_header() {
        assert_eq!(
            websocket_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn builds_slug_scoped_stream_path() {
        assert_eq!(stream_path("slug-a"), "/slug-a/stream");
        assert_eq!(stats_path("slug-a"), "/slug-a/stats");
    }

    #[test]
    fn builds_slug_viewer_paths() {
        assert_eq!(slug_path("slug-a"), "/slug-a");
        assert_eq!(slug_path_slash("slug-a"), "/slug-a/");
    }

    #[test]
    fn parses_masked_client_resume_text_frame() {
        let mut frame = masked_text_frame(r#"{"type":"resume"}"#);
        let event = parse_next_ws_event(&mut frame);
        match event {
            Some(WsEvent::Text(text)) => assert!(is_resume_message(&text)),
            _ => panic!("expected text event"),
        }
        assert!(frame.is_empty());
    }

    #[test]
    fn input_ack_preserves_message_id_and_status() {
        let ack = input_ack(r#"{"type":"paste","id":"msg-1","ack":true}"#, true, "ok").unwrap();
        assert!(ack.contains(r#""type":"ack""#));
        assert!(ack.contains(r#""id":"msg-1""#));
        assert!(ack.contains(r#""ok":true"#));
        assert!(input_ack(r#"{"type":"paste","id":"msg-1"}"#, true, "ok").is_none());
    }

    #[test]
    fn paste_character_mapping_marks_shifted_characters() {
        assert_eq!(char_to_hid('m'), Some((0x10, false)));
        assert_eq!(char_to_hid('M'), Some((0x10, true)));
        assert_eq!(char_to_hid('?'), Some((0x38, true)));
    }

    #[test]
    fn only_one_client_acquires_controller_role() {
        let config = ServeConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            quality: 0.7,
            fps: 120,
            idle_timeout: Duration::from_secs(300),
            slug: "browser".to_string(),
            udid: "UDID-1".to_string(),
            state_path: TempDir::new().unwrap().path().join("pool.json"),
            stats: Arc::new(Mutex::new(StreamStats::default())),
            controllers: Arc::new(Mutex::new(None)),
        };
        assert!(acquire_controller(&config, 1));
        assert!(!acquire_controller(&config, 2));
    }

    #[test]
    fn lease_check_reaps_expired_serve_lease() {
        let temp = TempDir::new().unwrap();
        let state_path = temp.path().join("pool.json");
        std::fs::write(
            &state_path,
            r#"{
  "version": 1,
  "size": 1,
  "device_type_id": "device",
  "runtime_id": "runtime",
  "devices": [
    {
      "name": "simx-pool-001",
      "udid": "UDID-1",
      "lease_id": "browser",
      "lease_expires_at": 1
    }
  ]
}
"#,
        )
        .unwrap();
        let config = ServeConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            quality: 0.7,
            fps: 120,
            idle_timeout: Duration::from_secs(300),
            slug: "browser".to_string(),
            udid: "UDID-1".to_string(),
            state_path: state_path.clone(),
            stats: Arc::new(Mutex::new(StreamStats::default())),
            controllers: Arc::new(Mutex::new(None)),
        };

        assert!(!lease_is_active(&config).unwrap());
        let raw = std::fs::read_to_string(state_path).unwrap();
        assert!(raw.contains(r#""lease_id": null"#));
        assert!(raw.contains(r#""lease_expires_at": null"#));
    }

    fn masked_text_frame(text: &str) -> Vec<u8> {
        let payload = text.as_bytes();
        let mask = [1_u8, 2, 3, 4];
        let mut frame = vec![0x81, 0x80 | payload.len() as u8];
        frame.extend_from_slice(&mask);
        for (index, byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[index % 4]);
        }
        frame
    }
}
