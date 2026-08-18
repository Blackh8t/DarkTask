use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use remote_protocol::{
    AgentHello, AgentToServer, ControlMessage, EnrollRequest, EnrollResponse, Heartbeat,
    MouseButton, ServerToAgent, SessionMode, SessionPeek, DEFAULT_JPEG_QUALITY, DEFAULT_STREAM_FPS,
    FRAME_COMPRESS_JPEG, FRAME_HEADER_LEN, FRAME_MAGIC, FRAME_PIXEL_JPEG, MAX_STREAM_FPS,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};
use uuid::Uuid;

#[cfg(windows)]
use jpeg_encoder::{ColorType, Encoder};

#[cfg(windows)]
use std::{ffi::{OsStr, OsString}, mem, os::windows::ffi::OsStrExt, ptr};

#[cfg(windows)]
use windows_service::{
    define_windows_service,
    service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
    service_manager::{ServiceManager, ServiceManagerAccess},
};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Graphics::Gdi::*,
    Security::{
        DuplicateTokenEx, ImpersonateLoggedOnUser, RevertToSelf, SecurityImpersonation,
        TokenPrimary, TOKEN_ALL_ACCESS,
    },
    System::{
        Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock},
        RemoteDesktop::{
            WTSActive, WTSConnectState, WTSGetActiveConsoleSessionId, WTSQuerySessionInformationW,
            WTSQueryUserToken, WTSUserName, WTS_CURRENT_SERVER_HANDLE,
        },
        Threading::{
            CreateProcessAsUserW, CREATE_UNICODE_ENVIRONMENT,
            PROCESS_INFORMATION, STARTUPINFOW,
        },
        SystemInformation::GetTickCount,
    },
    UI::{
        Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO, *},
        WindowsAndMessaging::*,
    },
};

const SERVICE_NAME: &str = "DarkTaskAgent";
const APP_DIR: &str = "DarkTask";
const INSTALL_DIR: &str = r"C:\Program Files\DarkTask";
const INSTALLED_EXE: &str = r"C:\Program Files\DarkTask\remote-agent.exe";
const USER_DESKTOP: &str = r"winsta0\default";
/// Presence ping interval while idle (no remote session).
const HEARTBEAT_SECS: u64 = 60;
const STOP_POLL_SECS: u64 = 5;
const RECONNECT_SECS: u64 = 5;

static HOSTNAME: OnceLock<String> = OnceLock::new();

