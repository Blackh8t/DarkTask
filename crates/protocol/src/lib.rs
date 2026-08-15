use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollRequest {
    pub enrollment_token: String,
    pub hostname: String,
    pub platform: String,
    pub arch: String,
    pub agent_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollResponse {
    pub device_id: Uuid,
    pub device_token: String,
    pub heartbeat_interval_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHello {
    pub device_id: Uuid,
    pub device_token: String,
    pub hostname: String,
    pub agent_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub device_id: Uuid,
    pub unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentToServer {
    Hello(AgentHello),
    Heartbeat(Heartbeat),
    SessionAccepted { session_id: Uuid },
    SessionRejected { session_id: Uuid, reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ServerToAgent {
    HelloAck,
    StartSession {
        session_id: Uuid,
        controller_id: String,
        session_token: String,
    },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSummary {
    pub device_id: Uuid,
    pub hostname: String,
    pub platform: String,
    pub arch: String,
    pub agent_version: String,
    pub online: bool,
    pub last_seen_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRequest {
    pub controller_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResponse {
    pub session_id: Uuid,
    pub session_token: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ControlMessage {
    MouseMove { x_norm: f32, y_norm: f32 },
    MouseButton { button: MouseButton, down: bool },
    MouseWheel { delta: i32 },
    Key { vk: u16, down: bool },
    SetQuality { jpeg_quality: u8, max_fps: u16 },
    Ping { unix_ms: u64 },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Binary frame format used by the v0.3 relay transport.
/// [0..4]  magic = b"RPF1"
/// [4..8]  width u32 LE
/// [8..12] height u32 LE
/// [12]    pixel format (1 = BGRA8)
/// [13]    compression (1 = zstd)
/// [14..16] reserved
/// [16..]  compressed BGRA bytes
pub const FRAME_MAGIC: &[u8; 4] = b"RPF1";
pub const FRAME_HEADER_LEN: usize = 16;
