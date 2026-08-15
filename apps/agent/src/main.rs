use anyhow::{anyhow, Context, Result};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use remote_protocol::{
    AgentHello, AgentToServer, ControlMessage, EnrollRequest, EnrollResponse, Heartbeat,
    MouseButton, ServerToAgent, FRAME_HEADER_LEN, FRAME_MAGIC,
};
use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};
use uuid::Uuid;

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

#[derive(Parser, Debug)]
#[command(version, about = "Minimal managed remote access agent")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8787")]
    server: String,
    #[arg(long, default_value = "dev-enroll")]
    enroll: String,
    #[arg(long)]
    hostname: Option<String>,
    #[arg(long)]
    reset_identity: bool,
    /// Maximum desktop capture FPS for the v0.3 relay transport.
    #[arg(long, default_value_t = 20)]
    max_fps: u16,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Identity {
    device_id: Uuid,
    device_token: String,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn identity_path() -> PathBuf {
    std::env::var_os("PROGRAMDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("RemotePlatform")
        .join("identity.json")
}

fn hostname(args: &Args) -> String {
    args.hostname
        .clone()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "unknown-host".into())
}

fn ws_url(server: &str, path: &str) -> String {
    let base = server.replace("https://", "wss://").replace("http://", "ws://");
    format!("{}{}", base.trim_end_matches('/'), path)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("remote_agent=info")
        .init();
    let args = Args::parse();
    let path = identity_path();
    if args.reset_identity && path.exists() {
        fs::remove_file(&path)?;
    }

    let identity = if path.exists() {
        serde_json::from_slice::<Identity>(&fs::read(&path)?)?
    } else {
        let client = reqwest::Client::new();
        let req = EnrollRequest {
            enrollment_token: args.enroll.clone(),
            hostname: hostname(&args),
            platform: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            agent_version: env!("CARGO_PKG_VERSION").into(),
        };
        let resp = client
            .post(format!("{}/api/v1/enroll", args.server.trim_end_matches('/')))
            .json(&req)
            .send()
            .await?
            .error_for_status()?
            .json::<EnrollResponse>()
            .await?;
        let id = Identity {
            device_id: resp.device_id,
            device_token: resp.device_token,
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, serde_json::to_vec_pretty(&id)?)?;
        id
    };

    loop {
        if let Err(e) = run_connection(&args, &identity).await {
            warn!(error = %e, "connection failed; retrying");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn run_connection(args: &Args, identity: &Identity) -> Result<()> {
    let url = ws_url(&args.server, "/ws/agent");
    let (ws, _) = connect_async(&url).await.context("connect websocket")?;
    let (mut tx, mut rx) = ws.split();

    let hello = AgentToServer::Hello(AgentHello {
        device_id: identity.device_id,
        device_token: identity.device_token.clone(),
        hostname: hostname(args),
        agent_version: env!("CARGO_PKG_VERSION").into(),
    });
    tx.send(Message::Text(serde_json::to_string(&hello)?.into()))
        .await?;

    let mut interval = tokio::time::interval(Duration::from_secs(10));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let hb = AgentToServer::Heartbeat(Heartbeat {
                    device_id: identity.device_id,
                    unix_ms: now_ms(),
                });
                tx.send(Message::Text(serde_json::to_string(&hb)?.into())).await?;
            }
            msg = rx.next() => {
                let Some(msg) = msg else { break };
                let msg = msg?;
                let Message::Text(text) = msg else { continue };
                let server_msg: ServerToAgent = serde_json::from_str(&text)?;
                match server_msg {
                    ServerToAgent::HelloAck => info!(device_id=%identity.device_id, "connected"),
                    ServerToAgent::StartSession { session_id, controller_id, session_token } => {
                        info!(%session_id, %controller_id, "starting remote session");
                        let reply = AgentToServer::SessionAccepted { session_id };
                        tx.send(Message::Text(serde_json::to_string(&reply)?.into())).await?;
                        let server = args.server.clone();
                        let max_fps = args.max_fps;
                        tokio::spawn(async move {
                            if let Err(e) = run_remote_session(server, session_id, session_token, max_fps).await {
                                warn!(%session_id, error=%e, "remote session ended");
                            }
                        });
                    }
                    ServerToAgent::Ping => {}
                }
            }
        }
    }
    Ok(())
}

async fn run_remote_session(server: String, session_id: Uuid, session_token: String, max_fps: u16) -> Result<()> {
    #[cfg(not(windows))]
    {
        let _ = (server, session_id, session_token, max_fps);
        return Err(anyhow!("desktop capture is implemented for Windows in v0.3"));
    }

    #[cfg(windows)]
    {
        let url = ws_url(
            &server,
            &format!("/ws/session/{session_id}?role=agent&token={session_token}"),
        );
        let (ws, _) = connect_async(&url).await.context("connect session websocket")?;
        let (mut tx, mut rx) = ws.split();
        let fps = max_fps.clamp(1, 60);
        let mut ticker = tokio::time::interval(Duration::from_millis(1000 / fps as u64));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let frame = tokio::task::spawn_blocking(capture_frame).await??;
                    tx.send(Message::Binary(frame.into())).await?;
                }
                msg = rx.next() => {
                    let Some(msg) = msg else { break };
                    match msg? {
                        Message::Text(text) => {
                            if let Ok(control) = serde_json::from_str::<ControlMessage>(&text) {
                                tokio::task::spawn_blocking(move || apply_control(control)).await??;
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn capture_frame() -> Result<Vec<u8>> {
    unsafe {
        let width = GetSystemMetrics(SM_CXSCREEN);
        let height = GetSystemMetrics(SM_CYSCREEN);
        if width <= 0 || height <= 0 {
            return Err(anyhow!("invalid desktop dimensions"));
        }

        let screen = GetDC(0);
        if screen == 0 { return Err(anyhow!("GetDC failed")); }
        let mem = CreateCompatibleDC(screen);
        if mem == 0 { ReleaseDC(0, screen); return Err(anyhow!("CreateCompatibleDC failed")); }
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        if bitmap == 0 {
            DeleteDC(mem); ReleaseDC(0, screen); return Err(anyhow!("CreateCompatibleBitmap failed"));
        }
        let old = SelectObject(mem, bitmap as _);
        if BitBlt(mem, 0, 0, width, height, screen, 0, 0, SRCCOPY | CAPTUREBLT) == 0 {
            SelectObject(mem, old); DeleteObject(bitmap as _); DeleteDC(mem); ReleaseDC(0, screen);
            return Err(anyhow!("BitBlt failed"));
        }

        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height; // top-down
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;

        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        let rows = GetDIBits(
            mem,
            bitmap,
            0,
            height as u32,
            pixels.as_mut_ptr() as *mut _,
            &mut info,
            DIB_RGB_COLORS,
        );

        SelectObject(mem, old);
        DeleteObject(bitmap as _);
        DeleteDC(mem);
        ReleaseDC(0, screen);

        if rows == 0 { return Err(anyhow!("GetDIBits failed")); }
        let compressed = zstd::stream::encode_all(&pixels[..], 1)?;
        let mut out = Vec::with_capacity(FRAME_HEADER_LEN + compressed.len());
        out.extend_from_slice(FRAME_MAGIC);
        out.extend_from_slice(&(width as u32).to_le_bytes());
        out.extend_from_slice(&(height as u32).to_le_bytes());
        out.push(1); // BGRA8
        out.push(1); // zstd
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&compressed);
        Ok(out)
    }
}

#[cfg(windows)]
fn apply_control(msg: ControlMessage) -> Result<()> {
    unsafe {
        match msg {
            ControlMessage::MouseMove { x_norm, y_norm } => {
                let w = GetSystemMetrics(SM_CXSCREEN).max(1);
                let h = GetSystemMetrics(SM_CYSCREEN).max(1);
                let x = (x_norm.clamp(0.0, 1.0) * (w - 1) as f32) as i32;
                let y = (y_norm.clamp(0.0, 1.0) * (h - 1) as f32) as i32;
                if SetCursorPos(x, y) == 0 { return Err(anyhow!("SetCursorPos failed")); }
            }
            ControlMessage::MouseButton { button, down } => {
                let flag = match (button, down) {
                    (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
                    (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
                    (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                    (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
                    (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                };
                mouse_event(flag, 0, 0, 0, 0);
            }
            ControlMessage::MouseWheel { delta } => mouse_event(MOUSEEVENTF_WHEEL, 0, 0, delta as u32, 0),
            ControlMessage::Key { vk, down } => {
                let flags = if down { 0 } else { KEYEVENTF_KEYUP };
                keybd_event(vk as u8, 0, flags, 0);
            }
            ControlMessage::SetQuality { .. } | ControlMessage::Ping { .. } => {}
        }
    }
    Ok(())
}