#[derive(Parser, Debug)]
#[command(version, about = "DarkTask managed remote access agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run interactively for development/testing.
    Run {
        #[arg(long)]
        server: String,
        #[arg(long)]
        enroll: String,
        #[arg(long, default_value_t = DEFAULT_STREAM_FPS)]
        max_fps: u16,
        #[arg(long)]
        reset_identity: bool,
    },

    /// Install this executable as the DarkTask Windows service.
    Install {
        #[arg(long, default_value = "http://62.72.31.30:8789")]
        server: String,
        #[arg(long)]
        enroll: String,
        #[arg(long, default_value_t = DEFAULT_STREAM_FPS)]
        max_fps: u16,
        /// Start the service immediately after installation.
        #[arg(long, default_value_t = true)]
        start: bool,
    },

    /// Remove the DarkTask Windows service registration.
    Uninstall {
        /// Also remove saved configuration and enrolled device identity.
        #[arg(long)]
        purge: bool,
    },

    /// Show Windows service and local identity/config status.
    Status,

    /// Entry point used by Windows Service Control Manager.
    Service,

    /// Per-user interactive worker launched by the service for a remote session.
    Worker {
        #[arg(long)]
        server: String,
        #[arg(long)]
        session_id: Uuid,
        #[arg(long)]
        session_token: String,
        #[arg(long, default_value_t = DEFAULT_STREAM_FPS)]
        max_fps: u16,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentConfig {
    server: String,
    enroll: String,
    #[serde(default = "default_fps")]
    max_fps: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Identity {
    device_id: Uuid,
    device_token: String,
}

fn default_fps() -> u16 { DEFAULT_STREAM_FPS }

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn program_data_dir() -> PathBuf {
    std::env::var_os("PROGRAMDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join(APP_DIR)
}

fn config_path() -> PathBuf {
    program_data_dir().join("agent-config.json")
}

fn identity_path() -> PathBuf {
    program_data_dir().join("identity.json")
}

fn hostname() -> &'static str {
    HOSTNAME.get_or_init(|| {
        std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown-host".into())
    })
}

fn ws_url(server: &str, path: &str) -> String {
    let base = server.replace("https://", "wss://").replace("http://", "ws://");
    format!("{}{}", base.trim_end_matches('/'), path)
}

fn read_config() -> Result<AgentConfig> {
    let path = config_path();
    let raw = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(serde_json::from_slice(&raw)?)
}

fn load_or_enroll(config: &AgentConfig, reset_identity: bool) -> Result<Identity> {
    let path = identity_path();

    if reset_identity && path.exists() {
        fs::remove_file(&path)?;
    }

    if path.exists() {
        return Ok(serde_json::from_slice(&fs::read(&path)?)?);
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let identity = rt.block_on(async {
        let client = reqwest::Client::new();
        let req = EnrollRequest {
            enrollment_token: config.enroll.clone(),
            hostname: hostname().into(),
            platform: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            agent_version: env!("CARGO_PKG_VERSION").into(),
        };

        let resp = client
            .post(format!("{}/api/v1/enroll", config.server.trim_end_matches('/')))
            .json(&req)
            .send()
            .await?
            .error_for_status()?
            .json::<EnrollResponse>()
            .await?;

        Ok::<Identity, anyhow::Error>(Identity {
            device_id: resp.device_id,
            device_token: resp.device_token,
        })
    })?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_vec_pretty(&identity)?)?;
    Ok(identity)
}

#[cfg(windows)]
fn install_service(config: AgentConfig, start_now: bool) -> Result<()> {
    fs::create_dir_all(INSTALL_DIR)?;
    fs::create_dir_all(program_data_dir())?;

    let current_exe = std::env::current_exe()?;
    let installed_exe = PathBuf::from(INSTALLED_EXE);

    if current_exe != installed_exe {
        fs::copy(&current_exe, &installed_exe)
            .with_context(|| format!("copy {} -> {}", current_exe.display(), installed_exe.display()))?;
    }

    fs::write(config_path(), serde_json::to_vec_pretty(&config)?)?;

    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;

    let service_info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from("DarkTask Remote Agent"),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: installed_exe,
        launch_arguments: vec![OsString::from("service")],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };

    let access = ServiceAccess::QUERY_STATUS
        | ServiceAccess::START
        | ServiceAccess::STOP
        | ServiceAccess::CHANGE_CONFIG
        | ServiceAccess::DELETE;

    let service = match manager.create_service(&service_info, access) {
        Ok(s) => s,
        Err(_) => {
            let existing = manager.open_service(SERVICE_NAME, access)?;
            let _ = existing.stop();
            existing.delete()?;
            drop(existing);
            std::thread::sleep(Duration::from_millis(750));
            manager.create_service(&service_info, access)?
        }
    };

    service.set_description("DarkTask managed remote access agent")?;
    service.set_delayed_auto_start(false)?;

    if start_now {
        service.start(&[] as &[&OsStr])?;
    }

    println!("DarkTaskAgent installed.");
    println!("Binary : {}", INSTALLED_EXE);
    println!("Config : {}", config_path().display());
    if start_now {
        println!("Service: start requested");
    }
    Ok(())
}

#[cfg(windows)]
fn uninstall_service(purge: bool) -> Result<()> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT,
    )?;

    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    let service = manager.open_service(SERVICE_NAME, access)?;

    if service.query_status()?.current_state != ServiceState::Stopped {
        let _ = service.stop();
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(250));
            if service.query_status()?.current_state == ServiceState::Stopped {
                break;
            }
        }
    }

    service.delete()?;
    drop(service);
    println!("DarkTaskAgent service removed.");

    if purge {
        let data = program_data_dir();
        if data.exists() {
            fs::remove_dir_all(&data)?;
        }
        println!("Removed {}", data.display());
    } else {
        println!("Saved identity/config preserved in {}", program_data_dir().display());
    }

    Ok(())
}

