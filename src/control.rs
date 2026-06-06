use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

#[cfg(target_os = "macos")]
use std::ffi::{c_char, c_uchar, c_ulong, c_void, CStr, CString};
#[cfg(target_os = "macos")]
use std::ptr;
#[cfg(target_os = "macos")]
use std::sync::{Arc, Condvar, Mutex};

use anyhow::{bail, Context};
use base64::Engine;
use serde::Serialize;
use sha1::{Digest, Sha1};

#[derive(Debug, Clone)]
pub struct ControlTarget {
    pub slug: String,
    pub udid: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotMetadata {
    pub ok: bool,
    pub slug: String,
    pub udid: String,
    pub source: &'static str,
    pub format: &'static str,
    pub width: Option<u16>,
    pub height: Option<u16>,
    pub bytes: usize,
    pub sha1: String,
    pub estimated_base64_chars: usize,
    pub estimated_base64_tokens: usize,
    pub estimated_metadata_tokens: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotOutput {
    #[serde(flatten)]
    pub metadata: SnapshotMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base64: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ControlAckOutput {
    pub ok: bool,
    pub slug: String,
    pub udid: String,
    pub source: &'static str,
    pub command: String,
    pub ack: serde_json::Value,
}

#[derive(Debug)]
pub struct SnapshotOptions<'a> {
    pub output: Option<&'a Path>,
    pub inline_base64: bool,
    pub wait_timeout: Duration,
}

pub trait HidTarget {
    fn send_touch(&self, nx: f64, ny: f64, down: bool) -> anyhow::Result<()>;
    fn send_key(&self, key_code: u16, down: bool) -> anyhow::Result<()>;
    fn press_home(&self) -> anyhow::Result<()>;
}

pub fn capture_snapshot(
    target: &ControlTarget,
    options: SnapshotOptions<'_>,
) -> anyhow::Result<SnapshotOutput> {
    let frame = capture_native_snapshot(&target.udid, options.wait_timeout)?;
    let metadata = snapshot_metadata(target, &frame);
    let path = if let Some(path) = options.output {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
        }
        fs::write(path, &frame).with_context(|| format!("failed to write {}", path.display()))?;
        Some(path.display().to_string())
    } else {
        None
    };
    let base64 = options
        .inline_base64
        .then(|| base64::engine::general_purpose::STANDARD.encode(frame.as_slice()));
    Ok(SnapshotOutput {
        metadata,
        path,
        base64,
    })
}

pub fn send_control_message(
    target: &ControlTarget,
    command: &str,
    message: serde_json::Value,
    wait_timeout: Duration,
) -> anyhow::Result<ControlAckOutput> {
    let mut outputs = send_control_messages(target, command, vec![message], wait_timeout)?;
    outputs
        .pop()
        .with_context(|| format!("{command} did not return an acknowledgement"))
}

pub fn send_control_messages(
    target: &ControlTarget,
    command: &str,
    messages: Vec<serde_json::Value>,
    wait_timeout: Duration,
) -> anyhow::Result<Vec<ControlAckOutput>> {
    let session = NativeHidSession::start(&target.udid, wait_timeout)?;
    let mut outputs = Vec::with_capacity(messages.len());
    for message in messages {
        let message = ensure_ack(message, command);
        let text = serde_json::to_string(&message)?;
        let acks = handle_hid_input(&session, &text)?;
        if acks.is_empty() {
            outputs.push(ControlAckOutput {
                ok: true,
                slug: target.slug.clone(),
                udid: target.udid.clone(),
                source: "native-hid",
                command: command.to_string(),
                ack: serde_json::json!({
                    "type": "ack",
                    "id": message.get("id").cloned().unwrap_or(serde_json::Value::Null),
                    "ok": true,
                    "message": "ok"
                }),
            });
            continue;
        }
        for ack in acks {
            let ack = serde_json::from_str::<serde_json::Value>(&ack)?;
            outputs.push(ControlAckOutput {
                ok: ack
                    .get("ok")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
                slug: target.slug.clone(),
                udid: target.udid.clone(),
                source: "native-hid",
                command: command.to_string(),
                ack,
            });
        }
    }
    Ok(outputs)
}

pub fn handle_hid_input(target: &impl HidTarget, text: &str) -> anyhow::Result<Vec<String>> {
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
            target.send_touch(nx, ny, down)
        }
        Some("swipe") | Some("drag") => send_drag_or_swipe(target, &value),
        Some("longPressScroll") | Some("long_press_scroll") => {
            send_long_press_scroll(target, &value)
        }
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
                send_key_with_modifiers(target, key_code, down, &value)
            } else {
                bail!("unsupported KeyboardEvent.code: {code}")
            }
        }
        Some("paste") => {
            let text = value
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            send_text(target, text)
        }
        Some("button") if value.get("button").and_then(|value| value.as_str()) == Some("home") => {
            target.press_home()
        }
        _ => Ok(()),
    };
    result?;
    Ok(input_ack(text, true, "ok").into_iter().collect())
}

