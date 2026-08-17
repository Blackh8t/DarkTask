#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
mod win {
    use std::{mem, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{CreateProcessW, CREATE_NEW_CONSOLE, PROCESS_INFORMATION, STARTUPINFOW},
        },
        UI::WindowsAndMessaging::*,
    };

    const ID_START: usize = 100;
    const ID_EXPLORER: usize = 101;
    const ID_POWERSHELL: usize = 102;
    const ID_CMD: usize = 103;
    const ID_TASKMGR: usize = 104;
    const ID_SETTINGS: usize = 105;
    const ID_NOTEPAD: usize = 106;
    const ID_RUN: usize = 107;

    static mut MENU: HWND = ptr::null_mut();

    fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() }

    unsafe fn launch(command: &str) {
        let mut cmd = wide(command);
        let mut si: STARTUPINFOW = mem::zeroed();
        si.cb = mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = mem::zeroed();
        if CreateProcessW(
            ptr::null(), cmd.as_mut_ptr(), ptr::null(), ptr::null(), 0,
            CREATE_NEW_CONSOLE, ptr::null(), ptr::null(), &si, &mut pi
        ) != 0 {
            CloseHandle(pi.hThread);
            CloseHandle(pi.hProcess);
        }
    }

    unsafe fn button(parent: HWND, id: usize, text: &str, x:i32,y:i32,w:i32,h:i32)->HWND{
        CreateWindowExW(0,wide("BUTTON").as_ptr(),wide(text).as_ptr(),
            WS_CHILD|WS_VISIBLE|BS_PUSHBUTTON as u32,x,y,w,h,parent,id as _,
            GetModuleHandleW(ptr::null()),ptr::null_mut())
    }

    unsafe fn label(parent: HWND, text: &str, x:i32,y:i32,w:i32,h:i32)->HWND{
        CreateWindowExW(0,wide("STATIC").as_ptr(),wide(text).as_ptr(),
            WS_CHILD|WS_VISIBLE,x,y,w,h,parent,ptr::null_mut(),
            GetModuleHandleW(ptr::null()),ptr::null_mut())
    }

    unsafe extern "system" fn menu_proc(hwnd: HWND,msg:u32,w:WPARAM,l:LPARAM)->LRESULT{
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
                ShowWindow(hwnd,SW_HIDE); 0
            }
            WM_CLOSE => { ShowWindow(hwnd,SW_HIDE); 0 }
            _ => DefWindowProcW(hwnd,msg,w,l)
        }
    }

    unsafe extern "system" fn bar_proc(hwnd: HWND,msg:u32,w:WPARAM,l:LPARAM)->LRESULT{
        match msg {
            WM_CREATE => {
                button(hwnd,ID_START,"DarkTask",8,7,94,34);
                label(hwnd,"Admin Workspace",116,14,220,24);
                0
            }
            WM_COMMAND if (w & 0xffff) as usize == ID_START => {
                if !MENU.is_null() {
                    let vis=IsWindowVisible(MENU);
                    ShowWindow(MENU,if vis!=0{SW_HIDE}else{SW_SHOW});
                    if vis==0 { SetForegroundWindow(MENU); }
                }
                0
            }
            WM_DESTROY => { PostQuitMessage(0); 0 }
            _ => DefWindowProcW(hwnd,msg,w,l)
        }
    }

    pub unsafe fn run() {
        let h=GetModuleHandleW(ptr::null());
        let bc = wide("DarkTaskShellBar");
        let mc = wide("DarkTaskShellMenu");
        RegisterClassW(&WNDCLASSW{
            lpfnWndProc:Some(bar_proc),hInstance:h,hCursor:LoadCursorW(ptr::null_mut(),IDC_ARROW),
            hbrBackground:ptr::null_mut(),lpszClassName:bc.as_ptr(),..mem::zeroed()
        });
        RegisterClassW(&WNDCLASSW{
            lpfnWndProc:Some(menu_proc),hInstance:h,hCursor:LoadCursorW(ptr::null_mut(),IDC_ARROW),
            hbrBackground:ptr::null_mut(),lpszClassName:mc.as_ptr(),..mem::zeroed()
        });

        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        let bh = 48;
        let bar=CreateWindowExW(WS_EX_TOPMOST|WS_EX_TOOLWINDOW,bc.as_ptr(),wide("DarkTask Shell").as_ptr(),
            WS_POPUP|WS_VISIBLE,0,sh-bh,sw,bh,ptr::null_mut(),ptr::null_mut(),h,ptr::null_mut());

        MENU=CreateWindowExW(WS_EX_TOPMOST|WS_EX_TOOLWINDOW,mc.as_ptr(),wide("DarkTask Start").as_ptr(),
            WS_POPUP|WS_BORDER,8,sh-bh-330,270,320,bar,ptr::null_mut(),h,ptr::null_mut());

        label(MENU,"DarkTask",18,14,220,28);
        button(MENU,ID_EXPLORER,"File Explorer",18,50,234,32);
        button(MENU,ID_POWERSHELL,"PowerShell",18,88,234,32);
        button(MENU,ID_CMD,"Command Prompt",18,126,234,32);
        button(MENU,ID_TASKMGR,"Task Manager",18,164,234,32);
        button(MENU,ID_SETTINGS,"Windows Settings",18,202,234,32);
        button(MENU,ID_NOTEPAD,"Notepad",18,240,112,32);
        button(MENU,ID_RUN,"Run",140,240,112,32);

        ShowWindow(bar,SW_SHOW);
        let mut msg:MSG=mem::zeroed();
        while GetMessageW(&mut msg,ptr::null_mut(),0,0)>0 {
            TranslateMessage(&msg);DispatchMessageW(&msg);
        }
    }
}

#[cfg(windows)]
fn main(){unsafe{win::run();}}