#[cfg(windows)]
fn show_status() -> Result<()> {
    println!("DarkTask");
    println!("-------");
    println!("Installed binary : {}", INSTALLED_EXE);
    println!("Config path      : {}", config_path().display());
    println!("Identity path    : {}", identity_path().display());
    println!("Config exists    : {}", config_path().exists());
    println!("Identity exists  : {}", identity_path().exists());

    if let Ok(config) = read_config() {
        println!("Server           : {}", config.server);
        println!("Max FPS          : {}", config.max_fps);
        println!("Enroll token     : configured");
    }

    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT,
    )?;

    match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
        Ok(service) => {
            let status = service.query_status()?;
            println!("Service          : {:?}", status.current_state);
            if let Some(pid) = status.process_id {
                println!("Service PID      : {}", pid);
            }
        }
        Err(_) => println!("Service          : not installed"),
    }

    Ok(())
}

#[cfg(not(windows))]
fn main() -> Result<()> {
    Err(anyhow!("remote-agent service is Windows-only"))
}

#[cfg(windows)]
define_windows_service!(ffi_service_main, service_main);

#[cfg(windows)]
fn init_tracing(command: &Option<Command>) {
    match command {
        None | Some(Command::Service) | Some(Command::Worker { .. }) => {}
        _ => {
            tracing_subscriber::fmt()
                .with_env_filter("remote_agent=info")
                .init();
        }
    }
}

#[cfg(windows)]
fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(&cli.command);

    match cli.command.unwrap_or(Command::Service) {
        Command::Install { server, enroll, max_fps, start } => {
            install_service(AgentConfig { server, enroll, max_fps }, start)
        }

        Command::Uninstall { purge } => uninstall_service(purge),

        Command::Status => show_status(),

        Command::Run { server, enroll, max_fps, reset_identity } => {
            let config = AgentConfig { server, enroll, max_fps };
            let identity = load_or_enroll(&config, reset_identity)?;
            let stop = Arc::new(AtomicBool::new(false));
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(run_control_plane(config, identity, stop))
        }

        Command::Service => {
            service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
            Ok(())
        }

        Command::Worker { server, session_id, session_token, max_fps } => {
            run_worker(server, session_id, session_token, max_fps)
        }
    }
}

#[cfg(windows)]
fn service_main(_arguments: Vec<OsString>) {
    if let Err(e) = run_windows_service() {
        eprintln!("DarkTask service failed: {e:#}");
    }
}

#[cfg(windows)]
fn run_windows_service() -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_handler = stop.clone();

    let status_handle = service_control_handler::register(
        SERVICE_NAME,
        move |event| -> ServiceControlHandlerResult {
            match event {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    stop_for_handler.store(true, Ordering::SeqCst);
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        },
    )?;

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::StartPending,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 1,
        wait_hint: Duration::from_secs(10),
        process_id: None,
    })?;

    let config = read_config()?;
    let identity = load_or_enroll(&config, false)?;

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let result = rt.block_on(run_control_plane(config, identity, stop.clone()));

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: if result.is_ok() {
            ServiceExitCode::Win32(0)
        } else {
            ServiceExitCode::ServiceSpecific(1)
        },
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    result
}