pub fn touch_message(phase: &str, nx: f64, ny: f64) -> serde_json::Value {
    serde_json::json!({
        "type": "touch",
        "id": "simx-control-touch",
        "ack": true,
        "phase": phase,
        "nx": nx,
        "ny": ny,
        "pressure": if phase == "ended" || phase == "cancelled" { 0 } else { 1 }
    })
}

pub fn point_gesture_message(
    message_type: &str,
    from_nx: f64,
    from_ny: f64,
    to_nx: f64,
    to_ny: f64,
    steps: Option<u32>,
) -> serde_json::Value {
    serde_json::json!({
        "type": message_type,
        "id": format!("simx-control-{message_type}"),
        "ack": true,
        "from": { "nx": from_nx, "ny": from_ny },
        "to": { "nx": to_nx, "ny": to_ny },
        "steps": steps
    })
}

pub fn key_message(code: &str, phase: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "key",
        "id": format!("simx-control-key-{phase}"),
        "ack": true,
        "phase": phase,
        "code": code,
        "key": "",
        "repeat": false
    })
}

pub fn paste_message(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "paste",
        "id": "simx-control-paste",
        "ack": true,
        "text": text
    })
}

pub fn button_message(button: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "button",
        "id": format!("simx-control-button-{button}"),
        "ack": true,
        "button": button
    })
}

