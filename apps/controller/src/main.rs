use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use minifb::{Key, KeyRepeat, MouseButton as MfMouseButton, MouseMode, Window, WindowOptions};
use remote_protocol::{
    ControlMessage, DeviceSummary, MouseButton, SessionMode, SessionRequest, SessionResponse,
    FRAME_COMPRESS_H264, FRAME_COMPRESS_JPEG, FRAME_COMPRESS_ZSTD, FRAME_HEADER_LEN, FRAME_MAGIC,
    FRAME_PIXEL_BGRA8, FRAME_PIXEL_H264, FRAME_PIXEL_JPEG,
};
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use openh264::nal_units;
use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

#[derive(Parser)]
#[command(version, about = "Remote Platform controller")]
struct Cli {
    #[arg(long, default_value = "http://127.0.0.1:8787")]
    server: String,
    #[arg(long, env = "REMOTE_ADMIN_TOKEN")]
    token: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Devices,
    Connect {
        device_id: Uuid,
        #[arg(long, default_value = "local-tech")]
        controller_id: String,
        /// `user_screen` (default) or `admin_workspace` (not yet supported on agent).
        #[arg(long, default_value = "user_screen")]
        session_mode: String,
    },
}

fn parse_session_mode(raw: &str) -> Result<SessionMode> {
    match raw {
        "user_screen" => Ok(SessionMode::UserScreen),
        "admin_workspace" => Ok(SessionMode::AdminWorkspace),
        _ => Err(anyhow!(
            "invalid session mode {raw:?}; expected user_screen or admin_workspace"
        )),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn ws_url(server: &str, path: &str) -> String {
    let base = server.replace("https://", "wss://").replace("http://", "ws://");
    format!("{}{}", base.trim_end_matches('/'), path)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = reqwest::Client::new();
    let auth = format!("Bearer {}", cli.token);
    let base = cli.server.trim_end_matches('/');

    match cli.command {
        Command::Devices => {
            let devices = client
                .get(format!("{base}/api/v1/devices"))
                .header("Authorization", &auth)
                .send()
                .await?
                .error_for_status()?
                .json::<Vec<DeviceSummary>>()
                .await?;
            println!("{:<38} {:<24} {:<8} {}", "DEVICE ID", "HOSTNAME", "STATE", "VERSION");
            for d in devices {
                println!(
                    "{:<38} {:<24} {:<8} {}",
                    d.device_id,
                    d.hostname,
                    if d.online { "online" } else { "offline" },
                    d.agent_version
                );
            }
        }
        Command::Connect {
            device_id,
            controller_id,
            session_mode,
        } => {
            let session_mode = parse_session_mode(&session_mode)?;
            let resp = client
                .post(format!("{base}/api/v1/devices/{device_id}/session"))
                .json(&SessionRequest {
                    controller_id,
                    session_mode,
                })
                .header("Authorization", &auth)
                .send()
                .await?
                .error_for_status()?
                .json::<SessionResponse>()
                .await?;
            println!("session {}: {}", resp.session_id, resp.status);
            run_viewer(cli.server, resp).await?;
        }
    }
    Ok(())
}

async fn run_viewer(server: String, session: SessionResponse) -> Result<()> {
    let url = ws_url(
        &server,
        &format!(
            "/ws/session/{}?role=controller&token={}",
            session.session_id, session.session_token
        ),
    );
    let (ws, _) = connect_async(&url).await.context("connect session websocket")?;
    let (mut sink, mut stream) = ws.split();

    let (frame_tx, mut frame_rx) = mpsc::channel(1);
    let latest_frame = Arc::new(Mutex::new(None::<Vec<u8>>));
    let latest_for_rx = latest_frame.clone();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel::<ControlMessage>();

    let receive_task = tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    *latest_for_rx.lock().unwrap() = Some(data.to_vec());
                    let _ = frame_tx.try_send(());
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });

    let send_task = tokio::spawn(async move {
        while let Some(control) = control_rx.recv().await {
            let text = match serde_json::to_string(&control) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if sink.send(Message::Text(text.into())).await.is_err() { break; }
        }
    });

    let viewer = tokio::task::spawn_blocking(move || -> Result<()> {
        frame_rx
            .blocking_recv()
            .ok_or_else(|| anyhow!("session closed before first frame"))?;
        let first = loop {
            let frame = latest_frame
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| anyhow!("session closed before first frame"))?;
            match decode_frame(&frame) {
                Ok(decoded) => break decoded,
                Err(_) => {
                    frame_rx
                        .blocking_recv()
                        .ok_or_else(|| anyhow!("session closed before first frame"))?;
                }
            }
        };
        let (mut width, mut height, mut pixels) = first;
        let mut window = Window::new(
            "Remote Platform v0.3",
            width,
            height,
            WindowOptions { resize: true, ..WindowOptions::default() },
        )?;
        window.set_target_fps(60);

        let mut last_mouse = (-1.0f32, -1.0f32);
        let mut last_left = false;
        let mut last_right = false;
        let mut last_middle = false;
        let mut last_keys: Vec<Key> = Vec::new();
        let mut ping_at = std::time::Instant::now();

        while window.is_open() && !window.is_key_down(Key::Escape) {
            while frame_rx.try_recv().is_ok() {
                if let Some(frame) = latest_frame.lock().unwrap().take() {
                    if let Ok((w, h, p)) = decode_frame(&frame) {
                        width = w;
                        height = h;
                        pixels = p;
                    }
                }
            }

            window.update_with_buffer(&pixels, width, height)?;

            if let Some((mx, my)) = window.get_mouse_pos(MouseMode::Clamp) {
                let (ww, wh) = window.get_size();
                let x = (mx / ww.max(1) as f32).clamp(0.0, 1.0);
                let y = (my / wh.max(1) as f32).clamp(0.0, 1.0);
                if (x - last_mouse.0).abs() > 0.0002 || (y - last_mouse.1).abs() > 0.0002 {
                    let _ = control_tx.send(ControlMessage::MouseMove { x_norm: x, y_norm: y });
                    last_mouse = (x, y);
                }
            }

            let left = window.get_mouse_down(MfMouseButton::Left);
            let right = window.get_mouse_down(MfMouseButton::Right);
            let middle = window.get_mouse_down(MfMouseButton::Middle);
            if left != last_left { let _ = control_tx.send(ControlMessage::MouseButton { button: MouseButton::Left, down: left }); last_left = left; }
            if right != last_right { let _ = control_tx.send(ControlMessage::MouseButton { button: MouseButton::Right, down: right }); last_right = right; }
            if middle != last_middle { let _ = control_tx.send(ControlMessage::MouseButton { button: MouseButton::Middle, down: middle }); last_middle = middle; }

            if let Some((_, wheel_y)) = window.get_scroll_wheel() {
                if wheel_y.abs() > 0.01 {
                    let _ = control_tx.send(ControlMessage::MouseWheel { delta: (wheel_y * 120.0) as i32 });
                }
            }

            let keys = window.get_keys();
            for key in &keys {
                if !last_keys.contains(key) {
                    if let Some(vk) = key_to_vk(*key) { let _ = control_tx.send(ControlMessage::Key { vk, down: true }); }
                }
            }
            for key in &last_keys {
                if !keys.contains(key) {
                    if let Some(vk) = key_to_vk(*key) { let _ = control_tx.send(ControlMessage::Key { vk, down: false }); }
                }
            }
            last_keys = keys;

            for key in window.get_keys_pressed(KeyRepeat::Yes) {
                if let Some(vk) = key_to_vk(key) {
                    let _ = control_tx.send(ControlMessage::Key { vk, down: true });
                    let _ = control_tx.send(ControlMessage::Key { vk, down: false });
                }
            }

            if ping_at.elapsed() >= Duration::from_secs(5) {
                let _ = control_tx.send(ControlMessage::Ping { unix_ms: now_ms() });
                ping_at = std::time::Instant::now();
            }
        }
        Ok(())
    }).await??;

    receive_task.abort();
    send_task.abort();
    Ok(viewer)
}

