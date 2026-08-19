use axum::{
    body::Body,
    extract::{
        multipart::Multipart,
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Path, Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use remote_protocol::{
    AgentToServer, DeviceSummary, EnrollRequest, EnrollResponse, ServerToAgent, SessionRequest,
    SessionResponse,
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{info, warn};
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const DEFAULT_DB: &str = "/var/lib/darktask/darktask.db";
const DEFAULT_SECRET_DIR: &str = "/var/lib/darktask";
const INSTALL_PS1_TEMPLATE: &str = include_str!("../../../scripts/install.ps1");
const MAINTENANCE_PS1: &str = include_str!("../../../scripts/agent-maintenance.ps1");
const MIN_AGENT_BYTES: usize = 64 * 1024;
const MAX_AGENT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
struct DeviceRecord {
    summary: DeviceSummary,
    device_token_hash: String,
}

#[derive(Clone)]
struct SessionRecord {
    device_id: Uuid,
    token_hash: String,
    controller_tx: Option<mpsc::UnboundedSender<Message>>,
    agent_tx: Option<mpsc::UnboundedSender<Message>>,
}

#[derive(Clone)]
struct AppState {
    devices: Arc<DashMap<Uuid, DeviceRecord>>,
    live_agents: Arc<DashMap<Uuid, mpsc::UnboundedSender<ServerToAgent>>>,
    sessions: Arc<DashMap<Uuid, SessionRecord>>,
    db: Arc<Mutex<Connection>>,
    admin_token: Arc<RwLock<String>>,
    enroll_token: Arc<RwLock<String>>,
    secret_dir: Arc<PathBuf>,
}

#[derive(Serialize)]
struct AdminBootstrap {
    enrollment_token: String,
    agent_command: String,
    server_url: String,
    install_ps1_url: String,
    install_command: String,
    android_page_url: String,
    android_download_url: String,
    android_qr_svg: String,
    enroll_qr_svg: String,
}

#[derive(Serialize)]
struct AgentRelease {
    version: String,
    sha256: String,
    download_url: String,
}

#[derive(Serialize)]
struct AdminAgentRelease {
    deployed: bool,
    version: String,
    sha256: String,
    download_url: String,
    size_bytes: u64,
}

#[derive(Serialize)]
struct TokenResponse {
    token: String,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn random_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn db_path() -> PathBuf {
    std::env::var_os("REMOTE_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DB))
}

fn secret_dir() -> PathBuf {
    std::env::var_os("REMOTE_SECRET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SECRET_DIR))
}

fn agent_exe_path() -> PathBuf {
    std::env::var_os("REMOTE_AGENT_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|| secret_dir().join("remote-agent.exe"))
}

fn agent_version_file() -> PathBuf {
    agent_exe_path()
        .parent()
        .map(|p| p.join("remote-agent.version"))
        .unwrap_or_else(|| secret_dir().join("remote-agent.version"))
}

fn android_apk_path() -> PathBuf {
    std::env::var_os("REMOTE_ANDROID_APK")
        .map(PathBuf::from)
        .unwrap_or_else(|| secret_dir().join("darktask.apk"))
}

fn android_version_file() -> PathBuf {
    android_apk_path()
        .parent()
        .map(|p| p.join("darktask-android.version"))
        .unwrap_or_else(|| secret_dir().join("darktask-android.version"))
}

fn android_version_string() -> String {
    if let Ok(raw) = fs::read_to_string(android_version_file()) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    env!("CARGO_PKG_VERSION").to_string()
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn qr_svg(payload: &str) -> Result<String, String> {
    let code = qrcode::QrCode::new(payload.as_bytes()).map_err(|e| e.to_string())?;
    Ok(code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(180, 180)
        .dark_color(qrcode::render::svg::Color("#11151D"))
        .light_color(qrcode::render::svg::Color("#EEF2F7"))
        .quiet_zone(true)
        .build())
}

fn agent_version_string() -> String {
    if let Ok(raw) = fs::read_to_string(agent_version_file()) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    std::env::var("REMOTE_AGENT_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

fn validate_agent_binary(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < MIN_AGENT_BYTES {
        return Err(format!(
            "file too small ({} bytes); expected a Windows agent executable",
            bytes.len()
        ));
    }
    if bytes.len() > MAX_AGENT_BYTES {
        return Err(format!(
            "file too large ({} bytes); limit is {} MB",
            bytes.len(),
            MAX_AGENT_BYTES / (1024 * 1024)
        ));
    }
    if bytes.get(0..2) != Some(b"MZ") {
        return Err("not a Windows PE executable (missing MZ header)".into());
    }
    Ok(())
}

async fn write_agent_binary(bytes: &[u8], version: Option<&str>) -> Result<(), String> {
    validate_agent_binary(bytes)?;
    let path = agent_exe_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("upload.tmp");
    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|e| format!("write temp agent: {e}"))?;
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(|e| format!("publish agent binary: {e}"))?;

    let version_text = version
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("upload-{}", now_ms() / 1000));
    fs::write(agent_version_file(), format!("{version_text}\n")).map_err(|e| e.to_string())?;
    Ok(())
}

fn validate_android_apk(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < MIN_AGENT_BYTES {
        return Err(format!(
            "file too small ({} bytes); expected a DarkTask APK",
            bytes.len()
        ));
    }
    if bytes.len() > MAX_AGENT_BYTES {
        return Err(format!(
            "file too large ({} bytes); limit is {} MB",
            bytes.len(),
            MAX_AGENT_BYTES / (1024 * 1024)
        ));
    }
    if bytes.get(0..2) != Some(b"PK") {
        return Err("not an APK (missing ZIP/PK header)".into());
    }
    Ok(())
}

async fn write_android_apk(bytes: &[u8], version: Option<&str>) -> Result<(), String> {
    validate_android_apk(bytes)?;
    let path = android_apk_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("upload.tmp");
    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|e| format!("write temp apk: {e}"))?;
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(|e| format!("publish apk: {e}"))?;
    let version_text = version
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("upload-{}", now_ms() / 1000));
    fs::write(android_version_file(), format!("{version_text}\n")).map_err(|e| e.to_string())?;
    Ok(())
}

fn android_release_info() -> Result<AdminAgentRelease, String> {
    let path = android_apk_path();
    if !path.is_file() {
        return Ok(AdminAgentRelease {
            deployed: false,
            version: android_version_string(),
            sha256: String::new(),
            download_url: "/api/v1/android/download".into(),
            size_bytes: 0,
        });
    }
    let meta = fs::metadata(&path).map_err(|e| e.to_string())?;
    let sha256 = file_sha256_hex(&path).map_err(|e| e.to_string())?;
    Ok(AdminAgentRelease {
        deployed: true,
        version: android_version_string(),
        sha256,
        download_url: "/api/v1/android/download".into(),
        size_bytes: meta.len(),
    })
}

fn agent_release_info() -> Result<AdminAgentRelease, String> {
    let path = agent_exe_path();
    if !path.is_file() {
        return Ok(AdminAgentRelease {
            deployed: false,
            version: agent_version_string(),
            sha256: String::new(),
            download_url: "/api/v1/agent/download".into(),
            size_bytes: 0,
        });
    }
    let meta = fs::metadata(&path).map_err(|e| e.to_string())?;
    let sha256 = file_sha256_hex(&path).map_err(|e| e.to_string())?;
    Ok(AdminAgentRelease {
        deployed: true,
        version: agent_version_string(),
        sha256,
        download_url: "/api/v1/agent/download".into(),
        size_bytes: meta.len(),
    })
}

fn file_sha256_hex(path: &FsPath) -> anyhow::Result<String> {
    let bytes = fs::read(path)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn secret_file(dir: &FsPath, name: &str) -> PathBuf {
    dir.join(format!("{name}.token"))
}

fn write_secret(path: &FsPath, value: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{value}\n"))?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn load_or_generate_secret(env_name: &str, file_name: &str, dir: &FsPath) -> anyhow::Result<String> {
    if let Ok(value) = std::env::var(env_name) {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return Ok(value);
        }
    }

    let path = secret_file(dir, file_name);
    if path.exists() {
        let value = fs::read_to_string(&path)?.trim().to_string();
        if !value.is_empty() {
            return Ok(value);
        }
    }

    let value = random_token();
    write_secret(&path, &value)?;
    eprintln!("{env_name} not supplied; generated {}", path.display());
    Ok(value)
}

fn init_db(path: &PathBuf) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(
        r#"
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;
        CREATE TABLE IF NOT EXISTS devices (
            device_id TEXT PRIMARY KEY,
            hostname TEXT NOT NULL,
            platform TEXT NOT NULL,
            arch TEXT NOT NULL,
            agent_version TEXT NOT NULL,
            device_token_hash TEXT NOT NULL,
            last_seen_unix_ms INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY,
            device_id TEXT NOT NULL,
            controller_id TEXT NOT NULL,
            started_unix_ms INTEGER NOT NULL,
            state TEXT NOT NULL
        );
        "#,
    )?;
    Ok(conn)
}

fn load_devices(conn: &Connection) -> anyhow::Result<DashMap<Uuid, DeviceRecord>> {
    let out = DashMap::new();
    let mut stmt = conn.prepare(
        "SELECT device_id, hostname, platform, arch, agent_version, device_token_hash, last_seen_unix_ms FROM devices",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?,
        ))
    })?;

    for row in rows {
        let (id, hostname, platform, arch, agent_version, device_token_hash, last_seen) = row?;
        let device_id = Uuid::parse_str(&id)?;
        out.insert(
            device_id,
            DeviceRecord {
                summary: DeviceSummary {
                    device_id,
                    hostname,
                    platform,
                    arch,
                    agent_version,
                    online: false,
                    last_seen_unix_ms: last_seen as u64,
                    session_peek: None,
                },
                device_token_hash,
            },
        );
    }
    Ok(out)
}

fn current_admin_token(state: &AppState) -> String {
    state.admin_token.read().expect("admin token lock poisoned").clone()
}

fn current_enroll_token(state: &AppState) -> String {
    state.enroll_token.read().expect("enroll token lock poisoned").clone()
}

fn require_admin(headers: &HeaderMap, state: &AppState) -> Result<(), (StatusCode, String)> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let expected = current_admin_token(state);

    match token {
        Some(t) if t == expected => Ok(()),
        _ => Err((StatusCode::UNAUTHORIZED, "missing or invalid admin token".into())),
    }
}

fn public_server_url(headers: &HeaderMap) -> String {
    if let Ok(v) = std::env::var("REMOTE_PUBLIC_URL") {
        if !v.trim().is_empty() {
            return v.trim_end_matches('/').to_string();
        }
    }
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1:8788");
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    format!("{proto}://{host}")
}

fn cli_mode() -> anyhow::Result<bool> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("token") {
        return Ok(false);
    }

    let dir = secret_dir();
    fs::create_dir_all(&dir)?;

    match args.get(2).map(String::as_str) {
        Some("generate") => println!("{}", random_token()),
        Some("admin") => println!("{}", load_or_generate_secret("REMOTE_ADMIN_TOKEN", "admin", &dir)?),
        Some("enroll") => println!("{}", load_or_generate_secret("REMOTE_ENROLL_TOKEN", "enroll", &dir)?),
        Some("rotate-admin") => {
            let token = random_token();
            let path = secret_file(&dir, "admin");
            write_secret(&path, &token)?;
            println!("{token}");
            eprintln!("saved {}; restart darktask if REMOTE_ADMIN_TOKEN is not set in server.env", path.display());
        }
        Some("rotate-enroll") => {
            let token = random_token();
            let path = secret_file(&dir, "enroll");
            write_secret(&path, &token)?;
            println!("{token}");
            eprintln!("saved {}; restart darktask if REMOTE_ENROLL_TOKEN is not set in server.env", path.display());
        }
        _ => {
            eprintln!("Usage:\n  darktask-server token generate\n  darktask-server token admin\n  darktask-server token enroll\n  darktask-server token rotate-admin\n  darktask-server token rotate-enroll");
            std::process::exit(2);
        }
    }
    Ok(true)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if cli_mode()? {
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("remote_server=info".parse()?),
        )
        .init();

    let secrets = secret_dir();
    fs::create_dir_all(&secrets)?;
    let admin_token = load_or_generate_secret("REMOTE_ADMIN_TOKEN", "admin", &secrets)?;
    let enroll_token = load_or_generate_secret("REMOTE_ENROLL_TOKEN", "enroll", &secrets)?;

    let path = db_path();
    let conn = init_db(&path)?;
    let devices = load_devices(&conn)?;
    info!(db=%path.display(), loaded=devices.len(), "device database ready");

    let state = AppState {
        devices: Arc::new(devices),
        live_agents: Arc::new(DashMap::new()),
        sessions: Arc::new(DashMap::new()),
        db: Arc::new(Mutex::new(conn)),
        admin_token: Arc::new(RwLock::new(admin_token)),
        enroll_token: Arc::new(RwLock::new(enroll_token)),
        secret_dir: Arc::new(secrets),
    };

    let app = Router::new()
        .route("/", get(admin_ui))
        .route("/health", get(|| async { "ok" }))
        .route("/api/v1/enroll", post(enroll))
        .route("/api/v1/devices", get(list_devices))
        .route("/api/v1/devices/{device_id}/session", post(request_session))
        .route("/api/v1/devices/{device_id}", delete(delete_device))
        .route("/api/v1/admin/bootstrap", get(admin_bootstrap))
        .route("/api/v1/admin/token/enroll", post(rotate_enroll_token))
        .route("/api/v1/admin/token/admin", post(rotate_admin_token))
        .route("/api/v1/admin/install.ps1", get(admin_install_ps1))
        .route("/api/v1/admin/agent/release", get(admin_agent_release))
        .route("/api/v1/admin/agent/upload", post(admin_upload_agent))
        .route("/api/v1/admin/android/release", get(admin_android_release))
        .route("/api/v1/admin/android/upload", post(admin_upload_android))
        .route("/api/v1/agent/latest", get(agent_latest))
        .route("/api/v1/agent/download", get(agent_download))
        .route("/api/v1/agent/maintenance.ps1", get(agent_maintenance_ps1))
        .route("/api/v1/android/download", get(android_download))
        .route("/android", get(android_install_page))
        .route("/ws/agent", get(agent_ws))
        .route("/ws/session/{session_id}", get(session_ws))
        .layer(DefaultBodyLimit::max(MAX_AGENT_BYTES))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = std::env::var("REMOTE_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8788".into())
        .parse()?;

    info!(%addr, "remote server listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn admin_ui() -> Html<&'static str> {
    Html(ADMIN_HTML)
}

async fn admin_bootstrap(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<AdminBootstrap>, (StatusCode, String)> {
    require_admin(&headers, &state)?;
    let server_url = public_server_url(&headers);
    let enrollment_token = current_enroll_token(&state);
    let install_ps1_url = format!("{server_url}/api/v1/admin/install.ps1");
    let install_command = format!(
        r#"powershell -ExecutionPolicy Bypass -Command "& {{ $h=@{{Authorization='Bearer {admin}'}}; $p=Join-Path $env:TEMP 'darktask-install.ps1'; Invoke-WebRequest -Uri '{install_ps1_url}' -Headers $h -OutFile $p; & $p }}""#,
        admin = current_admin_token(&state),
        install_ps1_url = install_ps1_url,
    );
    let android_page_url = format!("{server_url}/android");
    let android_download_url = format!("{server_url}/api/v1/android/download");
    let enroll_uri = format!(
        "darktask://enroll?server={}&token={}",
        url_encode(&server_url),
        url_encode(&enrollment_token),
    );
    let android_qr_svg = qr_svg(&android_page_url).unwrap_or_default();
    let enroll_qr_svg = qr_svg(&enroll_uri).unwrap_or_default();
    Ok(Json(AdminBootstrap {
        agent_command: format!(
            r#"powershell -ExecutionPolicy Bypass -File .\install.ps1 -Server "{server_url}" -EnrollToken "{enrollment_token}""#,
        ),
        enrollment_token,
        server_url: server_url.clone(),
        install_ps1_url,
        install_command,
        android_page_url,
        android_download_url,
        android_qr_svg,
        enroll_qr_svg,
    }))
}

async fn admin_install_ps1(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Response, (StatusCode, String)> {
    require_admin(&headers, &state)?;
    let server_url = public_server_url(&headers);
    let enrollment_token = current_enroll_token(&state);
    let script = INSTALL_PS1_TEMPLATE
        .replace("__DARKTASK_SERVER__", &server_url)
        .replace("__DARKTASK_ENROLL__", &enrollment_token);
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"install.ps1\"",
        )
        .body(Body::from(script))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?)
}

async fn admin_agent_release(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<AdminAgentRelease>, (StatusCode, String)> {
    require_admin(&headers, &state)?;
    admin_agent_release_info().map(Json).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

fn admin_agent_release_info() -> Result<AdminAgentRelease, String> {
    agent_release_info()
}

async fn admin_upload_agent(
    headers: HeaderMap,
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<AgentRelease>, (StatusCode, String)> {
    require_admin(&headers, &state)?;

    let mut bytes: Vec<u8> = Vec::new();
    let mut version: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("multipart read failed: {e}")))?
    {
        match field.name() {
            Some("file") => {
                bytes = field
                    .bytes()
                    .await
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("read upload: {e}")))?
                    .to_vec();
            }
            Some("version") => {
                version = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| (StatusCode::BAD_REQUEST, format!("read version: {e}")))?,
                );
            }
            _ => {}
        }
    }

    if bytes.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "missing file field".into()));
    }

    write_agent_binary(&bytes, version.as_deref())
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let info = agent_release_info().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    info!(
        version = %info.version,
        sha256 = %info.sha256,
        size_bytes = info.size_bytes,
        "agent binary uploaded from admin portal"
    );

    Ok(Json(AgentRelease {
        version: info.version,
        sha256: info.sha256,
        download_url: info.download_url,
    }))
}