#[cfg(windows)]
fn console_user_logged_in(session_id: u32) -> bool {
    unsafe {
        use std::ffi::c_void;
        use windows_sys::Win32::System::RemoteDesktop::WTSFreeMemory;

        let mut buffer: *mut c_void = ptr::null_mut();
        let mut bytes_returned = 0u32;
        if WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            session_id,
            WTSUserName,
            &mut buffer as *mut _ as *mut _,
            &mut bytes_returned,
        ) == 0
            || buffer.is_null()
        {
            return false;
        }

        let logged_in = *(buffer as *const u16) != 0;
        WTSFreeMemory(buffer);
        if !logged_in {
            return false;
        }

        buffer = ptr::null_mut();
        bytes_returned = 0;
        if WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            session_id,
            WTSConnectState,
            &mut buffer as *mut _ as *mut _,
            &mut bytes_returned,
        ) == 0
            || buffer.is_null()
        {
            return logged_in;
        }

        let active = *(buffer as *const u32) == WTSActive as u32;
        WTSFreeMemory(buffer);
        active
    }
}

#[cfg(windows)]
fn query_console_idle_secs(session_id: u32) -> Option<u64> {
    unsafe {
        let mut user_token: HANDLE = ptr::null_mut();
        if WTSQueryUserToken(session_id, &mut user_token) == 0 {
            return None;
        }

        if ImpersonateLoggedOnUser(user_token) == 0 {
            CloseHandle(user_token);
            return None;
        }

        let mut info: LASTINPUTINFO = mem::zeroed();
        info.cbSize = mem::size_of::<LASTINPUTINFO>() as u32;
        let ok = GetLastInputInfo(&mut info);
        RevertToSelf();
        CloseHandle(user_token);

        if ok == 0 {
            return None;
        }

        let elapsed_ms = GetTickCount().wrapping_sub(info.dwTime);
        Some((elapsed_ms / 1000) as u64)
    }
}

#[cfg(windows)]
fn collect_session_peek() -> Option<SessionPeek> {
    unsafe {
        let session_id = WTSGetActiveConsoleSessionId();
        if session_id == u32::MAX {
            return Some(SessionPeek {
                user_logged_in: false,
                idle_secs: None,
            });
        }

        let user_logged_in = console_user_logged_in(session_id);
        let idle_secs = if user_logged_in {
            query_console_idle_secs(session_id)
        } else {
            None
        };

        Some(SessionPeek {
            user_logged_in,
            idle_secs,
        })
    }
}

#[cfg(not(windows))]
fn collect_session_peek() -> Option<SessionPeek> {
    None
}

async fn run_control_plane(
    config: AgentConfig,
    identity: Identity,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    while !stop.load(Ordering::SeqCst) {
        if let Err(e) = run_connection(&config, &identity, stop.clone()).await {
            warn!(error=%e, "control-plane connection failed");
        }

        if !stop.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_secs(RECONNECT_SECS)).await;
        }
    }
    Ok(())
}

