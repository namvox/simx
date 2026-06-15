use std::collections::VecDeque;
#[cfg(target_os = "macos")]
use std::ffi::{c_char, c_uchar, c_ulong, c_void, CStr, CString};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
#[cfg(target_os = "macos")]
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264};
use webrtc::api::APIBuilder;
use webrtc::media::Sample;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;

#[cfg(target_os = "macos")]
use crate::control::toggle_simulator_soft_keyboard;
use crate::control::{handle_hid_input, HidTarget};
use crate::pool::PoolService;

const VIEWER_HTML: &str = include_str!("../viewer/index.html");
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const MAX_H264_DELIVERY_AGE: Duration = Duration::from_millis(120);
static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub host: String,
    pub port: u16,
    pub quality: f32,
    pub fps: u32,
    pub transport: StreamTransport,
    pub control_mode: StreamControlMode,
    pub idle_timeout: Duration,
    pub slug: String,
    pub udid: String,
    pub state_path: std::path::PathBuf,
    pub stats: Arc<Mutex<StreamStats>>,
    pub controllers: Arc<Mutex<Option<u64>>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StreamControlMode {
    #[default]
    ReadOnly,
    SingleController,
    Claim,
    Shared,
}

impl StreamControlMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::SingleController => "single-controller",
            Self::Claim => "claim",
            Self::Shared => "shared",
        }
    }

    fn client_role(self, is_controller: bool) -> &'static str {
        match self {
            Self::ReadOnly => "viewer",
            Self::SingleController if is_controller => "controller",
            Self::SingleController => "viewer",
            Self::Claim if is_controller => "controller",
            Self::Claim => "viewer",
            Self::Shared => "controller",
        }
    }

    fn can_send_input(self, config: &ServeConfig, client_id: u64, is_controller: bool) -> bool {
        match self {
            Self::ReadOnly => false,
            Self::SingleController => is_controller,
            Self::Claim => current_controller(config) == Some(client_id),
            Self::Shared => true,
        }
    }

    fn denied_message(self) -> &'static str {
        match self {
            Self::ReadOnly => "stream is read-only",
            Self::SingleController => "client is viewer-only",
            Self::Claim => "write permission has not been claimed",
            Self::Shared => "ok",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StreamTransport {
    #[default]
    Jpeg,
    H264,
    Webrtc,
}

impl StreamTransport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::H264 => "h264",
            Self::Webrtc => "webrtc",
        }
    }
}

#[derive(Debug, Serialize)]
struct Health<'a> {
    status: &'a str,
    slug: &'a str,
    udid: &'a str,
}

#[derive(Debug, Serialize)]
struct WebrtcPrototypeInfo<'a> {
    status: &'a str,
    slug: &'a str,
    transport: &'a str,
    viewer: String,
    signaling: String,
    hid: WebrtcHidInfo,
    media: WebrtcMediaInfo<'a>,
    incomplete: Vec<&'a str>,
    api_stability: &'a str,
}

#[derive(Debug, Serialize)]
struct WebrtcHidInfo {
    mode: &'static str,
    websocket: String,
}

#[derive(Debug, Serialize)]
struct WebrtcMediaInfo<'a> {
    codec: &'a str,
    source: &'a str,
    rtp_packetization: &'a str,
    clock_rate_hz: u32,
}

#[derive(Debug)]
struct WebrtcOfferSummary {
    sdp_bytes: usize,
    has_video_mline: bool,
    advertises_h264: bool,
}

#[derive(Debug, Deserialize)]
struct WebrtcOfferRequest {
    #[serde(rename = "type")]
    kind: String,
    sdp: String,
    hid: Option<String>,
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
    pub delivery_latency_ms_p50: Option<u128>,
    pub delivery_latency_ms_p95: Option<u128>,
    pub last_frame_age_ms: Option<u128>,
    pub last_send_age_ms: Option<u128>,
    pub last_delivery_latency_ms: Option<u128>,
    pub paused: bool,
    pub controller_connected: bool,
    pub webrtc_connection_state: Option<String>,
    pub webrtc_frames: u64,
    pub webrtc_bytes: u64,
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
    encode_latency_samples: VecDeque<u128>,
    #[serde(skip_serializing)]
    delivery_latency_samples: VecDeque<u128>,
}