async fn agent_latest() -> Result<Json<AgentRelease>, (StatusCode, String)> {
    let path = agent_exe_path();
    if !path.is_file() {
        return Err((
            StatusCode::NOT_FOUND,
            format!(
                "agent binary not deployed (set REMOTE_AGENT_EXE or place remote-agent.exe in {})",
                path.display()
            ),
        ));
    }
    let sha256 = file_sha256_hex(&path).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("hash agent binary: {e}"))
    })?;
    Ok(Json(AgentRelease {
        version: agent_version_string(),
        sha256,
        download_url: "/api/v1/agent/download".into(),
    }))
}

async fn agent_download() -> Result<Response, (StatusCode, String)> {
    let path = agent_exe_path();
    if !path.is_file() {
        return Err((StatusCode::NOT_FOUND, "agent binary not deployed".into()));
    }
    let bytes = tokio::fs::read(&path).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("read agent binary: {e}"))
    })?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"remote-agent.exe\"",
        )
        .body(Body::from(bytes))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?)
}

async fn admin_android_release(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<AdminAgentRelease>, (StatusCode, String)> {
    require_admin(&headers, &state)?;
    android_release_info().map(Json).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn admin_upload_android(
    headers: HeaderMap,
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<AgentRelease>, (StatusCode, String)> {
    require_admin(&headers, &state)?;
    let mut bytes: Vec<u8> = Vec::new();
    let mut version: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("multipart read failed: {e}")))?
    {
        match field.name() {
            Some("file") => {
                bytes = field
                    .bytes()
                    .await
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("read upload: {e}")))?
                    .to_vec();
            }
            Some("version") => {
                version = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| (StatusCode::BAD_REQUEST, format!("read version: {e}")))?,
                );
            }
            _ => {}
        }
    }
    if bytes.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "missing file field".into()));
    }
    write_android_apk(&bytes, version.as_deref())
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let info = android_release_info().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    info!(
        version = %info.version,
        sha256 = %info.sha256,
        size_bytes = info.size_bytes,
        "android apk uploaded from admin portal"
    );
    Ok(Json(AgentRelease {
        version: info.version,
        sha256: info.sha256,
        download_url: info.download_url,
    }))
}

