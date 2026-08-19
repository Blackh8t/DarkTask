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
pub struct SessionPeek {
    /// A user is logged into the active console session.
    pub user_logged_in: bool,
    /// Seconds since last keyboard/mouse input on the console session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_secs: Option<u64>,
}

impl SessionPeek {
    /// User considered actively using the PC within this idle window.
    pub const ACTIVE_IDLE_SECS: u64 = 300;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub device_id: Uuid,
    pub unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_peek: Option<SessionPeek>,
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
        #[serde(default)]
        session_mode: SessionMode,
    },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSummary {
    pub device_id: Uuid,
    pub hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    pub platform: String,
    pub arch: String,
    pub agent_version: String,
    pub online: bool,
    pub last_seen_unix_ms: u64,
    /// Latest console session activity reported by the agent (online devices only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_peek: Option<SessionPeek>,
}

/// How the remote session attaches to the endpoint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    /// Shared interactive desktop — assist/support mode.
    #[default]
    UserScreen,
    /// Isolated admin session — not yet implemented on the agent.
    AdminWorkspace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRequest {
    pub controller_id: String,
    #[serde(default)]
    pub session_mode: SessionMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResponse {
    pub session_id: Uuid,
    pub session_token: String,
    pub status: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecialAction {
    CtrlAltDel,
    OpenCmd,
    OpenPowerShell,
    OpenPowerShellAdmin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ControlMessage {
    MouseMove { x_norm: f32, y_norm: f32 },
    MouseButton { button: MouseButton, down: bool },
    MouseWheel { delta: i32 },
    Key { vk: u16, down: bool },
    SetQuality { jpeg_quality: u8, max_fps: u16 },
    SpecialAction { action: SpecialAction },
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
/// [12]    pixel format (1 = BGRA8, 2 = JPEG, 3 = H264)
/// [13]    compression (1 = zstd, 2 = jpeg, 3 = h264)
/// [14]    jpeg quality 1-100, or h264 keyframe flag (1 = IDR with SPS/PPS, 0 = P)
/// [15]    reserved
/// [16..]  payload bytes
pub const FRAME_MAGIC: &[u8; 4] = b"RPF1";
pub const FRAME_HEADER_LEN: usize = 16;

pub const FRAME_PIXEL_BGRA8: u8 = 1;
pub const FRAME_PIXEL_JPEG: u8 = 2;
pub const FRAME_PIXEL_H264: u8 = 3;
pub const FRAME_COMPRESS_ZSTD: u8 = 1;
pub const FRAME_COMPRESS_JPEG: u8 = 2;
pub const FRAME_COMPRESS_H264: u8 = 3;
pub const FRAME_H264_DELTA: u8 = 0;
pub const FRAME_H264_KEY: u8 = 1;

/// Default stream settings tuned for bandwidth over fidelity.
pub const DEFAULT_JPEG_QUALITY: u8 = 32;
pub const DEFAULT_STREAM_FPS: u16 = 15;
pub const MAX_STREAM_FPS: u16 = 20;
/// Default max capture width on Windows (viewer stretches to fit).
pub const DEFAULT_CAPTURE_MAX_WIDTH: u32 = 800;
/// Hard upper bound for capture width from config or control messages.
pub const MAX_CAPTURE_WIDTH: u32 = 1920;
/// Android H.264 target bitrate (no audio).
pub const DEFAULT_H264_BITRATE: u32 = 1_000_000;