pub fn serve(config: ServeConfig) -> anyhow::Result<()> {
    validate_config(&config)?;
    let frame_source: Arc<Mutex<Option<Arc<NativeFrameSource>>>> = Arc::new(Mutex::new(None));
    let h264_source: Arc<Mutex<Option<Arc<EncodedFrameSource>>>> = Arc::new(Mutex::new(None));
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
            Ok((stream, _peer_addr)) => {
                let config = config.clone();
                let frame_source = frame_source.clone();
                let h264_source = h264_source.clone();
                thread::spawn(move || {
                    if let Err(error) = handle_connection(stream, config, frame_source, h264_source)
                    {
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

fn handle_connection(
    mut stream: TcpStream,
    config: ServeConfig,
    frame_source: Arc<Mutex<Option<Arc<NativeFrameSource>>>>,
    h264_source: Arc<Mutex<Option<Arc<EncodedFrameSource>>>>,
) -> anyhow::Result<()> {
    stream.set_nonblocking(false)?;
    let request = read_http_request(&mut stream)?;
    let target = request_path(&request).unwrap_or("/");
    let path = target.split('?').next().unwrap_or(target);
    if is_ws_upgrade(&request) && path == stream_path(&config.slug) {
        let key = header_value(&request, "sec-websocket-key")
            .context("missing Sec-WebSocket-Key")?
            .to_string();
        let frame_source = frame_source_for(&config, &frame_source)?;
        write_ws_upgrade(&mut stream, &key)?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        stream.set_nonblocking(true)?;
        stream_frames(stream, config, frame_source)?;
        return Ok(());
    }
    if is_ws_upgrade(&request) && path == h264_stream_path(&config.slug) {
        let key = header_value(&request, "sec-websocket-key")
            .context("missing Sec-WebSocket-Key")?
            .to_string();
        let encoded_source = h264_source_for(&config, &h264_source)?;
        write_ws_upgrade(&mut stream, &key)?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        stream.set_nonblocking(true)?;
        stream_h264_frames(stream, config, encoded_source)?;
        return Ok(());
    }
    if request_method(&request) == Some("POST") && path == webrtc_offer_path(&config.slug) {
        let encoded_source = h264_source_for(&config, &h264_source)?;
        return handle_webrtc_offer(&mut stream, &config, encoded_source, &request);
    }

    match path {
        "/" => {
            let target = format!("/{}/", config.slug);
            write_http_redirect(&mut stream, &target)
        }
        path if path == slug_path(&config.slug) || path == slug_path_slash(&config.slug) => {
            let html = VIEWER_HTML
                .replace("__SIMX_SLUG__", &config.slug)
                .replace("__SIMX_TRANSPORT__", config.transport.as_str());
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
        path if path == webrtc_descriptor_path(&config.slug) => {
            let body = serde_json::to_vec(&webrtc_prototype_info(&config))?;
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

fn h264_stream_path(slug: &str) -> String {
    format!("/{slug}/h264-stream")
}

fn webrtc_descriptor_path(slug: &str) -> String {
    format!("/{slug}/webrtc")
}

fn webrtc_offer_path(slug: &str) -> String {
    format!("/{slug}/webrtc-offer")
}

fn webrtc_prototype_info(config: &ServeConfig) -> WebrtcPrototypeInfo<'_> {
    WebrtcPrototypeInfo {
        status: "experimental-loopback-video",
        slug: &config.slug,
        transport: "webrtc",
        viewer: format!(
            "http://{}:{}/{}?transport=webrtc",
            config.host, config.port, config.slug
        ),
        signaling: webrtc_offer_path(&config.slug),
        hid: WebrtcHidInfo {
            mode: "websocket",
            websocket: stream_path(&config.slug),
        },
        media: WebrtcMediaInfo {
            codec: "H.264/AVC",
            source: "VideoToolbox encoded frames from the existing experimental H.264 producer",
            rtp_packetization: "RFC 6184 packetization-mode=1, negotiated by SDP",
            clock_rate_hz: 90_000,
        },
        incomplete: vec![
            "Trickle ICE and TURN configuration",
            "Full RTCP feedback handling and bitrate adaptation",
            "WAN-shaped benchmark evidence",
            "Production readiness checks",
        ],
        api_stability: "Experimental; see docs/api-stability.md",
    }
}

fn handle_webrtc_offer(
    stream: &mut TcpStream,
    config: &ServeConfig,
    encoded_source: Arc<EncodedFrameSource>,
    request: &str,
) -> anyhow::Result<()> {
    match validate_webrtc_offer(http_body(request)) {
        Ok(summary) => {
            match start_webrtc_session(config.clone(), encoded_source, http_body(request), summary)
            {
                Ok(body) => write_http_response(stream, "200 OK", "application/json", &body),
                Err(error) => {
                    let body = serde_json::to_vec(&serde_json::json!({
                        "type": "webrtcPrototypeError",
                        "status": "answer-failed",
                        "error": error.to_string()
                    }))?;
                    write_http_response(
                        stream,
                        "500 Internal Server Error",
                        "application/json",
                        &body,
                    )
                }
            }
        }
        Err(message) => {
            let body = serde_json::to_vec(&serde_json::json!({
                "type": "webrtcPrototypeError",
                "status": "invalid-offer",
                "error": message
            }))?;
            write_http_response(stream, "400 Bad Request", "application/json", &body)
        }
    }
}

fn validate_webrtc_offer(body: &str) -> Result<WebrtcOfferSummary, String> {
    let offer: WebrtcOfferRequest =
        serde_json::from_str(body).map_err(|error| format!("invalid JSON offer: {error}"))?;
    if offer.kind != "offer" {
        return Err("offer type must be \"offer\"".to_string());
    }
    if let Some(hid) = offer.hid.as_deref() {
        if hid != "websocket" {
            return Err("WebRTC prototype keeps HID on the existing WebSocket path".to_string());
        }
    }
    let sdp = offer.sdp;
    if sdp.trim().is_empty() {
        return Err("offer sdp must not be empty".to_string());
    }
    let has_video_mline = sdp.lines().any(|line| line.starts_with("m=video "));
    if !has_video_mline {
        return Err("offer sdp must include a video m-line".to_string());
    }
    Ok(WebrtcOfferSummary {
        sdp_bytes: sdp.len(),
        has_video_mline,
        advertises_h264: sdp.to_ascii_uppercase().contains("H264"),
    })
}

fn start_webrtc_session(
    config: ServeConfig,
    encoded_source: Arc<EncodedFrameSource>,
    request_body: &str,
    summary: WebrtcOfferSummary,
) -> anyhow::Result<Vec<u8>> {
    let offer: WebrtcOfferRequest =
        serde_json::from_str(request_body).context("invalid WebRTC offer JSON")?;
    let (answer_tx, answer_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name(format!("simx-webrtc-{}", config.slug))
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = answer_tx.send(Err(anyhow::anyhow!(error)));
                    return;
                }
            };
            if let Err(error) = runtime.block_on(run_webrtc_session(
                config,
                encoded_source,
                offer,
                summary,
                answer_tx,
            )) {
                eprintln!("webrtc session error: {error:#}");
            }
        })
        .context("failed to spawn WebRTC session thread")?;
    answer_rx
        .recv_timeout(Duration::from_secs(10))
        .context("timed out waiting for WebRTC answer")?
}

async fn run_webrtc_session(
    config: ServeConfig,
    encoded_source: Arc<EncodedFrameSource>,
    offer: WebrtcOfferRequest,
    summary: WebrtcOfferSummary,
    answer_tx: mpsc::SyncSender<anyhow::Result<Vec<u8>>>,
) -> anyhow::Result<()> {
    let mut media_engine = MediaEngine::default();
    media_engine.register_default_codecs()?;
    let mut registry = webrtc::interceptor::registry::Registry::new();
    registry = register_default_interceptors(registry, &mut media_engine)?;
    let api = APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .build();
    let peer = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await?);
    let track = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: 90_000,
            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                .to_string(),
            ..Default::default()
        },
        "simx-video".to_string(),
        config.slug.clone(),
    ));
    let sender = peer
        .add_track(track.clone() as Arc<dyn TrackLocal + Send + Sync>)
        .await?;
    let rtcp_source = encoded_source.clone();
    tokio::spawn(async move {
        while let Ok((packets, _)) = sender.read_rtcp().await {
            if !packets.is_empty() {
                let _ = rtcp_source.request_keyframe();
            }
        }
    });
    let stats_config = config.clone();
    peer.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
        let stats_config = stats_config.clone();
        Box::pin(async move {
            update_stats(&stats_config, |stats| {
                stats.webrtc_connection_state = Some(state.to_string());
            });
        })
    }));

    peer.set_remote_description(RTCSessionDescription::offer(offer.sdp)?)
        .await?;
    let answer = peer.create_answer(None).await?;
    let mut gather_complete = peer.gathering_complete_promise().await;
    peer.set_local_description(answer).await?;
    let _ = gather_complete.recv().await;
    let local_description = peer
        .local_description()
        .await
        .ok_or_else(|| anyhow::anyhow!("missing local WebRTC description"))?;
    let body = serde_json::to_vec(&serde_json::json!({
        "type": "webrtcAnswer",
        "status": "ok",
        "slug": config.slug,
        "transport": "webrtc",
        "offer": {
            "sdpBytes": summary.sdp_bytes,
            "hasVideoMLine": summary.has_video_mline,
            "advertisesH264": summary.advertises_h264
        },
        "answer": {
            "type": "answer",
            "sdp": local_description.sdp
        },
        "hid": {
            "mode": "websocket",
            "websocket": stream_path(&config.slug)
        },
        "media": {
            "codec": "H.264/AVC",
            "source": "EncodedFrameSource::start_h264",
            "rtpPacketization": "RFC 6184 packetization-mode=1",
            "clockRateHz": 90000
        }
    }))?;
    answer_tx
        .send(Ok(body))
        .map_err(|_| anyhow::anyhow!("WebRTC answer receiver was dropped"))?;

    send_webrtc_h264_samples(config, encoded_source, track, peer).await
}

async fn send_webrtc_h264_samples(
    config: ServeConfig,
    encoded_source: Arc<EncodedFrameSource>,
    track: Arc<TrackLocalStaticSample>,
    peer: Arc<webrtc::peer_connection::RTCPeerConnection>,
) -> anyhow::Result<()> {
    let frame_interval = Duration::from_secs_f64(1.0 / config.fps.max(1) as f64);
    let mut last_sent_generation = 0_u64;
    let mut last_lease_check = Instant::now();
    if let Err(error) = encoded_source.request_keyframe() {
        eprintln!("webrtc keyframe request error: {error:#}");
    }
    loop {
        match peer.connection_state() {
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => break,
            _ => {}
        }
        if last_lease_check.elapsed() >= Duration::from_secs(1) {
            if !lease_is_active(&config)? {
                break;
            }
            last_lease_check = Instant::now();
        }
        if let Some(frame) = encoded_source.latest_frame_after(last_sent_generation) {
            if !frame.keyframe && frame.received_at.elapsed() > MAX_H264_DELIVERY_AGE {
                let _ = encoded_source.request_keyframe();
                tokio::time::sleep(frame_interval).await;
                continue;
            }
            let dropped = frame
                .generation
                .saturating_sub(last_sent_generation)
                .saturating_sub(1);
            last_sent_generation = frame.generation;
            let data = h264_sample_annex_b(&frame)?;
            let bytes = data.len();
            track
                .write_sample(&Sample {
                    data: data.into(),
                    timestamp: SystemTime::now(),
                    duration: frame_interval,
                    packet_timestamp: 0,
                    prev_dropped_packets: dropped.min(u16::MAX as u64) as u16,
                    prev_padding_packets: 0,
                })
                .await?;
            update_stats(&config, |stats| {
                let now = Instant::now();
                stats.sent_frames = stats.sent_frames.saturating_add(1);
                stats.dropped_frames = stats.dropped_frames.saturating_add(dropped);
                stats.last_frame_bytes = bytes;
                stats.last_sent_at = Some(now);
                stats.last_delivery_latency_ms =
                    Some(now.duration_since(frame.received_at).as_millis());
                stats.webrtc_frames = stats.webrtc_frames.saturating_add(1);
                stats.webrtc_bytes = stats.webrtc_bytes.saturating_add(bytes as u64);
                push_sample(&mut stats.sent_samples, now, Duration::from_secs(5));
                push_byte_sample(&mut stats.byte_samples, now, bytes, Duration::from_secs(5));
            });
        }
        tokio::time::sleep(frame_interval).await;
    }
    let _ = peer.close().await;
    Ok(())
}