async fn android_download() -> Result<Response, (StatusCode, String)> {
    let path = android_apk_path();
    if !path.is_file() {
        return Err((StatusCode::NOT_FOUND, "android apk not deployed".into()));
    }
    let bytes = tokio::fs::read(&path).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, format!("read apk: {e}"))
    })?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/vnd.android.package-archive")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"darktask.apk\"",
        )
        .body(Body::from(bytes))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?)
}

async fn android_install_page() -> Html<&'static str> {
    Html(ANDROID_INSTALL_HTML)
}

async fn agent_maintenance_ps1() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"agent-maintenance.ps1\"",
        )
        .body(Body::from(MAINTENANCE_PS1))
        .unwrap()
}

async fn rotate_enroll_token(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<TokenResponse>, (StatusCode, String)> {
    require_admin(&headers, &state)?;
    let token = random_token();
    write_secret(&secret_file(&state.secret_dir, "enroll"), &token)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.enroll_token.write().map_err(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, "enrollment token lock poisoned".into())
    })? = token.clone();
    info!("enrollment token rotated");
    Ok(Json(TokenResponse { token }))
}

async fn rotate_admin_token(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<TokenResponse>, (StatusCode, String)> {
    require_admin(&headers, &state)?;
    let token = random_token();
    write_secret(&secret_file(&state.secret_dir, "admin"), &token)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.admin_token.write().map_err(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, "admin token lock poisoned".into())
    })? = token.clone();
    warn!("admin token rotated");
    Ok(Json(TokenResponse { token }))
}