fn ensure_ack(mut message: serde_json::Value, command: &str) -> serde_json::Value {
    if let Some(object) = message.as_object_mut() {
        object.insert("ack".to_string(), serde_json::Value::Bool(true));
        object
            .entry("id".to_string())
            .or_insert_with(|| serde_json::Value::String(format!("simx-control-{command}")));
    }
    message
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

fn send_drag_or_swipe(target: &impl HidTarget, value: &serde_json::Value) -> anyhow::Result<()> {
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
    target.send_touch(from_x, from_y, true)?;
    for step in 1..steps {
        let ratio = step as f64 / steps as f64;
        target.send_touch(
            from_x + (to_x - from_x) * ratio,
            from_y + (to_y - from_y) * ratio,
            true,
        )?;
        thread::sleep(Duration::from_millis(8));
    }
    target.send_touch(to_x, to_y, false)
}

fn send_long_press_scroll(
    target: &impl HidTarget,
    value: &serde_json::Value,
) -> anyhow::Result<()> {
    let plan = long_press_scroll_plan(value);
    target.send_touch(plan.start_nx, plan.start_ny, true)?;
    thread::sleep(plan.hold);
    for step in 1..plan.steps {
        let ratio = step as f64 / plan.steps as f64;
        target.send_touch(
            plan.start_nx + (plan.end_nx - plan.start_nx) * ratio,
            plan.start_ny + (plan.end_ny - plan.start_ny) * ratio,
            true,
        )?;
        thread::sleep(Duration::from_millis(8));
    }
    target.send_touch(plan.end_nx, plan.end_ny, false)
}

fn send_key_with_modifiers(
    target: &impl HidTarget,
    key_code: u16,
    down: bool,
    value: &serde_json::Value,
) -> anyhow::Result<()> {
    let modifiers = modifier_key_codes(value);
    if down {
        for modifier in &modifiers {
            target.send_key(*modifier, true)?;
        }
        target.send_key(key_code, true)
    } else {
        target.send_key(key_code, false)?;
        for modifier in modifiers.iter().rev() {
            target.send_key(*modifier, false)?;
        }
        Ok(())
    }
}

fn send_text(target: &impl HidTarget, text: &str) -> anyhow::Result<()> {
    for ch in text.chars() {
        let Some((key_code, shift)) = char_to_hid(ch) else {
            continue;
        };
        if shift {
            target.send_key(0xe1, true)?;
        }
        target.send_key(key_code, true)?;
        target.send_key(key_code, false)?;
        if shift {
            target.send_key(0xe1, false)?;
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
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

pub fn char_to_hid(ch: char) -> Option<(u16, bool)> {
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

pub fn browser_code_to_hid(code: &str) -> Option<u16> {
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
        "CapsLock" => Some(0x39),
        "F1" => Some(0x3a),
        "F2" => Some(0x3b),
        "F3" => Some(0x3c),
        "F4" => Some(0x3d),
        "F5" => Some(0x3e),
        "F6" => Some(0x3f),
        "F7" => Some(0x40),
        "F8" => Some(0x41),
        "F9" => Some(0x42),
        "F10" => Some(0x43),
        "F11" => Some(0x44),
        "F12" => Some(0x45),
        "ArrowRight" => Some(0x4f),
        "ArrowLeft" => Some(0x50),
        "ArrowDown" => Some(0x51),
        "ArrowUp" => Some(0x52),
        _ => None,
    }
}

#[derive(Debug, PartialEq)]
pub struct LongPressScrollPlan {
    pub start_nx: f64,
    pub start_ny: f64,
    pub end_nx: f64,
    pub end_ny: f64,
    pub hold: Duration,
    pub steps: u64,
}

pub fn long_press_scroll_plan(value: &serde_json::Value) -> LongPressScrollPlan {
    let direction = value
        .get("direction")
        .and_then(|value| value.as_str())
        .unwrap_or("up");
    let distance = value
        .get("distance")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.5)
        .clamp(0.05, 1.0);
    let default_x = match direction {
        "left" => 1.0 - (distance / 2.0),
        "right" => distance / 2.0,
        _ => 0.5,
    };
    let default_y = match direction {
        "down" => distance / 2.0,
        "left" | "right" => 0.5,
        _ => 1.0 - (distance / 2.0),
    };
    let at = value.get("at").unwrap_or(value);
    let start_nx = at
        .get("nx")
        .and_then(|value| value.as_f64())
        .unwrap_or(default_x)
        .clamp(0.0, 1.0);
    let start_ny = at
        .get("ny")
        .and_then(|value| value.as_f64())
        .unwrap_or(default_y)
        .clamp(0.0, 1.0);
    let (delta_x, delta_y) = match direction {
        "down" => (0.0, distance),
        "left" => (-distance, 0.0),
        "right" => (distance, 0.0),
        _ => (0.0, -distance),
    };
    let hold_ms = value
        .get("holdMs")
        .or_else(|| value.get("hold_ms"))
        .and_then(|value| value.as_u64())
        .unwrap_or(500)
        .clamp(0, 3_000);
    let steps = value
        .get("steps")
        .and_then(|value| value.as_u64())
        .unwrap_or(12)
        .clamp(2, 60);

    LongPressScrollPlan {
        start_nx,
        start_ny,
        end_nx: (start_nx + delta_x).clamp(0.0, 1.0),
        end_ny: (start_ny + delta_y).clamp(0.0, 1.0),
        hold: Duration::from_millis(hold_ms),
        steps,
    }
}

fn snapshot_metadata(target: &ControlTarget, frame: &[u8]) -> SnapshotMetadata {
    let mut hasher = Sha1::new();
    hasher.update(frame);
    let sha1 = format!("{:x}", hasher.finalize());
    let (width, height) = jpeg_dimensions(frame).unwrap_or((None, None));
    let estimated_base64_chars = frame.len().div_ceil(3) * 4;
    let metadata_chars = 260 + target.slug.len() + target.udid.len() + sha1.len();
    SnapshotMetadata {
        ok: true,
        slug: target.slug.clone(),
        udid: target.udid.clone(),
        source: "native-snapshot",
        format: "jpeg",
        width,
        height,
        bytes: frame.len(),
        sha1,
        estimated_base64_chars,
        estimated_base64_tokens: estimate_tokens(estimated_base64_chars),
        estimated_metadata_tokens: estimate_tokens(metadata_chars),
    }
}

fn estimate_tokens(chars: usize) -> usize {
    chars.div_ceil(4)
}

fn duration_millis_i32(duration: Duration) -> i32 {
    duration.as_millis().clamp(1, i32::MAX as u128) as i32
}

fn jpeg_dimensions(bytes: &[u8]) -> anyhow::Result<(Option<u16>, Option<u16>)> {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        bail!("not a jpeg frame");
    }
    let mut index = 2;
    while index + 9 < bytes.len() {
        while index < bytes.len() && bytes[index] != 0xff {
            index += 1;
        }
        if index + 1 >= bytes.len() {
            break;
        }
        let marker = bytes[index + 1];
        index += 2;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if index + 2 > bytes.len() {
            break;
        }
        let length = u16::from_be_bytes([bytes[index], bytes[index + 1]]) as usize;
        if length < 2 || index + length > bytes.len() {
            break;
        }
        let is_sof = matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        );
        if is_sof && length >= 7 {
            let height = u16::from_be_bytes([bytes[index + 3], bytes[index + 4]]);
            let width = u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]);
            return Ok((Some(width), Some(height)));
        }
        index += length;
    }
    Ok((None, None))
}