async fn run_connection(
    config: &AgentConfig,
    identity: &Identity,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let url = ws_url(&config.server, "/ws/agent");
    let (ws, _) = connect_async(&url).await.context("connect agent websocket")?;
    let (mut tx, mut rx) = ws.split();

    let hello = AgentToServer::Hello(AgentHello {
        device_id: identity.device_id,
        device_token: identity.device_token.clone(),
        hostname: hostname().into(),
        agent_version: env!("CARGO_PKG_VERSION").into(),
    });

    tx.send(Message::Text(serde_json::to_string(&hello)?.into())).await?;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut stop_check = tokio::time::interval(Duration::from_secs(STOP_POLL_SECS));
    stop_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // First tick fires immediately; skip so hello isn't followed by instant peek work.
    heartbeat.tick().await;

    loop {
        tokio::select! {
            _ = stop_check.tick() => {
                if stop.load(Ordering::SeqCst) {
                    let _ = tx.send(Message::Close(None)).await;
                    break;
                }
            }

            _ = heartbeat.tick() => {
                let hb = AgentToServer::Heartbeat(Heartbeat {
                    device_id: identity.device_id,
                    unix_ms: now_ms(),
                    session_peek: collect_session_peek(),
                });
                tx.send(Message::Text(serde_json::to_string(&hb)?.into())).await?;
            }

            msg = rx.next() => {
                let Some(msg) = msg else { break };
                let msg = msg?;
                let Message::Text(text) = msg else { continue };
                let server_msg: ServerToAgent = serde_json::from_str(&text)?;

                match server_msg {
                    ServerToAgent::HelloAck => {
                        info!(device_id=%identity.device_id, "service connected");
                    }

                    ServerToAgent::StartSession {
                        session_id,
                        controller_id,
                        session_token,
                        session_mode,
                    } => {
                        info!(%session_id, %controller_id, ?session_mode, "remote session requested");

                        if session_mode == SessionMode::AdminWorkspace {
                            let reply = AgentToServer::SessionRejected {
                                session_id,
                                reason: "admin workspace is not yet supported; connect with user_screen".into(),
                            };
                            tx.send(Message::Text(serde_json::to_string(&reply)?.into())).await?;
                            continue;
                        }

                        match spawn_interactive_worker(
                            &config.server,
                            session_id,
                            &session_token,
                            config.max_fps,
                        ) {
                            Ok(()) => {
                                let reply = AgentToServer::SessionAccepted { session_id };
                                tx.send(Message::Text(serde_json::to_string(&reply)?.into())).await?;
                            }
                            Err(e) => {
                                warn!(%session_id, error=%e, "failed to start interactive worker");
                                let reply = AgentToServer::SessionRejected {
                                    session_id,
                                    reason: e.to_string(),
                                };
                                tx.send(Message::Text(serde_json::to_string(&reply)?.into())).await?;
                            }
                        }
                    }

                    ServerToAgent::Ping => {}
                }
            }
        }
    }

    Ok(())
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

#[cfg(windows)]
fn quote_arg(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\\\""))
}

#[cfg(windows)]
fn spawn_interactive_worker(
    server: &str,
    session_id: Uuid,
    session_token: &str,
    max_fps: u16,
) -> Result<()> {
    unsafe {
        let session_id_os = WTSGetActiveConsoleSessionId();
        if session_id_os == u32::MAX {
            return Err(anyhow!("no active interactive Windows session"));
        }

        let mut user_token: HANDLE = ptr::null_mut();
        if WTSQueryUserToken(session_id_os, &mut user_token) == 0 {
            return Err(anyhow!("WTSQueryUserToken failed; service must run as LocalSystem"));
        }

        let mut primary_token: HANDLE = ptr::null_mut();
        if DuplicateTokenEx(
            user_token,
            TOKEN_ALL_ACCESS,
            ptr::null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut primary_token,
        ) == 0 {
            CloseHandle(user_token);
            return Err(anyhow!("DuplicateTokenEx failed"));
        }

        let exe = std::env::current_exe()?;
        let command = format!(
            "{} worker --server {} --session-id {} --session-token {} --max-fps {}",
            quote_arg(&exe.to_string_lossy()),
            quote_arg(server),
            session_id,
            quote_arg(session_token),
            max_fps,
        );

        let mut command_w = wide(&command);
        let desktop_w = wide(USER_DESKTOP);

        let mut startup: STARTUPINFOW = mem::zeroed();
        startup.cb = mem::size_of::<STARTUPINFOW>() as u32;
        startup.lpDesktop = desktop_w.as_ptr() as *mut u16;

        let mut process: PROCESS_INFORMATION = mem::zeroed();
        let mut env: *mut core::ffi::c_void = ptr::null_mut();

        if CreateEnvironmentBlock(&mut env, primary_token, 0) == 0 {
            CloseHandle(primary_token);
            CloseHandle(user_token);
            return Err(anyhow!("CreateEnvironmentBlock failed"));
        }

        let ok = CreateProcessAsUserW(
            primary_token,
            ptr::null(),
            command_w.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            CREATE_UNICODE_ENVIRONMENT,
            env,
            ptr::null(),
            &startup,
            &mut process,
        );

        DestroyEnvironmentBlock(env);
        CloseHandle(primary_token);
        CloseHandle(user_token);

        if ok == 0 {
            return Err(anyhow!("CreateProcessAsUserW failed"));
        }

        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
        Ok(())
    }
}

