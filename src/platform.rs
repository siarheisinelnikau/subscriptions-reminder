use std::{
    collections::VecDeque,
    ffi::c_void,
    path::Path,
    ptr::{null, null_mut},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicIsize, Ordering},
        mpsc,
    },
    thread,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::Cryptography::*,
    System::{LibraryLoader::GetModuleHandleW, Registry::*},
    UI::{Shell::*, WindowsAndMessaging::*},
};

const OPEN: u32 = WM_APP + 1;
const NOTIFY: u32 = WM_APP + 2;
const TRAY: u32 = WM_APP + 3;
const STOP: u32 = WM_APP + 4;
static PLATFORM: OnceLock<Arc<Platform>> = OnceLock::new();

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub struct Notice {
    title: String,
    text: String,
    result: mpsc::Sender<Result<(), String>>,
}
pub struct Platform {
    host: AtomicIsize,
    main: AtomicIsize,
    context: Mutex<Option<egui::Context>>,
    notices: Mutex<VecDeque<Notice>>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
    pub quit: AtomicBool,
}

impl Platform {
    pub fn start(class: String) -> Result<Arc<Self>, String> {
        let value = Arc::new(Self {
            host: AtomicIsize::new(0),
            main: AtomicIsize::new(0),
            context: Mutex::new(None),
            notices: Mutex::new(VecDeque::new()),
            thread: Mutex::new(None),
            quit: AtomicBool::new(false),
        });
        PLATFORM
            .set(value.clone())
            .map_err(|_| "The tray is already initialized")?;
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        let worker = thread::spawn(move || unsafe {
            let name = wide(&class);
            let instance = GetModuleHandleW(null());
            let wc = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: name.as_ptr(),
                ..std::mem::zeroed()
            };
            if RegisterClassW(&wc) == 0 {
                let _ = tx.send(Err("Cannot register the background window".into()));
                return;
            }
            let hwnd = CreateWindowExW(
                0,
                name.as_ptr(),
                name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                instance,
                null(),
            );
            if hwnd.is_null() {
                let _ = tx.send(Err("Cannot create the background window".into()));
                return;
            }
            let platform = PLATFORM.get().unwrap();
            platform.host.store(hwnd as isize, Ordering::SeqCst);
            let mut data = icon_data(hwnd);
            data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            data.hIcon = create_icon();
            data.uCallbackMessage = TRAY;
            fill(&mut data.szTip, "Subscriptions Reminder");
            if data.hIcon.is_null() || Shell_NotifyIconW(NIM_ADD, &data) == 0 {
                DestroyIcon(data.hIcon);
                DestroyWindow(hwnd);
                let _ = tx.send(Err("Windows could not create the tray icon. Reminders cannot run safely in the background.".into()));
                return;
            }
            let _ = tx.send(Ok(()));
            let taskbar = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                if msg.message == taskbar {
                    Shell_NotifyIconW(NIM_ADD, &data);
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            Shell_NotifyIconW(NIM_DELETE, &data);
            DestroyIcon(data.hIcon);
            platform.host.store(0, Ordering::SeqCst);
        });
        *value.thread.lock().unwrap() = Some(worker);
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "The tray did not start".to_string())??;
        Ok(value)
    }
    pub fn bind(&self, hwnd: isize, ctx: egui::Context) {
        self.main.store(hwnd, Ordering::SeqCst);
        *self.context.lock().unwrap() = Some(ctx);
    }
    pub fn request_repaint(&self) {
        if let Some(ctx) = self.context.lock().unwrap().as_ref() {
            ctx.request_repaint();
        }
    }
    pub fn stop(&self) {
        self.main.store(0, Ordering::SeqCst);
        self.quit.store(true, Ordering::SeqCst);
        *self.context.lock().unwrap() = None;
        let host = self.host.load(Ordering::SeqCst) as HWND;
        if !host.is_null() {
            unsafe {
                PostMessageW(host, STOP, 0, 0);
            }
        }
        if let Some(worker) = self.thread.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
    pub fn restore(&self) {
        let hwnd = self.main.load(Ordering::SeqCst) as HWND;
        if hwnd.is_null() {
            return;
        }
        unsafe {
            ShowWindowAsync(
                hwnd,
                if IsIconic(hwnd) != 0 {
                    SW_RESTORE
                } else {
                    SW_SHOW
                },
            );
            SetForegroundWindow(hwnd);
        }
        if let Some(ctx) = self.context.lock().unwrap().as_ref() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
        }
    }
    pub fn request_quit(&self) {
        self.quit.store(true, Ordering::SeqCst);
        self.restore();
        if let Some(ctx) = self.context.lock().unwrap().as_ref() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            ctx.request_repaint();
        }
    }
    pub fn notify(&self, title: String, text: String) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.notices.lock().unwrap().push_back(Notice {
            title,
            text,
            result: tx,
        });
        unsafe {
            PostMessageW(self.host.load(Ordering::SeqCst) as HWND, NOTIFY, 0, 0);
        }
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "Windows notification timed out".to_string())?
    }
}