async fn enroll(
    State(state): State<AppState>,
    Json(req): Json<EnrollRequest>,
) -> Result<Json<EnrollResponse>, (StatusCode, String)> {
    if req.enrollment_token != current_enroll_token(&state) {
        return Err((StatusCode::UNAUTHORIZED, "invalid enrollment token".into()));
    }

    let device_id = Uuid::new_v4();
    let device_token = random_token();
    let hash = token_hash(&device_token);
    let summary = DeviceSummary {
        device_id,
        hostname: req.hostname,
        platform: req.platform,
        arch: req.arch,
        agent_version: req.agent_version,
        online: false,
        last_seen_unix_ms: now_ms(),
        session_peek: None,
    };

    {
        let db = state.db.lock().map_err(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "database lock poisoned".into())
        })?;
        db.execute(
            "INSERT INTO devices (device_id, hostname, platform, arch, agent_version, device_token_hash, last_seen_unix_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                summary.device_id.to_string(),
                summary.hostname,
                summary.platform,
                summary.arch,
                summary.agent_version,
                hash,
                summary.last_seen_unix_ms as i64
            ],
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    state.devices.insert(
        device_id,
        DeviceRecord {
            summary,
            device_token_hash: token_hash(&device_token),
        },
    );

    Ok(Json(EnrollResponse {
        device_id,
        device_token,
        heartbeat_interval_secs: 60,
    }))
}

async fn list_devices(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Json<Vec<DeviceSummary>>, (StatusCode, String)> {
    require_admin(&headers, &state)?;
    let mut out: Vec<_> = state.devices.iter().map(|d| d.summary.clone()).collect();
    out.sort_by(|a, b| a.hostname.cmp(&b.hostname));
    Ok(Json(out))
}

async fn delete_device(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(device_id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin(&headers, &state)?;

    let Some(record) = state.devices.get(&device_id) else {
        return Err((StatusCode::NOT_FOUND, "unknown device".into()));
    };
    let hostname = record.summary.hostname.clone();
    drop(record);

    state.live_agents.remove(&device_id);
    state.devices.remove(&device_id);

    {
        let db = state.db.lock()
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database lock poisoned".into()))?;
        db.execute("DELETE FROM sessions WHERE device_id=?1", params![device_id.to_string()])
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        db.execute("DELETE FROM devices WHERE device_id=?1", params![device_id.to_string()])
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    info!(%device_id, %hostname, "device deleted by admin");
    Ok(StatusCode::NO_CONTENT)
}

async fn request_session(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(device_id): Path<Uuid>,
    Json(req): Json<SessionRequest>,
) -> Result<Json<SessionResponse>, (StatusCode, String)> {
    require_admin(&headers, &state)?;
    let session_id = Uuid::new_v4();
    let session_token = random_token();

    let Some(tx) = state.live_agents.get(&device_id) else {
        return Err((StatusCode::CONFLICT, "device is offline".into()));
    };

    {
        let db = state.db.lock().map_err(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "database lock poisoned".into())
        })?;
        db.execute(
            "INSERT INTO sessions (session_id, device_id, controller_id, started_unix_ms, state) VALUES (?1, ?2, ?3, ?4, 'requested')",
            params![
                session_id.to_string(),
                device_id.to_string(),
                req.controller_id,
                now_ms() as i64
            ],
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    state.sessions.insert(
        session_id,
        SessionRecord {
            device_id,
            token_hash: token_hash(&session_token),
            controller_tx: None,
            agent_tx: None,
        },
    );

    tx.send(ServerToAgent::StartSession {
        session_id,
        controller_id: req.controller_id,
        session_token: session_token.clone(),
        session_mode: req.session_mode,
    })
    .map_err(|_| (StatusCode::GONE, "agent connection closed".into()))?;

    Ok(Json(SessionResponse {
        session_id,
        session_token,
        status: "requested".into(),
    }))
}

async fn agent_ws(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| agent_socket(socket, state))
}

async fn agent_socket(socket: WebSocket, state: AppState) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerToAgent>();

    let forward = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let Ok(text) = serde_json::to_string(&msg) else { continue };
            if ws_tx.send(Message::Text(text.into())).await.is_err() { break; }
        }
    });

    let mut authenticated_device: Option<Uuid> = None;

    while let Some(Ok(msg)) = ws_rx.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(parsed) = serde_json::from_str::<AgentToServer>(&text) else { continue };

        match parsed {
            AgentToServer::Hello(hello) => {
                let Some(mut record) = state.devices.get_mut(&hello.device_id) else { break };
                if record.device_token_hash != token_hash(&hello.device_token) { break; }

                record.summary.hostname = hello.hostname;
                record.summary.agent_version = hello.agent_version;
                record.summary.online = true;
                record.summary.last_seen_unix_ms = now_ms();

                authenticated_device = Some(hello.device_id);
                state.live_agents.insert(hello.device_id, tx.clone());
                let _ = tx.send(ServerToAgent::HelloAck);
                info!(device_id=%hello.device_id, "agent online");
            }
            AgentToServer::Heartbeat(hb) => {
                if authenticated_device != Some(hb.device_id) { continue; }
                if let Some(mut record) = state.devices.get_mut(&hb.device_id) {
                    record.summary.online = true;
                    record.summary.last_seen_unix_ms = hb.unix_ms;
                    record.summary.session_peek = hb.session_peek.clone();
                    if let Ok(db) = state.db.lock() {
                        let _ = db.execute(
                            "UPDATE devices SET last_seen_unix_ms=?1, hostname=?2, agent_version=?3 WHERE device_id=?4",
                            params![
                                hb.unix_ms as i64,
                                record.summary.hostname,
                                record.summary.agent_version,
                                hb.device_id.to_string()
                            ],
                        );
                    }
                }
            }
            AgentToServer::SessionAccepted { session_id } => {
                if let Ok(db) = state.db.lock() {
                    let _ = db.execute(
                        "UPDATE sessions SET state='accepted' WHERE session_id=?1",
                        params![session_id.to_string()],
                    );
                }
                info!(%session_id, "agent accepted session");
            }
            AgentToServer::SessionRejected { session_id, reason } => {
                if let Ok(db) = state.db.lock() {
                    let _ = db.execute(
                        "UPDATE sessions SET state='rejected' WHERE session_id=?1",
                        params![session_id.to_string()],
                    );
                }
                warn!(%session_id, %reason, "agent rejected session");
            }
        }
    }

    if let Some(device_id) = authenticated_device {
        state.live_agents.remove(&device_id);
        if let Some(mut record) = state.devices.get_mut(&device_id) {
            record.summary.online = false;
            record.summary.last_seen_unix_ms = now_ms();
            record.summary.session_peek = None;
        }
        info!(%device_id, "agent offline");
    }
    forward.abort();
}

#[derive(Deserialize)]
struct SessionQuery {
    role: String,
    token: String,
}

async fn session_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Query(q): Query<SessionQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let Some(session) = state.sessions.get(&session_id) else {
        return Err((StatusCode::NOT_FOUND, "unknown session".into()));
    };
    if session.token_hash != token_hash(&q.token) {
        return Err((StatusCode::UNAUTHORIZED, "bad session token".into()));
    }
    if q.role != "agent" && q.role != "controller" {
        return Err((StatusCode::BAD_REQUEST, "role must be agent or controller".into()));
    }
    drop(session);
    Ok(ws.on_upgrade(move |socket| session_socket(socket, state, session_id, q.role)))
}