fn h264_sample_annex_b(frame: &EncodedFrame) -> anyhow::Result<Vec<u8>> {
    let mut sample = Vec::with_capacity(
        frame.bytes.len()
            + frame
                .decoder_config
                .as_ref()
                .map(|config| config.len())
                .unwrap_or_default()
            + 16,
    );
    if frame.keyframe {
        if let Some(config) = &frame.decoder_config {
            append_avcc_decoder_config_annex_b(config, &mut sample);
        }
    }
    append_avcc_access_unit_annex_b(&frame.bytes, &mut sample)?;
    Ok(sample)
}

fn append_avcc_access_unit_annex_b(bytes: &[u8], output: &mut Vec<u8>) -> anyhow::Result<()> {
    let mut offset = 0;
    while offset + 4 <= bytes.len() {
        let length = u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        offset += 4;
        if length == 0 || offset + length > bytes.len() {
            bail!("invalid AVCC H.264 access unit");
        }
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(&bytes[offset..offset + length]);
        offset += length;
    }
    if offset != bytes.len() {
        bail!("trailing bytes in AVCC H.264 access unit");
    }
    Ok(())
}

fn append_avcc_decoder_config_annex_b(config: &[u8], output: &mut Vec<u8>) {
    if config.len() < 9 {
        return;
    }
    let sps_count_index = if config.get(5).map(|value| value & 0x1f) == Some(1) {
        5
    } else {
        6
    };
    let sps_count = config[sps_count_index] & 0x1f;
    let mut offset = sps_count_index + 1;
    for _ in 0..sps_count {
        if offset + 2 > config.len() {
            return;
        }
        let length = u16::from_be_bytes([config[offset], config[offset + 1]]) as usize;
        offset += 2;
        if length == 0 || offset + length > config.len() {
            return;
        }
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(&config[offset..offset + length]);
        offset += length;
    }
    if offset >= config.len() {
        return;
    }
    let pps_count = config[offset];
    offset += 1;
    for _ in 0..pps_count {
        if offset + 2 > config.len() {
            return;
        }
        let length = u16::from_be_bytes([config[offset], config[offset + 1]]) as usize;
        offset += 2;
        if length == 0 || offset + length > config.len() {
            return;
        }
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(&config[offset..offset + length]);
        offset += length;
    }
}

fn h264_frame_message(frame: &EncodedFrame) -> Vec<u8> {
    let mut message = Vec::with_capacity(28 + frame.bytes.len());
    message.extend_from_slice(b"SXH1");
    message.push(u8::from(frame.keyframe));
    message.extend_from_slice(&[0, 0, 0]);
    message.extend_from_slice(&frame.generation.to_be_bytes());
    message.extend_from_slice(&frame.pts_ms.to_be_bytes());
    message.extend_from_slice(&(frame.bytes.len() as u32).to_be_bytes());
    message.extend_from_slice(&frame.bytes);
    message
}