fn fill<const N: usize>(dest: &mut [u16; N], text: &str) {
    // Do not split a UTF-16 surrogate pair at the end of the Windows buffer.
    let mut offset = 0;
    for ch in text.chars() {
        let mut units = [0; 2];
        let encoded = ch.encode_utf16(&mut units);
        if offset + encoded.len() >= N {
            break;
        }
        dest[offset..offset + encoded.len()].copy_from_slice(encoded);
        offset += encoded.len();
    }
}
unsafe fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..unsafe { std::mem::zeroed() }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let Some(platform) = PLATFORM.get() else {
        return unsafe { DefWindowProcW(hwnd, msg, w, l) };
    };
    match msg {
        OPEN => {
            platform.restore();
            0
        }
        NOTIFY => {
            while let Some(notice) = platform.notices.lock().unwrap().pop_front() {
                let mut data = unsafe { icon_data(hwnd) };
                data.uFlags = NIF_INFO;
                data.dwInfoFlags = NIIF_INFO | NIIF_RESPECT_QUIET_TIME;
                fill(&mut data.szInfoTitle, &notice.title);
                fill(&mut data.szInfo, &notice.text);
                let result = if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } != 0 {
                    Ok(())
                } else {
                    Err("Windows rejected the notification".into())
                };
                let _ = notice.result.send(result);
            }
            0
        }
        TRAY => {
            match l as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK | NIN_BALLOONUSERCLICK => platform.restore(),
                WM_RBUTTONUP => unsafe {
                    let menu = CreatePopupMenu();
                    AppendMenuW(
                        menu,
                        MF_STRING,
                        1,
                        wide("Open Subscriptions Reminder").as_ptr(),
                    );
                    AppendMenuW(menu, MF_SEPARATOR, 0, null());
                    AppendMenuW(menu, MF_STRING, 2, wide("Quit").as_ptr());
                    let mut point = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut point);
                    SetForegroundWindow(hwnd);
                    let cmd = TrackPopupMenu(
                        menu,
                        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                        point.x,
                        point.y,
                        0,
                        hwnd,
                        null(),
                    );
                    DestroyMenu(menu);
                    PostMessageW(hwnd, WM_NULL, 0, 0);
                    if cmd != 0 {
                        PostMessageW(hwnd, WM_COMMAND, cmd as WPARAM, 0);
                    }
                },
                _ => (),
            }
            0
        }
        WM_COMMAND => {
            match (w & 0xffff) as u32 {
                1 => platform.restore(),
                2 => platform.request_quit(),
                _ => (),
            }
            0
        }
        STOP => {
            unsafe {
                DestroyWindow(hwnd);
                PostQuitMessage(0);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

pub fn open_existing(class: &str) -> bool {
    unsafe {
        let hwnd = FindWindowW(wide(class).as_ptr(), null());
        if hwnd.is_null() {
            return false;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        AllowSetForegroundWindow(pid);
        PostMessageW(hwnd, OPEN, 0, 0) != 0
    }
}

unsafe fn create_icon() -> HICON {
    let mut icon = null_mut();
    // SAFETY: Resource 1 is embedded by build.rs; LoadIconMetric returns an owned handle.
    let result = unsafe {
        windows_sys::Win32::UI::Controls::LoadIconMetric(
            GetModuleHandleW(null()),
            std::ptr::without_provenance::<u16>(1),
            windows_sys::Win32::UI::Controls::LIM_SMALL,
            &mut icon,
        )
    };
    if result < 0 { null_mut() } else { icon }
}

pub fn protect(text: &str) -> Result<Vec<u8>, String> {
    if text.is_empty() {
        return Ok(vec![]);
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: text.len() as u32,
        pbData: text.as_ptr() as *mut u8,
    };
    let mut output: CRYPT_INTEGER_BLOB = unsafe { std::mem::zeroed() };
    unsafe {
        if CryptProtectData(
            &input,
            null(),
            null(),
            null(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        ) == 0
        {
            return Err("Windows could not protect the credential".into());
        }
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as *mut c_void);
        Ok(bytes)
    }
}
pub fn unprotect(bytes: &[u8]) -> Result<String, String> {
    if bytes.is_empty() {
        return Ok(String::new());
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output: CRYPT_INTEGER_BLOB = unsafe { std::mem::zeroed() };
    unsafe {
        if CryptUnprotectData(
            &input,
            null_mut(),
            null(),
            null(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        ) == 0
        {
            return Err(
                "The saved credential cannot be decrypted by this Windows account. Enter it again."
                    .into(),
            );
        }
        let result = String::from_utf8(
            std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec(),
        )
        .map_err(|_| "Invalid saved token".into());
        LocalFree(output.pbData as *mut c_void);
        result
    }
}

pub fn autostart_enabled() -> bool {
    unsafe {
        let mut key = null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run").as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        ) != 0
        {
            return false;
        }
        let found = RegQueryValueExW(
            key,
            wide("SubscriptionsReminder").as_ptr(),
            null(),
            null_mut(),
            null_mut(),
            null_mut(),
        ) == 0;
        RegCloseKey(key);
        found
    }
}
pub fn set_autostart(enabled: bool, config_dir: &Path) -> Result<(), String> {
    unsafe {
        let mut key = null_mut();
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run").as_ptr(),
            0,
            null(),
            0,
            KEY_SET_VALUE,
            null(),
            &mut key,
            null_mut(),
        );
        if status != 0 {
            return Err(format!("Cannot open Windows startup settings ({status})"));
        }
        let result = if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let value = wide(&format!(
                "\"{}\" --background --data-dir \"{}\"",
                exe.display(),
                config_dir.display()
            ));
            RegSetValueExW(
                key,
                wide("SubscriptionsReminder").as_ptr(),
                0,
                REG_SZ,
                value.as_ptr() as *const u8,
                (value.len() * 2) as u32,
            )
        } else {
            RegDeleteValueW(key, wide("SubscriptionsReminder").as_ptr())
        };
        RegCloseKey(key);
        if result == 0 || (!enabled && result == ERROR_FILE_NOT_FOUND) {
            Ok(())
        } else {
            Err(format!("Cannot change Windows startup settings ({result})"))
        }
    }
}

pub fn show_error(message: &str) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(message).as_ptr(),
            wide("Subscriptions Reminder").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bot_token_is_protected_and_can_be_read_by_the_same_account() {
        let token = "123456789:sample_token_for_local_test";
        let encrypted = protect(token).unwrap();
        assert!(
            !encrypted
                .windows(token.len())
                .any(|bytes| bytes == token.as_bytes())
        );
        assert_eq!(unprotect(&encrypted).unwrap(), token);
        assert_eq!(unprotect(&protect("").unwrap()).unwrap(), "");
        assert!(unprotect(b"invalid protected data").is_err());
    }
}