fn decode_frame(frame: &[u8]) -> Result<(usize, usize, Vec<u32>)> {
    if frame.len() < FRAME_HEADER_LEN || &frame[0..4] != FRAME_MAGIC {
        return Err(anyhow!("invalid frame"));
    }
    let width = u32::from_le_bytes(frame[4..8].try_into().unwrap()) as usize;
    let height = u32::from_le_bytes(frame[8..12].try_into().unwrap()) as usize;
    let payload = &frame[FRAME_HEADER_LEN..];

    if frame[12] == FRAME_PIXEL_JPEG && frame[13] == FRAME_COMPRESS_JPEG {
        let mut decoder = jpeg_decoder::Decoder::new(payload);
        let rgb = decoder
            .decode()
            .map_err(|e| anyhow!("jpeg decode failed: {e}"))?;
        let info = decoder
            .info()
            .ok_or_else(|| anyhow!("jpeg missing image info"))?;
        if info.width as usize != width || info.height as usize != height {
            return Err(anyhow!("jpeg dimensions mismatch"));
        }
        let mut pixels = Vec::with_capacity(width * height);
        for px in rgb.chunks_exact(3) {
            let r = px[0] as u32;
            let g = px[1] as u32;
            let b = px[2] as u32;
            pixels.push((r << 16) | (g << 8) | b);
        }
        return Ok((width, height, pixels));
    }

    if frame[12] == FRAME_PIXEL_H264 && frame[13] == FRAME_COMPRESS_H264 {
        return decode_h264(payload, width, height);
    }

    if frame[12] != FRAME_PIXEL_BGRA8 || frame[13] != FRAME_COMPRESS_ZSTD {
        return Err(anyhow!("unsupported frame format"));
    }
    let bgra = zstd::stream::decode_all(payload)?;
    if bgra.len() != width * height * 4 {
        return Err(anyhow!("frame size mismatch"));
    }
    let mut pixels = Vec::with_capacity(width * height);
    for px in bgra.chunks_exact(4) {
        let b = px[0] as u32;
        let g = px[1] as u32;
        let r = px[2] as u32;
        pixels.push((r << 16) | (g << 8) | b);
    }
    Ok((width, height, pixels))
}

