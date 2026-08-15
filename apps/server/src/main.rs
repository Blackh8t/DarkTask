use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use remote_protocol::{
    AgentToServer, DeviceSummary, EnrollRequest, EnrollResponse, ServerToAgent, SessionRequest,
    SessionResponse,
};
use rusqlite::{params, Connection};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{info, warn};
use uuid::Uuid;

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
    admin_token: Arc<String>,
    enroll_token: Arc<String>,
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
        .unwrap_or_else(|| PathBuf::from("/var/lib/remote-platform/remote.db"))
}

fn init_db(path: &PathBuf) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
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
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?,
        ))
    })?;
    for row in rows {
        let (id, hostname, platform, arch, agent_version, device_token_hash, last_seen) = row?;
        let device_id = Uuid::parse_str(&id)?;
        out.insert(device_id, DeviceRecord {
            summary: DeviceSummary {
                device_id, hostname, platform, arch, agent_version,
                online: false,
                last_seen_unix_ms: last_seen as u64,
            },
            device_token_hash,
        });
    }
    Ok(out)
}

fn require_admin(headers: &HeaderMap, state: &AppState) -> Result<(), (StatusCode, String)> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match token {
        Some(t) if t == state.admin_token.as_str() => Ok(()),
        _ => Err((StatusCode::UNAUTHORIZED, "missing or invalid admin token".into())),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("remote_server=info".parse()?),
        )
        .init();

    let admin_token = std::env::var("REMOTE_ADMIN_TOKEN")
        .map_err(|_| anyhow::anyhow!("REMOTE_ADMIN_TOKEN must be set"))?;
    let enroll_token = std::env::var("REMOTE_ENROLL_TOKEN")
        .map_err(|_| anyhow::anyhow!("REMOTE_ENROLL_TOKEN must be set"))?;

    let path = db_path();
    let conn = init_db(&path)?;
    let devices = load_devices(&conn)?;
    info!(db=%path.display(), loaded=devices.len(), "device database ready");

    let state = AppState {
        devices: Arc::new(devices),
        live_agents: Arc::new(DashMap::new()),
        sessions: Arc::new(DashMap::new()),
        db: Arc::new(Mutex::new(conn)),
        admin_token: Arc::new(admin_token),
        enroll_token: Arc::new(enroll_token),
    };

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/api/v1/enroll", post(enroll))
        .route("/api/v1/devices", get(list_devices))
        .route("/api/v1/devices/{device_id}/session", post(request_session))
        .route("/ws/agent", get(agent_ws))
        .route("/ws/session/{session_id}", get(session_ws))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = std::env::var("REMOTE_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8787".into())
        .parse()?;

    info!(%addr, "remote server listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn enroll(
    State(state): State<AppState>,
    Json(req): Json<EnrollRequest>,
) -> Result<Json<EnrollResponse>, (StatusCode, String)> {
    if req.enrollment_token != *state.enroll_token {
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
    };

    {
        let db = state.db.lock().map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database lock poisoned".into()))?;
        db.execute(
            "INSERT INTO devices (device_id, hostname, platform, arch, agent_version, device_token_hash, last_seen_unix_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![summary.device_id.to_string(), summary.hostname, summary.platform, summary.arch, summary.agent_version, hash, summary.last_seen_unix_ms as i64],
        ).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    state.devices.insert(device_id, DeviceRecord { summary, device_token_hash: token_hash(&device_token) });
    Ok(Json(EnrollResponse { device_id, device_token, heartbeat_interval_secs: 10 }))
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
        let db = state.db.lock().map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database lock poisoned".into()))?;
        db.execute(
            "INSERT INTO sessions (session_id, device_id, controller_id, started_unix_ms, state) VALUES (?1, ?2, ?3, ?4, 'requested')",
            params![session_id.to_string(), device_id.to_string(), req.controller_id, now_ms() as i64],
        ).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    state.sessions.insert(session_id, SessionRecord {
        device_id,
        token_hash: token_hash(&session_token),
        controller_tx: None,
        agent_tx: None,
    });

    tx.send(ServerToAgent::StartSession {
        session_id,
        controller_id: req.controller_id,
        session_token: session_token.clone(),
    }).map_err(|_| (StatusCode::GONE, "agent connection closed".into()))?;

    Ok(Json(SessionResponse { session_id, session_token, status: "requested".into() }))
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
                let Some(mut record) = state.devices.get_mut(&hello.device_id) else { break; };
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
                    if let Ok(db) = state.db.lock() {
                        let _ = db.execute(
                            "UPDATE devices SET last_seen_unix_ms=?1, hostname=?2, agent_version=?3 WHERE device_id=?4",
                            params![hb.unix_ms as i64, record.summary.hostname, record.summary.agent_version, hb.device_id.to_string()],
                        );
                    }
                }
            }
            AgentToServer::SessionAccepted { session_id } => {
                if let Ok(db) = state.db.lock() {
                    let _ = db.execute("UPDATE sessions SET state='accepted' WHERE session_id=?1", params![session_id.to_string()]);
                }
                info!(%session_id, "agent accepted session");
            }
            AgentToServer::SessionRejected { session_id, reason } => {
                if let Ok(db) = state.db.lock() {
                    let _ = db.execute("UPDATE sessions SET state='rejected' WHERE session_id=?1", params![session_id.to_string()]);
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
        }
        info!(%device_id, "agent offline");
    }
    forward.abort();
}

#[derive(Deserialize)]
struct SessionQuery { role: String, token: String }

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
        if role == "agent" { session.agent_tx = Some(tx.clone()); }
        else { session.controller_tx = Some(tx.clone()); }
        info!(%session_id, %role, device_id=%session.device_id, "session peer connected");
    } else { return; }

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
        if role == "agent" { session.agent_tx = None; }
        else { session.controller_tx = None; }
        if session.agent_tx.is_none() && session.controller_tx.is_none() {
            drop(session);
            state.sessions.remove(&session_id);
            if let Ok(db) = state.db.lock() {
                let _ = db.execute("UPDATE sessions SET state='closed' WHERE session_id=?1", params![session_id.to_string()]);
            }
        }
    }
    writer.abort();
    info!(%session_id, %role, "session peer disconnected");
}