async fn session_socket(socket: WebSocket, state: AppState, session_id: Uuid, role: String) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    if let Some(mut session) = state.sessions.get_mut(&session_id) {
        if role == "agent" {
            session.agent_tx = Some(tx.clone());
        } else {
            session.controller_tx = Some(tx.clone());
        }
        info!(%session_id, %role, device_id=%session.device_id, "session peer connected");
    } else {
        return;
    }

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_tx.send(msg).await.is_err() { break; }
        }
    });

    while let Some(Ok(msg)) = ws_rx.next().await {
        let target = state.sessions.get(&session_id).and_then(|session| {
            if role == "agent" { session.controller_tx.clone() } else { session.agent_tx.clone() }
        });
        if let Some(target) = target {
            if target.send(msg).is_err() { break; }
        }
    }

    if let Some(mut session) = state.sessions.get_mut(&session_id) {
        if role == "agent" { session.agent_tx = None; } else { session.controller_tx = None; }
        if session.agent_tx.is_none() && session.controller_tx.is_none() {
            drop(session);
            state.sessions.remove(&session_id);
            if let Ok(db) = state.db.lock() {
                let _ = db.execute(
                    "UPDATE sessions SET state='closed' WHERE session_id=?1",
                    params![session_id.to_string()],
                );
            }
        }
    }
    writer.abort();
    info!(%session_id, %role, "session peer disconnected");
}