#[cfg(not(windows))]
fn spawn_interactive_worker(
    _server: &str,
    _session_id: Uuid,
    _session_token: &str,
    _max_fps: u16,
) -> Result<()> {
    Err(anyhow!("interactive worker is Windows-only"))
}

#[cfg(windows)]
fn run_worker(
    server: String,
    session_id: Uuid,
    session_token: String,
    max_fps: u16,
) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(run_remote_session(server, session_id, session_token, max_fps))
}

#[cfg(not(windows))]
fn run_worker(
    _server: String,
    _session_id: Uuid,
    _session_token: String,
    _max_fps: u16,
) -> Result<()> {
    Err(anyhow!("worker is Windows-only"))
}

#[cfg(windows)]
struct FrameCapture {
    width: i32,
    height: i32,
    screen: HDC,
    mem_dc: HDC,
    bitmap: HGDIOBJ,
    pixels: Vec<u8>,
    jpeg_buf: Vec<u8>,
    quality: u8,
    info: BITMAPINFO,
}

#[cfg(windows)]
impl FrameCapture {
    fn new(width: i32, height: i32) -> Result<Self> {
        unsafe {
            let screen = GetDC(ptr::null_mut());
            if screen.is_null() {
                return Err(anyhow!("GetDC failed"));
            }

            let mem_dc = CreateCompatibleDC(screen);
            if mem_dc.is_null() {
                ReleaseDC(ptr::null_mut(), screen);
                return Err(anyhow!("CreateCompatibleDC failed"));
            }

            let bitmap = CreateCompatibleBitmap(screen, width, height);
            if bitmap.is_null() {
                DeleteDC(mem_dc);
                ReleaseDC(ptr::null_mut(), screen);
                return Err(anyhow!("CreateCompatibleBitmap failed"));
            }

            SelectObject(mem_dc, bitmap as _);

            let mut info: BITMAPINFO = mem::zeroed();
            info.bmiHeader.biSize = mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = width;
            info.bmiHeader.biHeight = -height;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;

            Ok(Self {
                width,
                height,
                screen,
                mem_dc,
                bitmap,
                pixels: vec![0u8; width as usize * height as usize * 4],
                jpeg_buf: Vec::with_capacity(64 * 1024),
                quality: DEFAULT_JPEG_QUALITY,
                info,
            })
        }
    }

    fn capture(&mut self) -> Result<Vec<u8>> {
        unsafe {
            let width = GetSystemMetrics(SM_CXSCREEN);
            let height = GetSystemMetrics(SM_CYSCREEN);
            if width <= 0 || height <= 0 {
                return Err(anyhow!("invalid desktop dimensions"));
            }
            if width != self.width || height != self.height {
                *self = Self::new(width, height)?;
            }

            if BitBlt(
                self.mem_dc,
                0,
                0,
                self.width,
                self.height,
                self.screen,
                0,
                0,
                SRCCOPY,
            ) == 0
            {
                return Err(anyhow!("BitBlt failed"));
            }

            let rows = GetDIBits(
                self.mem_dc,
                self.bitmap as _,
                0,
                self.height as u32,
                self.pixels.as_mut_ptr() as *mut _,
                &mut self.info,
                DIB_RGB_COLORS,
            );
            if rows == 0 {
                return Err(anyhow!("GetDIBits failed"));
            }

            self.jpeg_buf.clear();
            let encoder = Encoder::new(&mut self.jpeg_buf, self.quality);
            encoder
                .encode(
                    &self.pixels,
                    self.width as u16,
                    self.height as u16,
                    ColorType::Bgra,
                )
                .map_err(|e| anyhow!("jpeg encode failed: {e}"))?;

            let mut out = Vec::with_capacity(FRAME_HEADER_LEN + self.jpeg_buf.len());
            out.extend_from_slice(FRAME_MAGIC);
            out.extend_from_slice(&(self.width as u32).to_le_bytes());
            out.extend_from_slice(&(self.height as u32).to_le_bytes());
            out.push(FRAME_PIXEL_JPEG);
            out.push(FRAME_COMPRESS_JPEG);
            out.push(self.quality);
            out.push(0);
            out.extend_from_slice(&self.jpeg_buf);
            Ok(out)
        }
    }
}