thread_local! {
    static H264: RefCell<Option<Decoder>> = RefCell::new(None);
}

fn decode_h264(payload: &[u8], width: usize, height: usize) -> Result<(usize, usize, Vec<u32>)> {
    let _ = (width, height);
    H264.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(Decoder::new().map_err(|e| anyhow!("h264 init: {e}"))?);
        }
        let mut dec = slot.borrow_mut();
        let decoder = dec.as_mut().unwrap();
        let mut out: Option<(usize, usize, Vec<u32>)> = None;
        for nal in nal_units(payload) {
            match decoder.decode(nal) {
                Ok(Some(yuv)) => {
                    let (dw, dh) = yuv.dimensions();
                    if dw == 0 || dh == 0 {
                        continue;
                    }
                    let mut rgb = vec![0u8; dw * dh * 3];
                    yuv.write_rgb8(&mut rgb);
                    let mut pixels = Vec::with_capacity(dw * dh);
                    for px in rgb.chunks_exact(3) {
                        pixels.push((px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32);
                    }
                    out = Some((dw, dh, pixels));
                }
                Ok(None) => {}
                Err(e) => {
                    *dec = None;
                    return Err(anyhow!("h264 decode failed: {e}"));
                }
            }
        }
        out.ok_or_else(|| anyhow!("h264 waiting for more nals"))
    })
}

fn key_to_vk(key: Key) -> Option<u16> {
    use Key::*;
    Some(match key {
        A=>0x41,B=>0x42,C=>0x43,D=>0x44,E=>0x45,F=>0x46,G=>0x47,H=>0x48,I=>0x49,J=>0x4A,K=>0x4B,L=>0x4C,M=>0x4D,
        N=>0x4E,O=>0x4F,P=>0x50,Q=>0x51,R=>0x52,S=>0x53,T=>0x54,U=>0x55,V=>0x56,W=>0x57,X=>0x58,Y=>0x59,Z=>0x5A,
        Key0=>0x30,Key1=>0x31,Key2=>0x32,Key3=>0x33,Key4=>0x34,Key5=>0x35,Key6=>0x36,Key7=>0x37,Key8=>0x38,Key9=>0x39,
        Space=>0x20, Enter=>0x0D, Tab=>0x09, Backspace=>0x08, Delete=>0x2E, Insert=>0x2D,
        Left=>0x25, Up=>0x26, Right=>0x27, Down=>0x28, Home=>0x24, End=>0x23, PageUp=>0x21, PageDown=>0x22,
        LeftShift|RightShift=>0x10, LeftCtrl|RightCtrl=>0x11, LeftAlt|RightAlt=>0x12,
        F1=>0x70,F2=>0x71,F3=>0x72,F4=>0x73,F5=>0x74,F6=>0x75,F7=>0x76,F8=>0x77,F9=>0x78,F10=>0x79,F11=>0x7A,F12=>0x7B,
        _ => return None,
    })
}
