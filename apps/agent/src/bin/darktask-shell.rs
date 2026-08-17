#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod win {
    use std::{mem, ptr, sync::OnceLock, time::Instant};

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{
                CreateProcessW, OpenEventW, SetEvent, CREATE_NEW_CONSOLE, EVENT_MODIFY_STATE,
                PROCESS_INFORMATION, STARTUPINFOW,
            },
        },
        UI::WindowsAndMessaging::*,
    };

    const DARKTASK_INPUT_MARKER: usize = 0x44544B31; // "DTK1"

    const ID_START: usize = 100;
    const ID_EXPLORER: usize = 101;
    const ID_POWERSHELL: usize = 102;
    const ID_CMD: usize = 103;
    const ID_TASKMGR: usize = 104;
    const ID_SETTINGS: usize = 105;
    const ID_NOTEPAD: usize = 106;
    const ID_RUN: usize = 107;
    const ID_CLOSE_SESSION: usize = 108;

    const TIMER_ELAPSED: usize = 1;
    const WDA_EXCLUDEFROMCAPTURE_VALUE: u32 = 0x00000011;

    static CLOSE_EVENT_NAME: OnceLock<String> = OnceLock::new();
    static SESSION_STARTED: OnceLock<Instant> = OnceLock::new();

    static mut MENU: HWND = ptr::null_mut();
    static mut OVERLAY: HWND = ptr::null_mut();
    static mut TIMER_LABEL: HWND = ptr::null_mut();
    static mut MOUSE_HOOK: HHOOK = ptr::null_mut();
    static mut KEYBOARD_HOOK: HHOOK = ptr::null_mut();

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    unsafe fn launch(command: &str) {
        let mut cmd = wide(command);
        let mut si: STARTUPINFOW = mem::zeroed();
        si.cb = mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = mem::zeroed();

        if CreateProcessW(
            ptr::null(),
            cmd.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            CREATE_NEW_CONSOLE,
            ptr::null(),
            ptr::null(),
            &si,
            &mut pi,
        ) != 0
        {
            CloseHandle(pi.hThread);
            CloseHandle(pi.hProcess);
        }
    }

    unsafe fn button(
        parent: HWND,
        id: usize,
        text: &str,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    ) -> HWND {
        CreateWindowExW(
            0,
            wide("BUTTON").as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | BS_PUSHBUTTON as u32,
            x,
            y,
            w,
            h,
            parent,
            id as _,
            GetModuleHandleW(ptr::null()),
            ptr::null_mut(),
        )
    }

    unsafe fn label(
        parent: HWND,
        text: &str,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        style: u32,
    ) -> HWND {
        CreateWindowExW(
            0,
            wide("STATIC").as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            x,
            y,
            w,
            h,
            parent,
            ptr::null_mut(),
            GetModuleHandleW(ptr::null()),
            ptr::null_mut(),
        )
    }

    unsafe fn request_close_session(hwnd: HWND) {
        let answer = MessageBoxW(
            hwnd,
            wide("End this DarkTask remote session and return to the normal desktop?").as_ptr(),
            wide("Close DarkTask Session").as_ptr(),
            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
        );
        if answer != IDYES {
            return;
        }

        let Some(event_name) = CLOSE_EVENT_NAME.get() else {
            return;
        };

        let event_name_w = wide(event_name);
        let event = OpenEventW(EVENT_MODIFY_STATE, 0, event_name_w.as_ptr());
        if event.is_null() {
            return;
        }

        let _ = SetEvent(event);
        CloseHandle(event);
    }

    unsafe extern "system" fn mouse_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
        if code >= 0 {
            let info = &*(l as *const MSLLHOOKSTRUCT);
            if info.dwExtraInfo != DARKTASK_INPUT_MARKER {
                return 1;
            }
        }
        CallNextHookEx(MOUSE_HOOK, code, w, l)
    }

    unsafe extern "system" fn keyboard_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
        if code >= 0 {
            let info = &*(l as *const KBDLLHOOKSTRUCT);
            if info.dwExtraInfo != DARKTASK_INPUT_MARKER {
                return 1;
            }
        }
        CallNextHookEx(KEYBOARD_HOOK, code, w, l)
    }

    unsafe fn install_local_input_block() {
        let module = GetModuleHandleW(ptr::null());
        MOUSE_HOOK = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0);
        KEYBOARD_HOOK = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), module, 0);
    }

    unsafe fn uninstall_local_input_block() {
        if !MOUSE_HOOK.is_null() {
            UnhookWindowsHookEx(MOUSE_HOOK);
            MOUSE_HOOK = ptr::null_mut();
        }
        if !KEYBOARD_HOOK.is_null() {
            UnhookWindowsHookEx(KEYBOARD_HOOK);
            KEYBOARD_HOOK = ptr::null_mut();
        }
    }

    unsafe extern "system" fn menu_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        match msg {
            WM_COMMAND => {
                match (w & 0xffff) as usize {
                    ID_EXPLORER => launch("explorer.exe"),
                    ID_POWERSHELL => launch("powershell.exe"),
                    ID_CMD => launch("cmd.exe"),
                    ID_TASKMGR => launch("taskmgr.exe"),
                    ID_SETTINGS => launch("cmd.exe /C start ms-settings:"),
                    ID_NOTEPAD => launch("notepad.exe"),
                    ID_RUN => launch("powershell.exe -NoExit"),
                    _ => {}
                }
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_CLOSE => {
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }

    unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        match msg {
            WM_CREATE => {
                let sw = GetSystemMetrics(SM_CXSCREEN);
                button(hwnd, ID_START, "DarkTask", 8, 7, 94, 34);
                label(hwnd, "Admin Workspace", 116, 14, 220, 24, 0u32);
                button(
                    hwnd,
                    ID_CLOSE_SESSION,
                    "Close Session",
                    sw - 154,
                    7,
                    146,
                    34,
                );
                0
            }
            WM_COMMAND => {
                match (w & 0xffff) as usize {
                    ID_START => {
                        if !MENU.is_null() {
                            let vis = IsWindowVisible(MENU);
                            ShowWindow(MENU, if vis != 0 { SW_HIDE } else { SW_SHOW });
                            if vis == 0 {
                                SetForegroundWindow(MENU);
                            }
                        }
                    }
                    ID_CLOSE_SESSION => request_close_session(hwnd),
                    _ => {}
                }
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }

    unsafe fn update_elapsed() {
        if TIMER_LABEL.is_null() {
            return;
        }

        let elapsed = SESSION_STARTED
            .get()
            .map(|started| started.elapsed().as_secs())
            .unwrap_or(0);

        let mins = elapsed / 60;
        let secs = elapsed % 60;
        let status = format!("Maintenance session active  -  {:02}:{:02}", mins, secs);

        SetWindowTextW(TIMER_LABEL, wide(&status).as_ptr());
    }

    unsafe extern "system" fn overlay_proc(
        hwnd: HWND,
        msg: u32,
        _w: WPARAM,
        l: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CREATE => {
                let sw = GetSystemMetrics(SM_CXSCREEN);
                let sh = GetSystemMetrics(SM_CYSCREEN);

                let width = 1000;
                let left = ((sw - width) / 2).max(20);
                let top = (sh / 2) - 190;

                label(
                    hwnd,
                    "System maintenance in progress",
                    left,
                    top,
                    width,
                    48,
                    1u32,
                );

                label(
                    hwnd,
                    "A system administrator is currently connected remotely and performing maintenance on this computer.",
                    left,
                    top + 82,
                    width,
                    32,
                    1u32,
                );

                label(
                    hwnd,
                    "To prevent interruptions or incomplete updates, this computer is temporarily unavailable.",
                    left,
                    top + 122,
                    width,
                    32,
                    1u32,
                );

                label(
                    hwnd,
                    "Please try again in approximately 10 minutes.",
                    left,
                    top + 162,
                    width,
                    32,
                    1u32,
                );

                label(
                    hwnd,
                    "DarkTask Remote Administration",
                    left,
                    top + 238,
                    width,
                    28,
                    1u32,
                );

                TIMER_LABEL = label(
                    hwnd,
                    "Maintenance session active  -  00:00",
                    left,
                    top + 280,
                    width,
                    28,
                    1u32,
                );

                SetTimer(hwnd, TIMER_ELAPSED, 1000, None);
                0
            }

            WM_TIMER => {
                update_elapsed();
                0
            }

            WM_NCHITTEST => HTTRANSPARENT as LRESULT,

            WM_CLOSE => 0,

            WM_DESTROY => {
                KillTimer(hwnd, TIMER_ELAPSED);
                0
            }

            _ => DefWindowProcW(hwnd, msg, _w, l),
        }
    }

    pub unsafe fn run() {
        let args: Vec<String> = std::env::args().collect();
        if let Some(pos) = args.iter().position(|arg| arg == "--close-event") {
            if let Some(name) = args.get(pos + 1) {
                let _ = CLOSE_EVENT_NAME.set(name.clone());
            }
        }

        let _ = SESSION_STARTED.set(Instant::now());

        let h = GetModuleHandleW(ptr::null());

        let bc = wide("DarkTaskShellBar");
        let mc = wide("DarkTaskShellMenu");
        let oc = wide("DarkTaskMaintenanceOverlay");

        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(bar_proc),
            hInstance: h,
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            hbrBackground: ptr::null_mut(),
            lpszClassName: bc.as_ptr(),
            ..mem::zeroed()
        });

        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(menu_proc),
            hInstance: h,
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            hbrBackground: ptr::null_mut(),
            lpszClassName: mc.as_ptr(),
            ..mem::zeroed()
        });

        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(overlay_proc),
            hInstance: h,
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            hbrBackground: ptr::null_mut(),
            lpszClassName: oc.as_ptr(),
            ..mem::zeroed()
        });

        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        let bh = 48;

        let bar = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            bc.as_ptr(),
            wide("DarkTask Shell").as_ptr(),
            WS_POPUP | WS_VISIBLE,
            0,
            sh - bh,
            sw,
            bh,
            ptr::null_mut(),
            ptr::null_mut(),
            h,
            ptr::null_mut(),
        );

        MENU = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            mc.as_ptr(),
            wide("DarkTask Start").as_ptr(),
            WS_POPUP | WS_BORDER,
            8,
            sh - bh - 330,
            270,
            320,
            bar,
            ptr::null_mut(),
            h,
            ptr::null_mut(),
        );

        label(MENU, "DarkTask", 18, 14, 220, 28, 0u32);
        button(MENU, ID_EXPLORER, "File Explorer", 18, 50, 234, 32);
        button(MENU, ID_POWERSHELL, "PowerShell", 18, 88, 234, 32);
        button(MENU, ID_CMD, "Command Prompt", 18, 126, 234, 32);
        button(MENU, ID_TASKMGR, "Task Manager", 18, 164, 234, 32);
        button(MENU, ID_SETTINGS, "Windows Settings", 18, 202, 234, 32);
        button(MENU, ID_NOTEPAD, "Notepad", 18, 240, 112, 32);
        button(MENU, ID_RUN, "Run", 140, 240, 112, 32);

        OVERLAY = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            oc.as_ptr(),
            wide("System maintenance in progress").as_ptr(),
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            sw,
            sh,
            ptr::null_mut(),
            ptr::null_mut(),
            h,
            ptr::null_mut(),
        );

        let _ = SetWindowDisplayAffinity(OVERLAY, WDA_EXCLUDEFROMCAPTURE_VALUE);

        install_local_input_block();

        ShowWindow(bar, SW_SHOW);
        ShowWindow(OVERLAY, SW_SHOW);
        SetForegroundWindow(OVERLAY);

        let mut msg: MSG = mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        uninstall_local_input_block();
    }
}

#[cfg(windows)]
fn main() {
    unsafe {
        win::run();
    }
}