#[cfg(windows)]
impl Drop for FrameCapture {
    fn drop(&mut self) {
        unsafe {
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap as _);
            }
            if !self.mem_dc.is_null() {
                DeleteDC(self.mem_dc);
            }
            if !self.screen.is_null() {
                ReleaseDC(ptr::null_mut(), self.screen);
            }
        }
    }
}

#[cfg(windows)]
async fn run_remote_session(
    server: String,
    session_id: Uuid,
    session_token: String,
    max_fps: u16,
) -> Result<()> {
    let url = ws_url(
        &server,
        &format!(
            "/ws/session/{session_id}?role=agent&token={session_token}"
        ),
    );

    let (ws, _) = connect_async(&url)
        .await
        .context("connect session websocket")?;

    let (mut tx, mut rx) = ws.split();
    let (frame_tx, frame_rx) = watch::channel(Vec::new());

    let fps = max_fps.clamp(8, MAX_STREAM_FPS);
    let mut ticker =
        tokio::time::interval(Duration::from_millis(1000 / fps as u64));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let mut capture = FrameCapture::new(width, height)?;

    let send_task = tokio::spawn(async move {
        let mut frame_rx = frame_rx;
        loop {
            if frame_rx.changed().await.is_err() {
                break;
            }
            let frame = frame_rx.borrow().clone();
            if frame.is_empty() {
                continue;
            }
            if tx.send(Message::Binary(frame.into())).await.is_err() {
                break;
            }
        }
    });

    loop {
        tokio::select! {
            biased;

            msg = rx.next() => {
                let Some(msg) = msg else { break };
                match msg? {
                    Message::Text(text) => {
                        if let Ok(control) = serde_json::from_str::<ControlMessage>(&text) {
                            apply_control(control)?;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }

            _ = ticker.tick() => {
                match capture.capture() {
                    Ok(frame) => {
                        let _ = frame_tx.send(frame);
                    }
                    Err(e) => warn!(error=%e, "frame capture failed"),
                }
            }
        }
    }

    send_task.abort();
    Ok(())
}

#[cfg(windows)]
fn apply_control(msg: ControlMessage) -> Result<()> {
    unsafe {
        match msg {
            ControlMessage::MouseMove { x_norm, y_norm } => {
                let w = GetSystemMetrics(SM_CXSCREEN).max(1);
                let h = GetSystemMetrics(SM_CYSCREEN).max(1);

                let x =
                    (x_norm.clamp(0.0, 1.0) * (w - 1) as f32) as i32;
                let y =
                    (y_norm.clamp(0.0, 1.0) * (h - 1) as f32) as i32;

                if SetCursorPos(x, y) == 0 {
                    return Err(anyhow!("SetCursorPos failed"));
                }
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

            ControlMessage::MouseWheel { delta } => {
                mouse_event(MOUSEEVENTF_WHEEL, 0, 0, delta, 0);
            }

            ControlMessage::Key { vk, down } => {
                let flags = if down { 0 } else { KEYEVENTF_KEYUP };
                keybd_event(vk as u8, 0, flags, 0);
            }

            ControlMessage::SetQuality { .. } | ControlMessage::Ping { .. } => {}
        }
    }

    Ok(())
}