#[cfg(target_os = "macos")]
struct NativeHidSession {
    handle: *mut c_void,
}

#[cfg(target_os = "macos")]
impl NativeHidSession {
    fn start(udid: &str, wait_timeout: Duration) -> anyhow::Result<Self> {
        let developer_dir = CString::new(developer_dir()?)?;
        let udid = CString::new(udid)?;
        let mut error: *mut c_char = ptr::null_mut();
        let hid_timeout_ms = duration_millis_i32(wait_timeout);
        let handle = unsafe {
            simx_frame_stream_start(
                developer_dir.as_ptr(),
                udid.as_ptr(),
                0.7,
                Some(native_noop_frame_callback),
                ptr::null_mut(),
                60,
                8 * 1000 * 1000,
                None,
                ptr::null_mut(),
                hid_timeout_ms,
                &mut error,
            )
        };
        if handle.is_null() {
            let message = native_error_message(error, "native HID bridge failed");
            bail!("{message}");
        }
        Ok(Self { handle })
    }
}

#[cfg(target_os = "macos")]
impl HidTarget for NativeHidSession {
    fn send_touch(&self, nx: f64, ny: f64, down: bool) -> anyhow::Result<()> {
        let mut error: *mut c_char = ptr::null_mut();
        let ok = unsafe { simx_hid_touch(self.handle, nx, ny, i32::from(down), &mut error) };
        native_bool_result(ok, error)
    }

    fn send_key(&self, key_code: u16, down: bool) -> anyhow::Result<()> {
        let mut error: *mut c_char = ptr::null_mut();
        let ok = unsafe { simx_hid_key(self.handle, key_code, i32::from(down), &mut error) };
        native_bool_result(ok, error)
    }

    fn press_home(&self) -> anyhow::Result<()> {
        let mut error: *mut c_char = ptr::null_mut();
        let ok = unsafe { simx_hid_home(self.handle, &mut error) };
        native_bool_result(ok, error)
    }
}

#[cfg(target_os = "macos")]
impl Drop for NativeHidSession {
    fn drop(&mut self) {
        unsafe { simx_frame_stream_stop(self.handle) };
    }
}

#[cfg(not(target_os = "macos"))]
struct NativeHidSession;