const ANDROID_INSTALL_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Install DarkTask</title>
<style>
:root{color-scheme:dark}*{box-sizing:border-box}body{margin:0;background:#090b10;color:#eef2f7;font:16px system-ui,-apple-system,Segoe UI,sans-serif}
.wrap{max-width:420px;margin:0 auto;padding:36px 22px}h1{font-size:28px;margin:0 0 8px}.brand span{color:#61a8ff}
p{color:#8c96a8;line-height:1.45}.btn{display:block;text-align:center;text-decoration:none;background:#edf1f7;color:#090b10;font-weight:700;border-radius:12px;padding:16px;margin-top:22px}
.meta{font:12px ui-monospace,SFMono-Regular,Consolas,monospace;color:#8c96a8;margin-top:18px}
</style></head>
<body><div class="wrap">
<div class="brand" style="font-size:22px;font-weight:800">Dark<span>Task</span></div>
<h1>Android endpoint</h1>
<p>Install the APK, then scan the <strong>Enroll</strong> QR from the admin console (or type the server URL and token in the app).</p>
<p>Allow screen capture. Enable DarkTask under Accessibility for remote taps. Video only — no audio.</p>
<a class="btn" href="/api/v1/android/download">Download DarkTask APK</a>
<p class="meta">Unknown sources / Install unknown apps must be allowed for this browser.</p>
</div></body></html>"#;

const ADMIN_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>DarkTask Admin</title>
<style>
:root{color-scheme:dark;--bg:#090b10;--p:#11151d;--p2:#171c26;--b:#252c39;--t:#eef2f7;--m:#8c96a8;--g:#36d17c;--blue:#61a8ff}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--t);font:14px system-ui,-apple-system,Segoe UI,sans-serif}
button,input{font:inherit}.shell{max-width:1200px;margin:auto;padding:28px}.top{display:flex;justify-content:space-between;align-items:center;margin-bottom:24px}
.brand{font-size:24px;font-weight:800}.brand span{color:var(--blue)}.sub,.meta{color:var(--m);font-size:12px}.grid{display:grid;grid-template-columns:1.7fr 1fr;gap:18px}
.card{background:var(--p);border:1px solid var(--b);border-radius:16px;overflow:hidden}.head{padding:16px 18px;border-bottom:1px solid var(--b);display:flex;justify-content:space-between;align-items:center}.head h2{margin:0;font-size:14px}.body{padding:18px}
.device{display:grid;grid-template-columns:1.4fr 1fr .95fr .7fr auto;gap:14px;align-items:center;padding:15px 18px;border-bottom:1px solid var(--b)}.host{font-weight:700}
.peek{font-size:12px;line-height:1.35}.peek.active{color:var(--g)}.peek.idle{color:var(--m)}.peek.none{color:#687184}
.status{display:flex;align-items:center;gap:7px}.dot{width:8px;height:8px;border-radius:50%;background:#687184}.online .dot{background:var(--g);box-shadow:0 0 12px #36d17c66}
.btn{border:1px solid var(--b);background:var(--p2);color:var(--t);border-radius:9px;padding:9px 12px;cursor:pointer}.btn.primary{background:#edf1f7;color:#090b10;font-weight:700}.btn:disabled{opacity:.4;cursor:not-allowed}
.mono{font:12px ui-monospace,SFMono-Regular,Consolas,monospace}.code{background:#090c12;border:1px solid var(--b);border-radius:10px;padding:12px;word-break:break-all}.label{color:var(--m);font-size:11px;text-transform:uppercase;letter-spacing:.08em;margin:15px 0 7px}
.toolbar{display:flex;gap:8px}.login{max-width:min(720px,94vw);margin:14vh auto}.login .body{padding:26px}.login p{color:var(--m)}input{width:100%;padding:12px;background:#090c12;border:1px solid var(--b);border-radius:10px;color:var(--t)}
.hidden{display:none!important}.empty{padding:28px;text-align:center;color:var(--m)}.notice{margin-top:12px;padding:11px;background:#101824;border:1px solid #203756;border-radius:10px;color:#afd0ff}
.qrrow{display:flex;gap:16px;flex-wrap:wrap;margin-top:12px}.qrbox{width:176px}.qr{background:#eef2f7;border-radius:12px;padding:8px;min-height:160px}.qr svg{display:block;width:100%;height:auto}
.viewer{position:fixed;right:24px;bottom:24px;width:min(900px,72vw);height:min(680px,72vh);background:#050608;z-index:1000;display:flex;flex-direction:column;border:1px solid var(--b);border-radius:14px;overflow:hidden;box-shadow:0 24px 80px #000a}.viewerbar{height:52px;background:#0d1118;border-bottom:1px solid var(--b);display:flex;align-items:center;gap:10px;padding:0 14px}.viewerbar .grow{flex:1}.stage{flex:1;display:flex;align-items:center;justify-content:center;overflow:hidden;background:#000}.stage canvas{max-width:100%;max-height:100%;outline:none}.danger{color:#ff9aa4}.iconbtn{width:34px;height:34px;padding:0;display:inline-flex;align-items:center;justify-content:center;font-size:18px;line-height:1}.device-actions{display:flex;gap:7px;align-items:center}@media(max-width:900px){.viewer{right:10px;bottom:10px;width:calc(100vw - 20px);height:65vh}}@media(max-width:850px){.grid{grid-template-columns:1fr}.device{grid-template-columns:1fr auto}.hide-sm{display:none}}
</style>
</head>
<body>
<div id="login" class="shell login"><div class="card"><div class="body">
<div class="brand">Dark<span>Task</span></div><h1>Admin console</h1>
<p>Enter the admin token. It is kept only in this browser tab.</p>
<input id="token" type="password" placeholder="REMOTE_ADMIN_TOKEN"><div style="height:12px"></div>
<button class="btn primary" onclick="login()">Open console</button><div id="err" class="notice hidden"></div>
<div class="label" style="margin-top:20px">Elevated install one-liner</div>
<div class="sub" style="margin-bottom:7px">Run in elevated PowerShell on a target PC. Updates as you type your admin token.</div>
<div id="login-installcmd" class="code mono">—</div>
<button class="btn" style="margin-top:9px" onclick="copy('login-installcmd')">Copy one-liner</button>
</div></div></div>

<div id="app" class="shell hidden">
<div class="top"><div><div class="brand">Dark<span>Task</span></div><div class="sub">Managed remote access</div></div>
<div class="toolbar"><div id="server" class="sub"></div><button class="btn" onclick="logout()">Lock</button></div></div>
<div class="grid">
<div class="card"><div class="head"><h2>Devices</h2><button class="btn" onclick="devices()">Refresh</button></div><div id="devices"><div class="empty">Loading…</div></div><div class="sub" style="padding:10px 18px 14px;border-top:1px solid var(--b)">Session peek updates every 60s · active = input within 5 minutes</div></div>
<div>
<div class="card"><div class="head"><h2>Enroll a device</h2></div><div class="body">
<div class="label">Enrollment token</div><div id="enroll" class="code mono">—</div>
<div class="toolbar" style="margin-top:9px"><button class="btn" onclick="copy('enroll')">Copy token</button><button class="btn" onclick="rotateEnroll()">Rotate</button></div>
<div class="label">Android</div>
<div class="sub" style="margin-bottom:8px">Scan <strong>Install</strong> with the phone camera, then scan <strong>Enroll</strong> after DarkTask is installed. Video is H.264, no audio.</div>
<div class="qrrow"><div class="qrbox"><div class="label">1. Install</div><div id="apkqr" class="qr"></div><div class="sub" style="margin-top:6px">Opens the APK page</div></div><div class="qrbox"><div class="label">2. Enroll</div><div id="enrollqr" class="qr"></div><div class="sub" style="margin-top:6px">Opens DarkTask with server + token</div></div></div>
<div class="toolbar" style="margin-top:12px"><button class="btn primary" onclick="downloadApk()">Download APK</button><button class="btn" onclick="copy('androidpage')">Copy install URL</button></div>
<div id="androidpage" class="code mono" style="margin-top:9px">—</div>
<div class="label">Current APK</div><div id="apkrelease" class="code mono">—</div>
<input id="apkfile" type="file" accept=".apk,application/vnd.android.package-archive" style="margin-top:8px">
<input id="apkversion" type="text" placeholder="Version label (optional)" style="margin-top:8px">
<div class="toolbar" style="margin-top:9px"><button class="btn primary" onclick="uploadApk()">Upload APK</button></div>
<div id="apkuploadmsg" class="notice hidden"></div>
<div class="label">install.ps1 (service + reboot task + silent updates)</div>
<div class="sub" style="margin-bottom:8px">Run elevated on the target PC. Registers a scheduled task at startup and daily to keep the agent running and auto-update.</div>
<div class="toolbar" style="margin-top:9px"><button class="btn primary" onclick="downloadInstall()">Download install.ps1</button><button class="btn" onclick="copy('installcmd')">Copy elevated one-liner</button></div>
<div class="label">Elevated one-liner</div><div id="installcmd" class="code mono">—</div>
<div class="label">Manual install (script already downloaded)</div><div id="command" class="code mono">—</div><button class="btn" style="margin-top:9px" onclick="copy('command')">Copy manual command</button>
</div></div>
<div class="card" style="margin-top:18px"><div class="head"><h2>Agent release</h2></div><div class="body">
<div class="sub">Upload <span class="mono">remote-agent.exe</span> here. Endpoints install and silently update from this file.</div>
<div class="label">Current release</div><div id="agentrelease" class="code mono">—</div>
<div class="label">Upload new agent</div>
<input id="agentfile" type="file" accept=".exe,application/octet-stream,application/x-msdownload">
<input id="agentversion" type="text" placeholder="Version label (optional, e.g. 0.3.1)" style="margin-top:8px">
<div class="toolbar" style="margin-top:9px"><button class="btn primary" onclick="uploadAgent()">Upload agent</button><button class="btn" onclick="refreshAgentRelease()">Refresh</button></div>
<div id="agentuploadmsg" class="notice hidden"></div>
</div></div>
<div class="card" style="margin-top:18px"><div class="head"><h2>Security</h2></div><div class="body">
<div class="sub">Rotate the admin token if it has been exposed.</div><button class="btn" style="margin-top:12px" onclick="rotateAdmin()">Rotate admin token</button><div id="newadmin" class="notice hidden"></div>
</div></div>
</div></div></div>


<div id="viewer" class="viewer hidden">
  <div class="viewerbar">
    <strong id="viewerHost">Remote session</strong>
    <span id="viewerState" class="sub">Connecting…</span><span class="sub">Click screen to control</span>
    <div class="grow"></div>
    <button class="btn" onclick="toggleFullscreen()">Fullscreen</button>
    <button class="btn danger" onclick="disconnectViewer()">Disconnect</button>
  </div>
  <div class="stage"><canvas id="screen" tabindex="0"></canvas></div>
</div>

<script>
let tok=sessionStorage.getItem('darktask_admin')||'';
const ACTIVE_IDLE_SECS=300;
let deviceRefresh=null;
const H=()=>({'Authorization':'Bearer '+tok,'Content-Type':'application/json'});
async function A(path,opt={}){opt.headers={...(opt.headers||{}),...H()};let r=await fetch(path,opt),t=await r.text();if(r.status===401){logout();throw Error('Unauthorized')}if(!r.ok)throw Error(t||('HTTP '+r.status));return t?JSON.parse(t):{}}
function loginInstallCmd(){const t=document.getElementById('token').value.trim()||'YOUR_ADMIN_TOKEN',url=location.origin+'/api/v1/admin/install.ps1';document.getElementById('login-installcmd').textContent=`powershell -ExecutionPolicy Bypass -Command "& { $h=@{Authorization='Bearer ${t}'}; $p=Join-Path $env:TEMP 'darktask-install.ps1'; irm '${url}' -Headers $h -OutFile $p; & $p }"`}
document.getElementById('token').addEventListener('input',loginInstallCmd);
loginInstallCmd();
async function login(){tok=document.getElementById('token').value.trim();try{await A('/api/v1/admin/bootstrap');sessionStorage.setItem('darktask_admin',tok);show()}catch(e){let x=document.getElementById('err');x.textContent=e.message;x.classList.remove('hidden')}}
function logout(){tok='';sessionStorage.removeItem('darktask_admin');if(deviceRefresh){clearInterval(deviceRefresh);deviceRefresh=null}document.getElementById('app').classList.add('hidden');document.getElementById('login').classList.remove('hidden')}
async function show(){document.getElementById('login').classList.add('hidden');document.getElementById('app').classList.remove('hidden');await Promise.all([boot(),devices(),refreshAgentRelease(),refreshApkRelease()]);if(deviceRefresh)clearInterval(deviceRefresh);deviceRefresh=setInterval(devices,60000)}
async function boot(){let x=await A('/api/v1/admin/bootstrap');document.getElementById('enroll').textContent=x.enrollment_token;document.getElementById('command').textContent=x.agent_command;document.getElementById('installcmd').textContent=x.install_command;document.getElementById('server').textContent=x.server_url;document.getElementById('androidpage').textContent=x.android_page_url;document.getElementById('apkqr').innerHTML=x.android_qr_svg||'';document.getElementById('enrollqr').innerHTML=x.enroll_qr_svg||'';window.__installPs1Url=x.install_ps1_url;window.__apkUrl=x.android_download_url}
async function refreshApkRelease(){try{let x=await A('/api/v1/admin/android/release');document.getElementById('apkrelease').textContent=x.deployed?`v${x.version}  ·  ${(x.size_bytes/1024/1024).toFixed(2)} MB  ·  sha256 ${x.sha256.slice(0,16)}…`:'Not deployed — upload darktask.apk below.'}catch(e){document.getElementById('apkrelease').textContent='Unable to load APK info.'}}
async function uploadApk(){const msg=document.getElementById('apkuploadmsg');msg.classList.add('hidden');const f=document.getElementById('apkfile').files[0];if(!f){alert('Choose the DarkTask APK first.');return}if(!confirm(`Upload ${f.name} (${(f.size/1024/1024).toFixed(2)} MB) as the Android client?`))return;try{const fd=new FormData();fd.append('file',f,f.name);const v=document.getElementById('apkversion').value.trim();if(v)fd.append('version',v);const r=await fetch('/api/v1/admin/android/upload',{method:'POST',headers:{Authorization:'Bearer '+tok},body:fd});const t=await r.text();if(r.status===401){logout();throw Error('Unauthorized')}if(!r.ok)throw Error(t||('HTTP '+r.status));msg.textContent='APK uploaded. Phones can scan Install or use Download APK.';msg.classList.remove('hidden');document.getElementById('apkfile').value='';await refreshApkRelease()}catch(e){alert(e.message)}}
function downloadApk(){window.open(window.__apkUrl||'/api/v1/android/download','_blank')}
async function refreshAgentRelease(){try{let x=await A('/api/v1/admin/agent/release');document.getElementById('agentrelease').textContent=x.deployed?`v${x.version}  ·  ${(x.size_bytes/1024/1024).toFixed(2)} MB  ·  sha256 ${x.sha256.slice(0,16)}…`:'Not deployed — upload remote-agent.exe below.'}catch(e){document.getElementById('agentrelease').textContent='Unable to load release info.'}}
async function uploadAgent(){const msg=document.getElementById('agentuploadmsg');msg.classList.add('hidden');const f=document.getElementById('agentfile').files[0];if(!f){alert('Choose remote-agent.exe first.');return}if(!confirm(`Upload ${f.name} (${(f.size/1024/1024).toFixed(2)} MB) as the new agent release?`))return;try{const fd=new FormData();fd.append('file',f,f.name);const v=document.getElementById('agentversion').value.trim();if(v)fd.append('version',v);const r=await fetch('/api/v1/admin/agent/upload',{method:'POST',headers:{Authorization:'Bearer '+tok},body:fd});const t=await r.text();if(r.status===401){logout();throw Error('Unauthorized')}if(!r.ok)throw Error(t||('HTTP '+r.status));msg.textContent='Agent uploaded. Endpoints will pick this up on install or next maintenance run.';msg.classList.remove('hidden');document.getElementById('agentfile').value='';await refreshAgentRelease()}catch(e){alert(e.message)}}
async function downloadInstall(){try{let r=await fetch(window.__installPs1Url||'/api/v1/admin/install.ps1',{headers:{Authorization:'Bearer '+tok}});if(r.status===401){logout();throw Error('Unauthorized')}if(!r.ok)throw Error(await r.text()||('HTTP '+r.status));let blob=await r.blob();let a=document.createElement('a');a.href=URL.createObjectURL(blob);a.download='install.ps1';a.click();URL.revokeObjectURL(a.href)}catch(e){alert(e.message)}}
function fmtDuration(secs){if(secs<60)return secs+'s';const m=Math.floor(secs/60);if(m<60)return m+'m';return Math.floor(m/60)+'h '+(m%60)+'m'}
function sessionPeek(d){if(!d.online||!d.session_peek)return{text:'—',cls:'none'};const p=d.session_peek;if(!p.user_logged_in)return{text:'No user session',cls:'none'};if(p.idle_secs==null)return{text:'Logged in',cls:'active'};if(p.idle_secs<60)return{text:'Active now',cls:'active'};if(p.idle_secs<ACTIVE_IDLE_SECS)return{text:'Used '+fmtDuration(p.idle_secs)+' ago',cls:'active'};return{text:'Idle · '+fmtDuration(p.idle_secs),cls:'idle'}}
async function devices(){let a=await A('/api/v1/devices'),r=document.getElementById('devices');if(!a.length){r.innerHTML='<div class="empty">No enrolled devices</div>';return}r.innerHTML=a.map(d=>{const peek=sessionPeek(d);return `<div class="device"><div><div class="host">${E(d.hostname)}</div><div class="meta mono">${E(d.device_id)}</div></div><div class="hide-sm">${E(d.platform)} / ${E(d.arch)}<div class="meta">Agent ${E(d.agent_version)}</div></div><div class="peek ${peek.cls}">${E(peek.text)}</div><div class="status ${d.online?'online':''}"><span class="dot"></span>${d.online?'Online':'Offline'}</div><div class="device-actions"><button class="btn primary" ${d.online?'':'disabled'} onclick="connect('${d.device_id}','${EA(d.hostname)}')">Connect</button><button class="btn danger iconbtn" title="Delete client" aria-label="Delete client" onclick="deleteDevice('${d.device_id}','${EA(d.hostname)}')">×</button></div></div><div id="s-${d.device_id}" class="hidden"></div>`}).join('')}
let sessionSocket=null,sessionPing=null,lastMove=0;
const canvas=document.getElementById('screen'),ctx=canvas.getContext('2d',{alpha:false});
async function deleteDevice(id,host){
  if(!confirm(`Delete "${host}" from DarkTask?\n\nThis removes the saved server-side client identity. If the agent is still installed, it will need to be re-enrolled before reconnecting.`))return;
  try{
    let r=await fetch('/api/v1/devices/'+encodeURIComponent(id),{
      method:'DELETE',
      headers:{Authorization:'Bearer '+tok}
    });
    if(r.status===401){logout();throw Error('Unauthorized')}
    if(!r.ok)throw Error((await r.text())||('HTTP '+r.status));
    await devices();
  }catch(e){alert(e.message)}
}
async function connect(id,host){try{let s=await A('/api/v1/devices/'+id+'/session',{method:'POST',body:JSON.stringify({controller_id:'web-admin'})});openViewer(host,s)}catch(e){alert(e.message)}}
function wsBase(){return (location.protocol==='https:'?'wss://':'ws://')+location.host}
function openViewer(host,s){disconnectViewer();document.getElementById('viewer').classList.remove('hidden');document.getElementById('viewerHost').textContent=host;let st=document.getElementById('viewerState');st.textContent='Connecting…';let ws=new WebSocket(wsBase()+'/ws/session/'+encodeURIComponent(s.session_id)+'?role=controller&token='+encodeURIComponent(s.session_token));sessionSocket=ws;ws.binaryType='arraybuffer';ws.onopen=()=>{st.textContent='Connected';sessionPing=setInterval(()=>sendControl({type:'ping',data:{unix_ms:Date.now()}}),5000)};ws.onclose=()=>{st.textContent='Disconnected';if(sessionPing){clearInterval(sessionPing);sessionPing=null}};ws.onerror=()=>st.textContent='Error';ws.onmessage=e=>{if(typeof e.data==='string')return;try{drawFrame(new Uint8Array(e.data))}catch(err){st.textContent='Frame decode error';console.error(err)}}}
function disconnectViewer(){if(sessionPing){clearInterval(sessionPing);sessionPing=null}if(sessionSocket){try{sessionSocket.close()}catch{}sessionSocket=null}closeVdec();document.getElementById('viewer').classList.add('hidden')}
let vdec=null,vts=0,avcc=null;
function closeVdec(){try{vdec&&vdec.close()}catch{}vdec=null;avcc=null}
function splitNal(u){const nals=[];let i=0,start=-1;while(i+3<=u.length){let sc=0;if(u[i]===0&&u[i+1]===0&&u[i+2]===1)sc=3;else if(i+4<=u.length&&u[i]===0&&u[i+1]===0&&u[i+2]===0&&u[i+3]===1)sc=4;if(sc){if(start>=0)nals.push(u.subarray(start,i));i+=sc;start=i;continue}i++}if(start>=0&&start<u.length)nals.push(u.subarray(start));return nals}
function avcCFromAnnexB(u){let sps,pps;for(const n of splitNal(u)){const t=n[0]&31;if(t===7)sps=n;else if(t===8)pps=n}if(!sps||!pps)return null;const b=new Uint8Array(11+sps.length+pps.length);b[0]=1;b[1]=sps[1];b[2]=sps[2];b[3]=sps[3];b[4]=0xFF;b[5]=0xE1;b[6]=sps.length>>8;b[7]=sps.length&255;b.set(sps,8);let o=8+sps.length;b[o]=1;b[o+1]=pps.length>>8;b[o+2]=pps.length&255;b.set(pps,o+3);return{desc:b,codec:'avc1.'+[sps[1],sps[2],sps[3]].map(x=>x.toString(16).padStart(2,'0')).join('')}}
function toAvcc(u){const nals=splitNal(u).filter(n=>{const t=n[0]&31;return t!==7&&t!==8&&t!==9});let len=0;for(const n of nals)len+=4+n.length;const o=new Uint8Array(len);let p=0;for(const n of nals){o[p]=n.length>>24;o[p+1]=n.length>>16;o[p+2]=n.length>>8;o[p+3]=n.length;o.set(n,p+4);p+=4+n.length}return o}
function drawH264(w,h,key,payload){if(!window.VideoDecoder)throw Error('H.264 needs Chromium');if(canvas.width!==w||canvas.height!==h){canvas.width=w;canvas.height=h}if(key){const cfg=avcCFromAnnexB(payload);if(cfg&&(!vdec||vdec.state==='closed'||avcc!==cfg.codec)){closeVdec();vdec=new VideoDecoder({output:f=>{ctx.drawImage(f,0,0,w,h);f.close()},error:e=>console.error(e)});avcc=cfg.codec;const data=toAvcc(payload);vdec.configure({codec:cfg.codec,codedWidth:w,codedHeight:h,description:cfg.desc,hardwareAcceleration:'prefer-hardware',optimizeForLatency:true}).then(()=>{if(vdec&&vdec.state==='configured'&&data.length)vdec.decode(new EncodedVideoChunk({type:'key',timestamp:(vts+=83333),data}))});return}}if(!vdec||vdec.state!=='configured')return;if(!key&&vdec.decodeQueueSize>2)return;const data=toAvcc(payload);if(!data.length)return;vdec.decode(new EncodedVideoChunk({type:key?'key':'delta',timestamp:(vts+=83333),data}))}
function drawFrame(b){if(b.length<16||String.fromCharCode(b[0],b[1],b[2],b[3])!=='RPF1')throw Error('bad frame');let v=new DataView(b.buffer,b.byteOffset,b.byteLength),w=v.getUint32(4,true),h=v.getUint32(8,true);if(b[12]===2&&b[13]===2){closeVdec();if(canvas.width!==w||canvas.height!==h){canvas.width=w;canvas.height=h}createImageBitmap(new Blob([b.subarray(16)],{type:'image/jpeg'})).then(bmp=>{ctx.drawImage(bmp,0,0,w,h);bmp.close()}).catch(err=>{console.error(err);throw err});return}if(b[12]===3&&b[13]===3){drawH264(w,h,b[14]===1,b.subarray(16));return}if(b[12]!==1||b[13]!==1)throw Error('unsupported frame');throw Error('legacy zstd frames are no longer supported; update the agent')}
function sendControl(o){if(sessionSocket&&sessionSocket.readyState===WebSocket.OPEN)sessionSocket.send(JSON.stringify(o))}
function pos(e){let r=canvas.getBoundingClientRect();return{x:Math.max(0,Math.min(1,(e.clientX-r.left)/Math.max(1,r.width))),y:Math.max(0,Math.min(1,(e.clientY-r.top)/Math.max(1,r.height)))}}
canvas.addEventListener('mousemove',e=>{let n=performance.now();if(n-lastMove<4)return;lastMove=n;let p=pos(e);sendControl({type:'mouse_move',data:{x_norm:p.x,y_norm:p.y}})});
canvas.addEventListener('mousedown',e=>{e.preventDefault();canvas.focus();sendControl({type:'mouse_button',data:{button:e.button===2?'right':e.button===1?'middle':'left',down:true}})});
canvas.addEventListener('mouseup',e=>{e.preventDefault();sendControl({type:'mouse_button',data:{button:e.button===2?'right':e.button===1?'middle':'left',down:false}})});
canvas.addEventListener('contextmenu',e=>e.preventDefault());
canvas.addEventListener('wheel',e=>{e.preventDefault();sendControl({type:'mouse_wheel',data:{delta:e.deltaY<0?120:-120}})},{passive:false});
canvas.addEventListener('keydown',e=>{let vk=vkFor(e);if(vk==null)return;e.preventDefault();sendControl({type:'key',data:{vk:vk,down:true}})});
canvas.addEventListener('keyup',e=>{let vk=vkFor(e);if(vk==null)return;e.preventDefault();sendControl({type:'key',data:{vk:vk,down:false}})});
function vkFor(e){if(/^Key[A-Z]$/.test(e.code))return e.code.charCodeAt(3);if(/^Digit[0-9]$/.test(e.code))return e.code.charCodeAt(5);let m={Space:32,Enter:13,Tab:9,Backspace:8,Delete:46,Insert:45,ArrowLeft:37,ArrowUp:38,ArrowRight:39,ArrowDown:40,Home:36,End:35,PageUp:33,PageDown:34,ShiftLeft:16,ShiftRight:16,ControlLeft:17,ControlRight:17,AltLeft:18,AltRight:18,F1:112,F2:113,F3:114,F4:115,F5:116,F6:117,F7:118,F8:119,F9:120,F10:121,F11:122,F12:123};return m[e.code]??null}
function toggleFullscreen(){let v=document.getElementById('viewer');if(!document.fullscreenElement)v.requestFullscreen?.();else document.exitFullscreen?.()}
async function rotateEnroll(){if(!confirm('Rotate enrollment token?'))return;await A('/api/v1/admin/token/enroll',{method:'POST'});await boot()}
async function rotateAdmin(){if(!confirm('Rotate admin token now?'))return;let x=await A('/api/v1/admin/token/admin',{method:'POST'});tok=x.token;sessionStorage.setItem('darktask_admin',tok);let e=document.getElementById('newadmin');e.textContent='NEW ADMIN TOKEN — save now: '+x.token;e.classList.remove('hidden')}
function copy(id){navigator.clipboard.writeText(document.getElementById(id).textContent)}
function E(s){return String(s??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]))}function EA(s){return String(s??'').replace(/['\\]/g,'')}
if(tok)A('/api/v1/admin/bootstrap').then(show).catch(logout)
</script></body></html>"#;