fn h264_codec_string(config_bytes: &[u8]) -> String {
    if config_bytes.len() >= 4 {
        format!(
            "avc1.{:02x}{:02x}{:02x}",
            config_bytes[1], config_bytes[2], config_bytes[3]
        )
    } else {
        "avc1.64002a".to_string()
    }
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

fn stream_frames(
    mut stream: TcpStream,
    config: ServeConfig,
    frame_source: Arc<NativeFrameSource>,
) -> anyhow::Result<()> {
    let client_id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
    let is_controller = config.control_mode == StreamControlMode::SingleController
        && acquire_controller(&config, client_id);
    let role = config.control_mode.client_role(is_controller);
    let frame_interval = Duration::from_secs_f64(1.0 / config.fps as f64);
    let mut last_lease_check = Instant::now();
    let mut last_activity = Instant::now();
    let mut paused = false;
    let mut input_buffer = Vec::new();
    let mut last_sent_generation = 0_u64;
    let mut next_frame_at = Instant::now();
    update_stats(&config, |stats| {
        stats.connected_clients = stats.connected_clients.saturating_add(1);
        stats.target_fps = config.fps;
        stats.paused = false;
        stats.controller_connected = match config.control_mode {
            StreamControlMode::ReadOnly => false,
            StreamControlMode::SingleController => stats.controller_connected || is_controller,
            StreamControlMode::Claim => stats.controller_connected,
            StreamControlMode::Shared => true,
        };
    });
    let _client_stats = ClientStatsGuard::new(
        config.stats.clone(),
        config.control_mode == StreamControlMode::Shared,
    );
    let _controller = ControllerGuard::new(
        config.controllers.clone(),
        config.stats.clone(),
        client_id,
        is_controller || config.control_mode == StreamControlMode::Claim,
    );
    write_ws_text(
        &mut stream,
        &format!(
            r#"{{"type":"client","role":"{role}","controlMode":"{}"}}"#,
            config.control_mode.as_str()
        ),
    )?;
    loop {
        let events = coalesce_touch_move_events(read_ws_events(&mut stream, &mut input_buffer));
        let mut should_close = false;
        for event in events {
            match event {
                WsEvent::Text(text) => {
                    if is_resume_message(&text) {
                        paused = false;
                        next_frame_at = Instant::now();
                        last_activity = Instant::now();
                        update_stats(&config, |stats| stats.paused = false);
                        write_ws_text(&mut stream, r#"{"type":"resumed"}"#)?;
                    } else if is_claim_control_message(&text)
                        && config.control_mode == StreamControlMode::Claim
                    {
                        claim_controller(&config, client_id);
                        last_activity = Instant::now();
                        write_client_message(&mut stream, &config, "controller", None)?;
                        if let Some(ack) = input_ack(&text, true, "ok") {
                            write_ws_text(&mut stream, &ack)?;
                        }
                    } else if config
                        .control_mode
                        .can_send_input(&config, client_id, is_controller)
                    {
                        last_activity = Instant::now();
                        match handle_hid_input(frame_source.as_ref(), &text) {
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
                        if let Some(ack) =
                            input_ack(&text, false, config.control_mode.denied_message())
                        {
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
            next_frame_at = Instant::now();
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
                stats.delivery_latency_samples.push_back(latency);
                while stats.delivery_latency_samples.len() > 240 {
                    stats.delivery_latency_samples.pop_front();
                }
            });
        }
        sleep_until_next_frame(&mut next_frame_at, frame_interval);
    }
    let _ = write_ws_frame(&mut stream, 0x8, &[]);
    Ok(())
}

fn h264_source_for(
    config: &ServeConfig,
    h264_source: &Arc<Mutex<Option<Arc<EncodedFrameSource>>>>,
) -> anyhow::Result<Arc<EncodedFrameSource>> {
    let mut source = h264_source
        .lock()
        .map_err(|_| anyhow::anyhow!("h264 source lock was poisoned"))?;
    if let Some(source) = source.as_ref() {
        return Ok(source.clone());
    }
    let created = Arc::new(EncodedFrameSource::start_h264(config, 16 * 1000 * 1000)?);
    *source = Some(created.clone());
    Ok(created)
}

fn frame_source_for(
    config: &ServeConfig,
    frame_source: &Arc<Mutex<Option<Arc<NativeFrameSource>>>>,
) -> anyhow::Result<Arc<NativeFrameSource>> {
    let mut source = frame_source
        .lock()
        .map_err(|_| anyhow::anyhow!("frame source lock was poisoned"))?;
    if let Some(source) = source.as_ref() {
        return Ok(source.clone());
    }
    let created = Arc::new(NativeFrameSource::start(config)?);
    *source = Some(created.clone());
    Ok(created)
}

fn stream_h264_frames(
    mut stream: TcpStream,
    config: ServeConfig,
    encoded_source: Arc<EncodedFrameSource>,
) -> anyhow::Result<()> {
    let client_id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
    let is_controller = config.control_mode == StreamControlMode::SingleController
        && acquire_controller(&config, client_id);
    let role = config.control_mode.client_role(is_controller);
    let frame_interval = Duration::from_secs_f64(1.0 / config.fps as f64);
    let mut last_lease_check = Instant::now();
    let mut last_activity = Instant::now();
    let mut paused = false;
    let mut input_buffer = Vec::new();
    let mut last_sent_generation = 0_u64;
    let mut last_config: Option<Vec<u8>> = None;
    let mut next_frame_at = Instant::now();
    update_stats(&config, |stats| {
        stats.connected_clients = stats.connected_clients.saturating_add(1);
        stats.target_fps = config.fps;
        stats.paused = false;
        stats.controller_connected = match config.control_mode {
            StreamControlMode::ReadOnly => false,
            StreamControlMode::SingleController => stats.controller_connected || is_controller,
            StreamControlMode::Claim => stats.controller_connected,
            StreamControlMode::Shared => true,
        };
    });
    let _client_stats = ClientStatsGuard::new(
        config.stats.clone(),
        config.control_mode == StreamControlMode::Shared,
    );
    let _controller = ControllerGuard::new(
        config.controllers.clone(),
        config.stats.clone(),
        client_id,
        is_controller || config.control_mode == StreamControlMode::Claim,
    );
    write_ws_text(
        &mut stream,
        &format!(
            r#"{{"type":"client","role":"{role}","transport":"h264","controlMode":"{}"}}"#,
            config.control_mode.as_str()
        ),
    )?;
    if let Err(error) = encoded_source.request_keyframe() {
        eprintln!("keyframe request error: {error:#}");
    }
    loop {
        let events = coalesce_touch_move_events(read_ws_events(&mut stream, &mut input_buffer));
        let mut should_close = false;
        for event in events {
            match event {
                WsEvent::Text(text) => {
                    if is_resume_message(&text) {
                        paused = false;
                        next_frame_at = Instant::now();
                        last_activity = Instant::now();
                        update_stats(&config, |stats| stats.paused = false);
                        if let Err(error) = encoded_source.request_keyframe() {
                            eprintln!("keyframe request error: {error:#}");
                        }
                        write_ws_text(&mut stream, r#"{"type":"resumed"}"#)?;
                    } else if is_keyframe_request_message(&text) {
                        last_activity = Instant::now();
                        let mut sent_cached_keyframe = false;
                        if let Some(frame) = encoded_source.latest_keyframe() {
                            send_h264_frame(
                                &mut stream,
                                &config,
                                &frame,
                                &mut last_config,
                                &mut last_sent_generation,
                            )?;
                            sent_cached_keyframe = true;
                            next_frame_at = Instant::now();
                        }
                        if let Err(error) = encoded_source.request_keyframe() {
                            if !sent_cached_keyframe {
                                if let Some(ack) = input_ack(&text, false, &error.to_string()) {
                                    write_ws_text(&mut stream, &ack)?;
                                }
                                eprintln!("keyframe request error: {error:#}");
                                continue;
                            }
                            eprintln!("keyframe request error: {error:#}");
                        }
                        if let Some(ack) = input_ack(&text, true, "ok") {
                            write_ws_text(&mut stream, &ack)?;
                        }
                    } else if is_claim_control_message(&text)
                        && config.control_mode == StreamControlMode::Claim
                    {
                        claim_controller(&config, client_id);
                        last_activity = Instant::now();
                        write_client_message(&mut stream, &config, "controller", Some("h264"))?;
                        if let Some(ack) = input_ack(&text, true, "ok") {
                            write_ws_text(&mut stream, &ack)?;
                        }
                    } else if config
                        .control_mode
                        .can_send_input(&config, client_id, is_controller)
                    {
                        last_activity = Instant::now();
                        match handle_hid_input(encoded_source.as_ref(), &text) {
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
                    } else if let Some(ack) =
                        input_ack(&text, false, config.control_mode.denied_message())
                    {
                        write_ws_text(&mut stream, &ack)?;
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
            next_frame_at = Instant::now();
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        if let Some(frame) = encoded_source.latest_frame_after(last_sent_generation) {
            if !frame.keyframe && frame.received_at.elapsed() > MAX_H264_DELIVERY_AGE {
                if let Err(error) = encoded_source.request_keyframe() {
                    eprintln!("keyframe request error: {error:#}");
                }
                sleep_until_next_frame(&mut next_frame_at, frame_interval);
                continue;
            }
            send_h264_frame(
                &mut stream,
                &config,
                &frame,
                &mut last_config,
                &mut last_sent_generation,
            )?;
        }
        sleep_until_next_frame(&mut next_frame_at, frame_interval);
    }
    let _ = write_ws_frame(&mut stream, 0x8, &[]);
    Ok(())
}

fn sleep_until_next_frame(next_frame_at: &mut Instant, frame_interval: Duration) {
    *next_frame_at += frame_interval;
    loop {
        let now = Instant::now();
        if *next_frame_at <= now {
            if now.duration_since(*next_frame_at) >= frame_interval {
                *next_frame_at = now;
            }
            return;
        }
        let remaining = *next_frame_at - now;
        if remaining > Duration::from_millis(2) {
            thread::sleep(remaining - Duration::from_millis(1));
        } else {
            thread::yield_now();
        }
    }
}

fn send_h264_frame(
    stream: &mut TcpStream,
    config: &ServeConfig,
    frame: &EncodedFrame,
    last_config: &mut Option<Vec<u8>>,
    last_sent_generation: &mut u64,
) -> anyhow::Result<()> {
    let dropped = frame
        .generation
        .saturating_sub(*last_sent_generation)
        .saturating_sub(1);
    *last_sent_generation = frame.generation;
    if frame.decoder_config.is_some() && frame.decoder_config != *last_config {
        last_config.clone_from(&frame.decoder_config);
        if let Some(config_bytes) = last_config {
            let config_message = serde_json::json!({
                "type": "h264Config",
                "codec": h264_codec_string(config_bytes),
                "description": base64::engine::general_purpose::STANDARD.encode(config_bytes),
            })
            .to_string();
            write_ws_text(stream, &config_message)?;
        }
    }

    let encoded_message = h264_frame_message(frame);
    write_ws_frame(stream, 0x2, &encoded_message)?;
    update_stats(config, |stats| {
        let now = Instant::now();
        stats.sent_frames = stats.sent_frames.saturating_add(1);
        stats.dropped_frames = stats.dropped_frames.saturating_add(dropped);
        stats.last_frame_bytes = encoded_message.len();
        stats.last_sent_at = Some(now);
        let latency = now.duration_since(frame.received_at).as_millis();
        stats.last_delivery_latency_ms = Some(latency);
        push_sample(&mut stats.sent_samples, now, Duration::from_secs(5));
        push_byte_sample(
            &mut stats.byte_samples,
            now,
            encoded_message.len(),
            Duration::from_secs(5),
        );
        stats.delivery_latency_samples.push_back(latency);
        while stats.delivery_latency_samples.len() > 240 {
            stats.delivery_latency_samples.pop_front();
        }
    });
    Ok(())
}

fn write_client_message(
    stream: &mut TcpStream,
    config: &ServeConfig,
    role: &str,
    transport: Option<&str>,
) -> anyhow::Result<()> {
    let transport_field = transport
        .map(|transport| format!(r#","transport":"{transport}""#))
        .unwrap_or_default();
    write_ws_text(
        stream,
        &format!(
            r#"{{"type":"client","role":"{role}"{transport_field},"controlMode":"{}"}}"#,
            config.control_mode.as_str()
        ),
    )
}

struct ClientStatsGuard {
    stats: Arc<Mutex<StreamStats>>,
    clear_controller_when_empty: bool,
}

impl ClientStatsGuard {
    fn new(stats: Arc<Mutex<StreamStats>>, clear_controller_when_empty: bool) -> Self {
        Self {
            stats,
            clear_controller_when_empty,
        }
    }
}

impl Drop for ClientStatsGuard {
    fn drop(&mut self) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.connected_clients = stats.connected_clients.saturating_sub(1);
            if stats.connected_clients == 0 {
                stats.paused = false;
                if self.clear_controller_when_empty {
                    stats.controller_connected = false;
                }
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

fn claim_controller(config: &ServeConfig, client_id: u64) {
    if let Ok(mut controller) = config.controllers.lock() {
        *controller = Some(client_id);
    }
    update_stats(config, |stats| {
        stats.controller_connected = true;
    });
}

fn current_controller(config: &ServeConfig) -> Option<u64> {
    config
        .controllers
        .lock()
        .ok()
        .and_then(|controller| *controller)
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
    let mut encode_latencies: Vec<_> = stats.encode_latency_samples.iter().copied().collect();
    encode_latencies.sort_unstable();
    stats.encode_latency_ms_p50 = percentile(&encode_latencies, 50);
    stats.encode_latency_ms_p95 = percentile(&encode_latencies, 95);
    let mut delivery_latencies: Vec<_> = stats.delivery_latency_samples.iter().copied().collect();
    delivery_latencies.sort_unstable();
    stats.delivery_latency_ms_p50 = percentile(&delivery_latencies, 50);
    stats.delivery_latency_ms_p95 = percentile(&delivery_latencies, 95);
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
        if buffer.len() > 16 * 1024 {
            bail!("request header too large");
        }
        if let Some(header_end) = find_header_end(&buffer) {
            let headers = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
            let content_length = content_length(&headers).unwrap_or(0);
            if content_length > 64 * 1024 {
                bail!("request body too large");
            }
            let expected = header_end + 4 + content_length;
            while buffer.len() < expected {
                let read = stream.read(&mut chunk)?;
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

#[derive(Default)]
struct LatestFrame {
    generation: u64,
    frame: Vec<u8>,
    received_at: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    pub generation: u64,
    pub bytes: Vec<u8>,
    pub keyframe: bool,
    pub pts_ms: i64,
    pub decoder_config: Option<Vec<u8>>,
    pub received_at: Instant,
}

#[derive(Default)]
struct LatestEncodedFrame {
    generation: u64,
    bytes: Vec<u8>,
    keyframe: bool,
    pts_ms: i64,
    decoder_config: Option<Vec<u8>>,
    received_at: Option<Instant>,
    frames: VecDeque<EncodedFrame>,
}

struct FrameContext {
    latest: Mutex<LatestFrame>,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    stats: Arc<Mutex<StreamStats>>,
}

struct EncodedFrameContext {
    latest: Mutex<LatestEncodedFrame>,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    stats: Arc<Mutex<StreamStats>>,
}

struct NativeFrameSource {
    #[cfg(target_os = "macos")]
    handle: *mut c_void,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    udid: String,
    context: Arc<FrameContext>,
}

pub struct EncodedFrameSource {
    #[cfg(target_os = "macos")]
    handle: *mut c_void,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    udid: String,
    context: Arc<EncodedFrameContext>,
}

// The native streamer handle is retained for the serve process lifetime. Frame
// data shared with client threads is guarded by `FrameContext::latest`, and HID
// calls go through SimulatorKit's asynchronous delivery API.
unsafe impl Send for NativeFrameSource {}
unsafe impl Sync for NativeFrameSource {}
unsafe impl Send for EncodedFrameSource {}
unsafe impl Sync for EncodedFrameSource {}

impl NativeFrameSource {
    #[cfg(test)]
    fn test_with_frame(
        stats: Arc<Mutex<StreamStats>>,
        generation: u64,
        frame: Vec<u8>,
        received_at: Instant,
    ) -> Self {
        Self {
            #[cfg(target_os = "macos")]
            handle: ptr::null_mut(),
            udid: "TEST-UDID".to_string(),
            context: Arc::new(FrameContext {
                latest: Mutex::new(LatestFrame {
                    generation,
                    frame,
                    received_at: Some(received_at),
                }),
                stats,
            }),
        }
    }

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
                Some(native_frame_callback),
                raw_context,
                config.fps as i32,
                8 * 1000 * 1000,
                None,
                ptr::null_mut(),
                2000,
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
        Ok(Self {
            handle,
            udid: config.udid.clone(),
            context,
        })
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

    #[cfg(target_os = "macos")]
    fn toggle_soft_keyboard(&self) -> anyhow::Result<()> {
        toggle_simulator_soft_keyboard(&self.udid)
    }

    #[cfg(not(target_os = "macos"))]
    fn toggle_soft_keyboard(&self) -> anyhow::Result<()> {
        bail!("HID input requires macOS private Simulator APIs");
    }
}

impl HidTarget for NativeFrameSource {
    fn send_touch(&self, nx: f64, ny: f64, down: bool) -> anyhow::Result<()> {
        NativeFrameSource::send_touch(self, nx, ny, down)
    }

    fn send_key(&self, key_code: u16, down: bool) -> anyhow::Result<()> {
        NativeFrameSource::send_key(self, key_code, down)
    }

    fn press_home(&self) -> anyhow::Result<()> {
        NativeFrameSource::press_home(self)
    }

    fn toggle_soft_keyboard(&self) -> anyhow::Result<()> {
        NativeFrameSource::toggle_soft_keyboard(self)
    }
}

impl EncodedFrameSource {
    #[cfg(target_os = "macos")]
    pub fn start_h264(config: &ServeConfig, bitrate: i32) -> anyhow::Result<Self> {
        let developer_dir = developer_dir()?;
        let developer_dir = CString::new(developer_dir)?;
        let udid = CString::new(config.udid.clone())?;
        let context = Arc::new(EncodedFrameContext {
            latest: Mutex::new(LatestEncodedFrame::default()),
            stats: config.stats.clone(),
        });
        let raw_context = Arc::into_raw(context.clone()) as *mut c_void;
        let mut error: *mut c_char = ptr::null_mut();
        let handle = unsafe {
            simx_frame_stream_start(
                developer_dir.as_ptr(),
                udid.as_ptr(),
                config.quality,
                None,
                ptr::null_mut(),
                config.fps as i32,
                bitrate,
                Some(native_encoded_frame_callback),
                raw_context,
                2000,
                &mut error,
            )
        };
        unsafe {
            let _ = Arc::from_raw(raw_context as *const EncodedFrameContext);
        }
        if handle.is_null() {
            let message = unsafe {
                if error.is_null() {
                    "native h264 stream bridge failed".to_string()
                } else {
                    let message = CStr::from_ptr(error).to_string_lossy().into_owned();
                    simx_bridge_free_string(error);
                    message
                }
            };
            bail!("{message}");
        }
        Ok(Self {
            handle,
            udid: config.udid.clone(),
            context,
        })
    }

    #[cfg(not(target_os = "macos"))]
    pub fn start_h264(config: &ServeConfig, bitrate: i32) -> anyhow::Result<Self> {
        let _ = (config, bitrate);
        bail!("h264 streaming requires macOS private Simulator APIs and VideoToolbox");
    }

    pub fn latest_frame_after(&self, generation: u64) -> Option<EncodedFrame> {
        let latest = self.context.latest.lock().ok()?;
        if latest.generation <= generation || (latest.bytes.is_empty() && latest.frames.is_empty())
        {
            return None;
        }
        if generation == 0 {
            if let Some(frame) = latest
                .frames
                .iter()
                .rev()
                .find(|frame| frame.keyframe)
                .cloned()
            {
                return Some(frame);
            }
        }
        if let Some(frame) = latest
            .frames
            .iter()
            .find(|frame| frame.generation == generation.saturating_add(1))
            .cloned()
        {
            if frame.received_at.elapsed() > MAX_H264_DELIVERY_AGE {
                if let Some(keyframe) = latest
                    .frames
                    .iter()
                    .rev()
                    .find(|frame| frame.generation > generation && frame.keyframe)
                    .cloned()
                {
                    return Some(keyframe);
                }
            }
            return Some(frame);
        }
        if let Some(frame) = latest
            .frames
            .iter()
            .find(|frame| frame.generation > generation && frame.keyframe)
            .cloned()
        {
            return Some(frame);
        }
        if latest.generation != generation.saturating_add(1) && !latest.keyframe && generation != 0
        {
            return None;
        }
        Some(EncodedFrame {
            generation: latest.generation,
            bytes: latest.bytes.clone(),
            keyframe: latest.keyframe,
            pts_ms: latest.pts_ms,
            decoder_config: latest.decoder_config.clone(),
            received_at: latest.received_at.unwrap_or_else(Instant::now),
        })
    }

    pub fn latest_keyframe(&self) -> Option<EncodedFrame> {
        let latest = self.context.latest.lock().ok()?;
        if let Some(frame) = latest
            .frames
            .iter()
            .rev()
            .find(|frame| frame.keyframe)
            .cloned()
        {
            return Some(frame);
        }
        if latest.keyframe && !latest.bytes.is_empty() {
            return Some(EncodedFrame {
                generation: latest.generation,
                bytes: latest.bytes.clone(),
                keyframe: true,
                pts_ms: latest.pts_ms,
                decoder_config: latest.decoder_config.clone(),
                received_at: latest.received_at.unwrap_or_else(Instant::now),
            });
        }
        None
    }

    fn request_keyframe(&self) -> anyhow::Result<()> {
        self.request_native_keyframe()
    }

    #[cfg(target_os = "macos")]
    fn request_native_keyframe(&self) -> anyhow::Result<()> {
        let mut error: *mut c_char = ptr::null_mut();
        let ok = unsafe { simx_stream_request_keyframe(self.handle, &mut error) };
        native_bool_result(ok, error)
    }

    #[cfg(not(target_os = "macos"))]
    fn request_native_keyframe(&self) -> anyhow::Result<()> {
        bail!("keyframe requests require macOS private Simulator APIs and VideoToolbox");
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

    #[cfg(target_os = "macos")]
    fn toggle_soft_keyboard(&self) -> anyhow::Result<()> {
        toggle_simulator_soft_keyboard(&self.udid)
    }

    #[cfg(not(target_os = "macos"))]
    fn toggle_soft_keyboard(&self) -> anyhow::Result<()> {
        bail!("HID input requires macOS private Simulator APIs");
    }
}

impl HidTarget for EncodedFrameSource {
    fn send_touch(&self, nx: f64, ny: f64, down: bool) -> anyhow::Result<()> {
        EncodedFrameSource::send_touch(self, nx, ny, down)
    }

    fn send_key(&self, key_code: u16, down: bool) -> anyhow::Result<()> {
        EncodedFrameSource::send_key(self, key_code, down)
    }

    fn press_home(&self) -> anyhow::Result<()> {
        EncodedFrameSource::press_home(self)
    }

    fn toggle_soft_keyboard(&self) -> anyhow::Result<()> {
        EncodedFrameSource::toggle_soft_keyboard(self)
    }
}

#[cfg(target_os = "macos")]
impl Drop for EncodedFrameSource {
    fn drop(&mut self) {
        unsafe { simx_frame_stream_stop(self.handle) };
    }
}

#[cfg(target_os = "macos")]
impl Drop for NativeFrameSource {
    fn drop(&mut self) {
        unsafe { simx_frame_stream_stop(self.handle) };
    }
}

#[cfg(target_os = "macos")]
extern "C" fn native_frame_callback(
    bytes: *const c_uchar,
    length: c_ulong,
    encode_latency_ms: i64,
    context: *mut c_void,
) {
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
            push_latency_sample(&mut stats.encode_latency_samples, encode_latency_ms);
        }
    }
}

#[cfg(target_os = "macos")]
extern "C" fn native_encoded_frame_callback(
    bytes: *const c_uchar,
    length: c_ulong,
    keyframe: i32,
    pts_ms: i64,
    config_bytes: *const c_uchar,
    config_length: c_ulong,
    encode_latency_ms: i64,
    context: *mut c_void,
) {
    if bytes.is_null() || context.is_null() || length == 0 {
        return;
    }
    let context = unsafe { &*(context as *const EncodedFrameContext) };
    if let Ok(mut latest) = context.latest.lock() {
        let frame = unsafe { std::slice::from_raw_parts(bytes, length as usize) };
        let now = Instant::now();
        let generation = latest.generation.saturating_add(1);
        let decoder_config = if config_bytes.is_null() || config_length == 0 {
            None
        } else {
            Some(unsafe {
                std::slice::from_raw_parts(config_bytes, config_length as usize).to_vec()
            })
        };
        latest.bytes.clear();
        latest.bytes.extend_from_slice(frame);
        latest.generation = generation;
        latest.keyframe = keyframe != 0;
        latest.pts_ms = pts_ms;
        latest.decoder_config.clone_from(&decoder_config);
        latest.received_at = Some(now);
        latest.frames.push_back(EncodedFrame {
            generation,
            bytes: frame.to_vec(),
            keyframe: keyframe != 0,
            pts_ms,
            decoder_config,
            received_at: now,
        });
        while latest.frames.len() > 240 {
            latest.frames.pop_front();
        }
        if let Ok(mut stats) = context.stats.lock() {
            if stats.started_at.is_none() {
                stats.started_at = Some(now);
            }
            stats.source_frames = stats.source_frames.saturating_add(1);
            stats.last_frame_bytes = frame.len();
            stats.last_source_at = Some(now);
            push_sample(&mut stats.source_samples, now, Duration::from_secs(5));
            push_latency_sample(&mut stats.encode_latency_samples, encode_latency_ms);
        }
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn push_latency_sample(samples: &mut VecDeque<u128>, latency_ms: i64) {
    if latency_ms < 0 {
        return;
    }
    samples.push_back(latency_ms as u128);
    while samples.len() > 240 {
        samples.pop_front();
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

fn is_keyframe_request_message(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(|value| value.as_str())
                .map(|message_type| message_type == "requestKeyframe")
        })
        .unwrap_or(false)
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

#[cfg(target_os = "macos")]
extern "C" {
    fn simx_frame_stream_start(
        developer_dir: *const c_char,
        udid: *const c_char,
        quality: f32,
        callback: Option<extern "C" fn(*const c_uchar, c_ulong, i64, *mut c_void)>,
        callback_context: *mut c_void,
        target_fps: i32,
        bitrate: i32,
        encoded_callback: Option<
            extern "C" fn(
                *const c_uchar,
                c_ulong,
                i32,
                i64,
                *const c_uchar,
                c_ulong,
                i64,
                *mut c_void,
            ),
        >,
        encoded_callback_context: *mut c_void,
        hid_timeout_ms: i32,
        error: *mut *mut c_char,
    ) -> *mut c_void;
    fn simx_frame_stream_stop(handle: *mut c_void);
    fn simx_bridge_free_string(value: *mut c_char);
    fn simx_stream_request_keyframe(handle: *mut c_void, error: *mut *mut c_char) -> i32;
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

fn request_method(request: &str) -> Option<&str> {
    request.lines().next()?.split_whitespace().next()
}

fn http_body(request: &str) -> &str {
    request
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or("")
}

fn content_length(headers: &str) -> Option<usize> {
    header_value(headers, "content-length")?.parse().ok()
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
    write_ws_bytes(stream, &header)?;
    write_ws_bytes(stream, payload)?;
    Ok(())
}

fn write_ws_bytes(stream: &mut TcpStream, mut bytes: &[u8]) -> anyhow::Result<()> {
    let started = Instant::now();
    while !bytes.is_empty() {
        match stream.write(bytes) {
            Ok(0) => bail!("websocket write returned zero bytes"),
            Ok(written) => bytes = &bytes[written..],
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if started.elapsed() >= Duration::from_secs(5) {
                    return Err(error).context("websocket write timed out");
                }
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error.into()),
        }
    }
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

fn coalesce_touch_move_events(events: Vec<WsEvent>) -> Vec<WsEvent> {
    let mut coalesced = Vec::with_capacity(events.len());
    let mut pending_move: Option<String> = None;
    for event in events {
        match event {
            WsEvent::Text(text) if is_unacknowledged_touch_move(&text) => {
                pending_move = Some(text);
            }
            event => {
                if let Some(text) = pending_move.take() {
                    coalesced.push(WsEvent::Text(text));
                }
                coalesced.push(event);
            }
        }
    }
    if let Some(text) = pending_move {
        coalesced.push(WsEvent::Text(text));
    }
    coalesced
}

fn is_unacknowledged_touch_move(text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    value.get("type").and_then(|value| value.as_str()) == Some("touch")
        && value.get("phase").and_then(|value| value.as_str()) == Some("moved")
        && value.get("ack").and_then(|value| value.as_bool()) != Some(true)
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

fn is_claim_control_message(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(|value| value.as_str())
                .map(|message_type| message_type == "claimControl")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::{
        acquire_controller, claim_controller, h264_codec_string, h264_sample_annex_b,
        h264_stream_path, input_ack, is_claim_control_message, is_keyframe_request_message,
        is_resume_message, is_unacknowledged_touch_move, lease_is_active, parse_next_ws_event,
        slug_path, slug_path_slash, stats_path, stream_path, validate_webrtc_offer,
        webrtc_descriptor_path, webrtc_offer_path, websocket_accept, EncodedFrame,
        EncodedFrameContext, EncodedFrameSource, LatestEncodedFrame, NativeFrameSource,
        ServeConfig, StreamControlMode, StreamStats, StreamTransport, WsEvent,
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
        assert_eq!(h264_stream_path("slug-a"), "/slug-a/h264-stream");
        assert_eq!(webrtc_descriptor_path("slug-a"), "/slug-a/webrtc");
        assert_eq!(webrtc_offer_path("slug-a"), "/slug-a/webrtc-offer");
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
    fn detects_only_unacknowledged_touch_moves_as_coalescible() {
        assert!(is_unacknowledged_touch_move(
            r#"{"type":"touch","phase":"moved","nx":0.1,"ny":0.2}"#
        ));
        assert!(!is_unacknowledged_touch_move(
            r#"{"type":"touch","phase":"moved","ack":true}"#
        ));
        assert!(!is_unacknowledged_touch_move(
            r#"{"type":"touch","phase":"began"}"#
        ));
        assert!(!is_unacknowledged_touch_move(
            r#"{"type":"key","phase":"down"}"#
        ));
    }

    #[test]
    fn detects_keyframe_request_messages() {
        assert!(is_keyframe_request_message(
            r#"{"type":"requestKeyframe","reason":"decode_error"}"#
        ));
        assert!(!is_keyframe_request_message(r#"{"type":"resume"}"#));
        assert!(!is_keyframe_request_message("not-json"));
    }

    #[test]
    fn detects_claim_control_messages() {
        assert!(is_claim_control_message(
            r#"{"type":"claimControl","ack":true}"#
        ));
        assert!(!is_claim_control_message(r#"{"type":"resume"}"#));
        assert!(!is_claim_control_message("not-json"));
    }

    #[test]
    fn validates_webrtc_video_offers_for_websocket_hid() {
        let offer = validate_webrtc_offer(
            r#"{
                "type": "offer",
                "sdp": "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=rtpmap:102 H264/90000\r\n",
                "hid": "websocket"
            }"#,
        )
        .unwrap();

        assert!(offer.has_video_mline);
        assert!(offer.advertises_h264);
        assert!(offer.sdp_bytes > 0);
    }

    #[test]
    fn rejects_webrtc_offers_that_move_hid_to_data_channel() {
        let error = validate_webrtc_offer(
            r#"{
                "type": "offer",
                "sdp": "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\n",
                "hid": "data-channel"
            }"#,
        )
        .unwrap_err();

        assert!(error.contains("WebSocket"));
    }

    #[test]
    fn rejects_webrtc_offers_without_video() {
        let error = validate_webrtc_offer(
            r#"{
                "type": "offer",
                "sdp": "v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n"
            }"#,
        )
        .unwrap_err();

        assert!(error.contains("video"));
    }

    #[test]
    fn converts_avcc_access_units_to_annex_b_samples() {
        let frame = EncodedFrame {
            generation: 1,
            bytes: vec![0, 0, 0, 3, 0x65, 0xaa, 0xbb, 0, 0, 0, 2, 0x41, 0xcc],
            keyframe: false,
            pts_ms: 0,
            decoder_config: None,
            received_at: Instant::now(),
        };

        let sample = h264_sample_annex_b(&frame).unwrap();

        assert_eq!(
            sample,
            vec![0, 0, 0, 1, 0x65, 0xaa, 0xbb, 0, 0, 0, 1, 0x41, 0xcc]
        );
    }

    #[test]
    fn prepends_decoder_config_for_webrtc_keyframes() {
        let frame = EncodedFrame {
            generation: 1,
            bytes: vec![0, 0, 0, 2, 0x65, 0xaa],
            keyframe: true,
            pts_ms: 0,
            decoder_config: Some(vec![
                1, 0x42, 0xe0, 0x1f, 0xff, 0xff, 0xe1, 0, 3, 0x67, 0x42, 0x00, 1, 0, 2, 0x68, 0xce,
            ]),
            received_at: Instant::now(),
        };

        let sample = h264_sample_annex_b(&frame).unwrap();

        assert_eq!(
            sample,
            vec![0, 0, 0, 1, 0x67, 0x42, 0x00, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0xaa,]
        );
    }

    #[test]
    fn coalesces_adjacent_touch_moves_but_preserves_boundaries() {
        let events = super::coalesce_touch_move_events(vec![
            WsEvent::Text(r#"{"type":"touch","phase":"began"}"#.to_string()),
            WsEvent::Text(r#"{"type":"touch","phase":"moved","nx":0.1}"#.to_string()),
            WsEvent::Text(r#"{"type":"touch","phase":"moved","nx":0.2}"#.to_string()),
            WsEvent::Ping(vec![1, 2, 3]),
            WsEvent::Text(r#"{"type":"touch","phase":"moved","nx":0.3,"ack":true}"#.to_string()),
            WsEvent::Text(r#"{"type":"touch","phase":"ended"}"#.to_string()),
        ]);

        let texts: Vec<_> = events
            .into_iter()
            .filter_map(|event| match event {
                WsEvent::Text(text) => Some(text),
                _ => None,
            })
            .collect();

        assert_eq!(
            texts,
            vec![
                r#"{"type":"touch","phase":"began"}"#,
                r#"{"type":"touch","phase":"moved","nx":0.2}"#,
                r#"{"type":"touch","phase":"moved","nx":0.3,"ack":true}"#,
                r#"{"type":"touch","phase":"ended"}"#,
            ]
        );
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
    fn only_one_client_acquires_controller_role() {
        let config = ServeConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            quality: 0.7,
            fps: 120,
            transport: StreamTransport::Jpeg,
            control_mode: StreamControlMode::SingleController,
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
    fn control_modes_authorize_input_as_expected() {
        let mut config = test_serve_config(StreamControlMode::Claim);
        assert!(!StreamControlMode::ReadOnly.can_send_input(&config, 1, true));
        assert!(!StreamControlMode::ReadOnly.can_send_input(&config, 1, false));
        assert!(StreamControlMode::SingleController.can_send_input(&config, 1, true));
        assert!(!StreamControlMode::SingleController.can_send_input(&config, 2, false));
        assert!(!StreamControlMode::Claim.can_send_input(&config, 1, false));
        claim_controller(&config, 2);
        assert!(!StreamControlMode::Claim.can_send_input(&config, 1, false));
        assert!(StreamControlMode::Claim.can_send_input(&config, 2, false));
        assert!(StreamControlMode::Shared.can_send_input(&config, 1, true));
        assert!(StreamControlMode::Shared.can_send_input(&config, 2, false));
        config.control_mode = StreamControlMode::ReadOnly;
    }

    #[test]
    fn control_modes_report_client_roles() {
        assert_eq!(StreamControlMode::ReadOnly.client_role(true), "viewer");
        assert_eq!(StreamControlMode::ReadOnly.client_role(false), "viewer");
        assert_eq!(
            StreamControlMode::SingleController.client_role(true),
            "controller"
        );
        assert_eq!(
            StreamControlMode::SingleController.client_role(false),
            "viewer"
        );
        assert_eq!(StreamControlMode::Claim.client_role(true), "controller");
        assert_eq!(StreamControlMode::Claim.client_role(false), "viewer");
        assert_eq!(StreamControlMode::Shared.client_role(true), "controller");
        assert_eq!(StreamControlMode::Shared.client_role(false), "controller");
    }

    #[test]
    fn shared_frame_source_allows_independent_client_cursors() {
        let received_at = Instant::now();
        let source = NativeFrameSource::test_with_frame(
            Arc::new(Mutex::new(StreamStats::default())),
            7,
            vec![1, 2, 3, 4],
            received_at,
        );

        let first_client = source.latest_frame_after(0).unwrap();
        let second_client = source.latest_frame_after(0).unwrap();

        assert_eq!(first_client.0, 7);
        assert_eq!(second_client.0, 7);
        assert_eq!(first_client.1, vec![1, 2, 3, 4]);
        assert_eq!(second_client.1, vec![1, 2, 3, 4]);
        assert_eq!(first_client.2, received_at);
        assert!(source.latest_frame_after(7).is_none());
    }

    #[test]
    fn encoded_frame_source_preserves_h264_metadata() {
        let received_at = Instant::now();
        let source = EncodedFrameSource {
            #[cfg(target_os = "macos")]
            handle: std::ptr::null_mut(),
            udid: "TEST-UDID".to_string(),
            context: Arc::new(EncodedFrameContext {
                latest: Mutex::new(LatestEncodedFrame {
                    generation: 3,
                    bytes: vec![0, 0, 0, 4, 0x65],
                    keyframe: true,
                    pts_ms: 42,
                    decoder_config: Some(vec![1, 100, 0, 42]),
                    received_at: Some(received_at),
                    frames: VecDeque::new(),
                }),
                stats: Arc::new(Mutex::new(StreamStats::default())),
            }),
        };

        let frame = source.latest_frame_after(2).unwrap();

        assert_eq!(frame.generation, 3);
        assert_eq!(frame.bytes, vec![0, 0, 0, 4, 0x65]);
        assert!(frame.keyframe);
        assert_eq!(frame.pts_ms, 42);
        assert_eq!(frame.decoder_config, Some(vec![1, 100, 0, 42]));
        assert_eq!(frame.received_at, received_at);
        assert!(source.latest_frame_after(3).is_none());
    }

    #[test]
    fn encoded_frame_source_prefers_contiguous_frames_and_recovers_at_keyframes() {
        let received_at = Instant::now();
        let mut frames = VecDeque::new();
        frames.push_back(EncodedFrame {
            generation: 10,
            bytes: vec![0x01],
            keyframe: true,
            pts_ms: 10,
            decoder_config: Some(vec![1, 100, 0, 42]),
            received_at,
        });
        frames.push_back(EncodedFrame {
            generation: 11,
            bytes: vec![0x02],
            keyframe: false,
            pts_ms: 11,
            decoder_config: None,
            received_at,
        });
        frames.push_back(EncodedFrame {
            generation: 20,
            bytes: vec![0x03],
            keyframe: true,
            pts_ms: 20,
            decoder_config: Some(vec![1, 100, 0, 42]),
            received_at,
        });
        let source = EncodedFrameSource {
            #[cfg(target_os = "macos")]
            handle: std::ptr::null_mut(),
            udid: "TEST-UDID".to_string(),
            context: Arc::new(EncodedFrameContext {
                latest: Mutex::new(LatestEncodedFrame {
                    generation: 20,
                    bytes: vec![0x03],
                    keyframe: true,
                    pts_ms: 20,
                    decoder_config: Some(vec![1, 100, 0, 42]),
                    received_at: Some(received_at),
                    frames,
                }),
                stats: Arc::new(Mutex::new(StreamStats::default())),
            }),
        };

        assert_eq!(source.latest_frame_after(10).unwrap().generation, 11);
        assert_eq!(source.latest_frame_after(12).unwrap().generation, 20);
        assert_eq!(source.latest_frame_after(0).unwrap().generation, 20);
    }

    #[test]
    fn encoded_frame_source_returns_latest_cached_keyframe() {
        let received_at = Instant::now();
        let mut frames = VecDeque::new();
        frames.push_back(EncodedFrame {
            generation: 10,
            bytes: vec![0x01],
            keyframe: true,
            pts_ms: 10,
            decoder_config: Some(vec![1, 0x42, 0xe0, 0x1f]),
            received_at,
        });
        frames.push_back(EncodedFrame {
            generation: 11,
            bytes: vec![0x02],
            keyframe: false,
            pts_ms: 11,
            decoder_config: None,
            received_at,
        });
        let source = EncodedFrameSource {
            #[cfg(target_os = "macos")]
            handle: std::ptr::null_mut(),
            udid: "TEST-UDID".to_string(),
            context: Arc::new(EncodedFrameContext {
                latest: Mutex::new(LatestEncodedFrame {
                    generation: 11,
                    bytes: vec![0x02],
                    keyframe: false,
                    pts_ms: 11,
                    decoder_config: None,
                    received_at: Some(received_at),
                    frames,
                }),
                stats: Arc::new(Mutex::new(StreamStats::default())),
            }),
        };

        let frame = source.latest_keyframe().unwrap();

        assert_eq!(frame.generation, 10);
        assert_eq!(frame.bytes, vec![0x01]);
        assert!(frame.keyframe);
        assert_eq!(frame.decoder_config, Some(vec![1, 0x42, 0xe0, 0x1f]));
    }

    #[test]
    fn h264_frame_message_packs_metadata_and_payload() {
        let frame = EncodedFrame {
            generation: 9,
            bytes: vec![0, 0, 0, 1, 0x65],
            keyframe: true,
            pts_ms: 1234,
            decoder_config: None,
            received_at: Instant::now(),
        };

        let message = super::h264_frame_message(&frame);

        assert_eq!(&message[0..4], b"SXH1");
        assert_eq!(message[4], 1);
        assert_eq!(u64::from_be_bytes(message[8..16].try_into().unwrap()), 9);
        assert_eq!(
            i64::from_be_bytes(message[16..24].try_into().unwrap()),
            1234
        );
        assert_eq!(u32::from_be_bytes(message[24..28].try_into().unwrap()), 5);
        assert_eq!(&message[28..], &[0, 0, 0, 1, 0x65]);
    }

    #[test]
    fn h264_codec_string_uses_avc_decoder_config_profile() {
        assert_eq!(h264_codec_string(&[1, 0x64, 0x00, 0x2a]), "avc1.64002a");
        assert_eq!(h264_codec_string(&[1, 0x42, 0xe0, 0x1f]), "avc1.42e01f");
        assert_eq!(h264_codec_string(&[]), "avc1.64002a");
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
            transport: StreamTransport::Jpeg,
            control_mode: StreamControlMode::ReadOnly,
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

    fn test_serve_config(control_mode: StreamControlMode) -> ServeConfig {
        ServeConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            quality: 0.7,
            fps: 120,
            transport: StreamTransport::Jpeg,
            control_mode,
            idle_timeout: Duration::from_secs(300),
            slug: "browser".to_string(),
            udid: "UDID-1".to_string(),
            state_path: TempDir::new().unwrap().path().join("pool.json"),
            stats: Arc::new(Mutex::new(StreamStats::default())),
            controllers: Arc::new(Mutex::new(None)),
        }
    }
}