#[cfg(not(target_os = "macos"))]
impl NativeHidSession {
    fn start(udid: &str, wait_timeout: Duration) -> anyhow::Result<Self> {
        let _ = (udid, wait_timeout);
        bail!("HID input requires macOS private Simulator APIs");
    }
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct SnapshotState {
    frame: Option<Vec<u8>>,
}

#[cfg(target_os = "macos")]
fn capture_native_snapshot(udid: &str, wait_timeout: Duration) -> anyhow::Result<Vec<u8>> {
    let developer_dir = CString::new(developer_dir()?)?;
    let udid = CString::new(udid)?;
    let state = Arc::new((Mutex::new(SnapshotState::default()), Condvar::new()));
    let raw_context = Arc::into_raw(state.clone()) as *mut c_void;
    let mut error: *mut c_char = ptr::null_mut();
    let handle = unsafe {
        simx_frame_stream_start(
            developer_dir.as_ptr(),
            udid.as_ptr(),
            0.7,
            Some(native_snapshot_callback),
            raw_context,
            60,
            8 * 1000 * 1000,
            None,
            ptr::null_mut(),
            2000,
            &mut error,
        )
    };
    unsafe {
        let _ = Arc::from_raw(raw_context as *const (Mutex<SnapshotState>, Condvar));
    }
    if handle.is_null() {
        let message = native_error_message(error, "native snapshot bridge failed");
        bail!("{message}");
    }
    let frame = {
        let (lock, condvar) = &*state;
        let guard = lock
            .lock()
            .map_err(|_| anyhow::anyhow!("snapshot lock was poisoned"))?;
        let (guard, _) = condvar
            .wait_timeout_while(guard, wait_timeout, |state| state.frame.is_none())
            .map_err(|_| anyhow::anyhow!("snapshot lock was poisoned"))?;
        guard.frame.clone()
    };
    unsafe { simx_frame_stream_stop(handle) };
    frame.with_context(|| "timed out waiting for native simulator snapshot")
}

#[cfg(not(target_os = "macos"))]
fn capture_native_snapshot(udid: &str, wait_timeout: Duration) -> anyhow::Result<Vec<u8>> {
    let _ = (udid, wait_timeout);
    bail!("snapshot requires macOS private Simulator APIs");
}

#[cfg(target_os = "macos")]
extern "C" fn native_snapshot_callback(
    bytes: *const c_uchar,
    length: c_ulong,
    _encode_latency_ms: i64,
    context: *mut c_void,
) {
    if bytes.is_null() || context.is_null() {
        return;
    }
    let frame = unsafe { std::slice::from_raw_parts(bytes, length as usize).to_vec() };
    let state = unsafe { &*(context as *const (Mutex<SnapshotState>, Condvar)) };
    if let Ok(mut snapshot) = state.0.lock() {
        if snapshot.frame.is_none() {
            snapshot.frame = Some(frame);
            state.1.notify_all();
        }
    }
}

#[cfg(target_os = "macos")]
extern "C" fn native_noop_frame_callback(
    _bytes: *const c_uchar,
    _length: c_ulong,
    _encode_latency_ms: i64,
    _context: *mut c_void,
) {
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
    let message = native_error_message(error, "native HID bridge failed");
    bail!("{message}");
}

#[cfg(target_os = "macos")]
fn native_error_message(error: *mut c_char, fallback: &str) -> String {
    unsafe {
        if error.is_null() {
            fallback.to_string()
        } else {
            let message = CStr::from_ptr(error).to_string_lossy().into_owned();
            simx_bridge_free_string(error);
            message
        }
    }
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
    fn simx_hid_touch(
        handle: *mut c_void,
        nx: f64,
        ny: f64,
        down: i32,
        error: *mut *mut c_char,
    ) -> i32;
    fn simx_hid_key(handle: *mut c_void, key_code: u16, down: i32, error: *mut *mut c_char) -> i32;
    fn simx_hid_home(handle: *mut c_void, error: *mut *mut c_char) -> i32;
    fn simx_bridge_free_string(value: *mut c_char);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_dimensions_reads_sof_marker() {
        let jpeg = [
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x03,
            0x54, 0x01, 0x89, 0x03, 0x01, 0x11, 0x00, 0xff, 0xd9,
        ];
        assert_eq!(jpeg_dimensions(&jpeg).unwrap(), (Some(393), Some(852)));
    }

    #[test]
    fn snapshot_metadata_estimates_base64_tokens() {
        let target = ControlTarget {
            slug: "browser".to_string(),
            udid: "UDID".to_string(),
        };
        let frame = vec![0_u8; 1024];
        let metadata = snapshot_metadata(&target, &frame);
        assert_eq!(metadata.estimated_base64_chars, 1368);
        assert_eq!(metadata.estimated_base64_tokens, 342);
        assert!(metadata.estimated_metadata_tokens < metadata.estimated_base64_tokens);
    }

    #[test]
    fn paste_character_mapping_marks_shifted_characters() {
        assert_eq!(char_to_hid('m'), Some((0x10, false)));
        assert_eq!(char_to_hid('M'), Some((0x10, true)));
        assert_eq!(char_to_hid('?'), Some((0x38, true)));
    }

    #[test]
    fn long_press_scroll_defaults_drag_up_from_lower_screen() {
        let plan = long_press_scroll_plan(&serde_json::json!({}));
        assert_eq!(plan.start_nx, 0.5);
        assert_eq!(plan.start_ny, 0.75);
        assert_eq!(plan.end_nx, 0.5);
        assert_eq!(plan.end_ny, 0.25);
        assert_eq!(plan.hold, Duration::from_millis(500));
        assert_eq!(plan.steps, 12);
    }

    #[test]
    fn unsupported_key_code_returns_failed_ack() {
        struct NoopTarget;

        impl HidTarget for NoopTarget {
            fn send_touch(&self, _nx: f64, _ny: f64, _down: bool) -> anyhow::Result<()> {
                Ok(())
            }

            fn send_key(&self, _key_code: u16, _down: bool) -> anyhow::Result<()> {
                Ok(())
            }

            fn press_home(&self) -> anyhow::Result<()> {
                Ok(())
            }
        }

        let error = handle_hid_input(
            &NoopTarget,
            r#"{"type":"key","id":"bad-key","ack":true,"phase":"down","code":"KeyFoo"}"#,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "unsupported KeyboardEvent.code: KeyFoo");
    }

    #[test]
    fn hid_timeout_is_clamped_to_native_milliseconds() {
        assert_eq!(duration_millis_i32(Duration::ZERO), 1);
        assert_eq!(duration_millis_i32(Duration::from_millis(125)), 125);
        assert_eq!(duration_millis_i32(Duration::from_secs(u64::MAX)), i32::MAX);
    }
}
