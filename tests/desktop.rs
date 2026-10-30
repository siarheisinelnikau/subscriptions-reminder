//! Opt-in regression test against the real Windows GUI and tray callback.
#![cfg(windows)]
use std::{
    os::windows::io::AsRawHandle,
    path::Path,
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*, System::Threading::GetProcessTimes, UI::WindowsAndMessaging::*,
};

struct Running {
    child: Child,
    data: tempfile::TempDir,
}
impl Drop for Running {
    fn drop(&mut self) {
        unsafe {
            PostMessageW(windows(self.child.id()).tray, WM_COMMAND, 2, 0);
        }
        for _ in 0..30 {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn wait(label: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(30));
    }
    panic!("Timed out: {label}");
}
struct Search {
    pid: u32,
    main: HWND,
    tray: HWND,
}
unsafe extern "system" fn find(window: HWND, data: LPARAM) -> i32 {
    unsafe {
        let search = &mut *(data as *mut Search);
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        if pid == search.pid {
            let mut class = [0u16; 256];
            let len = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32);
            let class = String::from_utf16_lossy(&class[..len as usize]);
            if class.starts_with("SubscriptionsReminder-") {
                search.tray = window;
            } else {
                let mut title = [0u16; 256];
                let len = GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32);
                if String::from_utf16_lossy(&title[..len as usize]) == "Subscriptions Reminder" {
                    search.main = window;
                }
            }
        }
        1
    }
}
fn windows(pid: u32) -> Search {
    let mut search = Search {
        pid,
        main: std::ptr::null_mut(),
        tray: std::ptr::null_mut(),
    };
    unsafe {
        EnumWindows(Some(find), &mut search as *mut Search as LPARAM);
    }
    search
}
fn executable() -> std::path::PathBuf {
    std::env::var_os("SUBSCRIPTIONS_GUI_EXE")
        .map(Into::into)
        .unwrap_or_else(|| Path::new(env!("CARGO_BIN_EXE_subscriptions-reminder")).into())
}

fn cpu_seconds(child: &Child) -> f64 {
    unsafe {
        let mut created: FILETIME = std::mem::zeroed();
        let mut exited: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        assert_ne!(
            GetProcessTimes(
                child.as_raw_handle(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            ),
            0
        );
        let ticks =
            |time: FILETIME| ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64;
        (ticks(kernel) + ticks(user)) as f64 / 10_000_000.0
    }
}

fn assert_idle_cpu(running: &Running, state: &str) {
    thread::sleep(Duration::from_secs(2));
    let before = cpu_seconds(&running.child);
    let started = Instant::now();
    thread::sleep(Duration::from_secs(4));
    let percent = (cpu_seconds(&running.child) - before) / started.elapsed().as_secs_f64() * 100.0;
    eprintln!("{state}: {percent:.2}% of one CPU core");
    assert!(
        percent < 25.0,
        "Idle {state} window is busy polling: {percent:.2}% of one core"
    );
}

#[test]
#[ignore = "Requires an interactive Windows desktop; measures the packaged app's idle CPU"]
fn idle_window_does_not_busy_poll_cpu() {
    let data = tempfile::tempdir().unwrap();
    let child = Command::new(executable())
        .arg("--data-dir")
        .arg(data.path())
        .arg("--demo")
        .spawn()
        .unwrap();
    let running = Running { child, data };
    let pid = running.child.id();
    wait("visible window and tray", || {
        let w = windows(pid);
        !w.main.is_null() && !w.tray.is_null() && unsafe { IsWindowVisible(w.main) != 0 }
    });
    // Include the old 15-second repaint period in the visible-idle observation.
    thread::sleep(Duration::from_secs(16));
    assert_idle_cpu(&running, "visible");
    let w = windows(pid);
    unsafe {
        assert_ne!(PostMessageW(w.main, WM_CLOSE, 0, 0), 0);
        wait("hidden window", || IsWindowVisible(w.main) == 0);
    }
    assert_idle_cpu(&running, "hidden");
    unsafe {
        assert_ne!(PostMessageW(w.tray, WM_COMMAND, 1, 0), 0);
    }
    wait("restored window", || unsafe {
        IsWindowVisible(w.main) != 0
    });
    assert_idle_cpu(&running, "restored");
    // Exercise a populated Overview while pointer events continue to arrive.
    let before = cpu_seconds(&running.child);
    let started = Instant::now();
    for step in 0..120 {
        let x = 250 + step % 500;
        let y = 200 + step % 300;
        unsafe {
            assert_ne!(PostMessageW(w.main, WM_MOUSEMOVE, 0, x | (y << 16)), 0);
        }
        thread::sleep(Duration::from_millis(16));
    }
    let percent = (cpu_seconds(&running.child) - before) / started.elapsed().as_secs_f64() * 100.0;
    eprintln!("pointer movement: {percent:.2}% of one CPU core");
    assert!(
        percent < 50.0,
        "Pointer movement saturates the UI thread: {percent:.2}%"
    );
}

#[test]
#[ignore = "Requires an interactive Windows desktop; optionally set SUBSCRIPTIONS_GUI_EXE to the packaged EXE"]
fn hidden_window_reopens_through_tray_and_duplicate_launch() {
    let data = tempfile::tempdir().unwrap();
    let exe = executable();
    let child = Command::new(&exe)
        .arg("--data-dir")
        .arg(data.path())
        .arg("--background")
        .arg("--demo")
        .spawn()
        .unwrap();
    let mut running = Running { child, data };
    let pid = running.child.id();
    wait("main and tray HWNDs", || {
        let w = windows(pid);
        !w.main.is_null() && !w.tray.is_null()
    });
    let w = windows(pid);
    unsafe {
        wait("background window hidden", || IsWindowVisible(w.main) == 0);
        thread::sleep(Duration::from_millis(300));
        assert_eq!(
            IsWindowVisible(w.main),
            0,
            "Background startup must remain hidden"
        );
        for event in [WM_LBUTTONUP, WM_LBUTTONDBLCLK, WM_LBUTTONUP] {
            assert_ne!(PostMessageW(w.tray, WM_APP + 3, 1, event as LPARAM), 0);
            wait("tray restore", || {
                IsWindowVisible(w.main) != 0 && IsIconic(w.main) == 0
            });
            assert!(running.child.try_wait().unwrap().is_none());
            assert_eq!(windows(pid).main, w.main);
            assert_ne!(PostMessageW(w.main, WM_CLOSE, 0, 0), 0);
            wait("close hides to tray", || IsWindowVisible(w.main) == 0);
        }
        // A normal second launch must show the existing hidden HWND, not a new process.
        assert!(
            Command::new(&exe)
                .arg("--data-dir")
                .arg(running.data.path())
                .status()
                .unwrap()
                .success()
        );
        wait("duplicate launch restore", || IsWindowVisible(w.main) != 0);
        assert_eq!(windows(pid).main, w.main);
        ShowWindowAsync(w.main, SW_MINIMIZE);
        wait("minimized", || IsIconic(w.main) != 0);
        PostMessageW(w.tray, WM_APP + 3, 1, WM_LBUTTONUP as LPARAM);
        wait("restore minimized window", || {
            IsWindowVisible(w.main) != 0 && IsIconic(w.main) == 0
        });
        PostMessageW(w.main, WM_CLOSE, 0, 0);
        wait("hidden before quit", || IsWindowVisible(w.main) == 0);
    }
    unsafe {
        assert_ne!(PostMessageW(w.tray, WM_COMMAND, 2, 0), 0);
    }
    wait("quit while hidden", || {
        running.child.try_wait().unwrap().is_some()
    });
    assert!(running.child.wait().unwrap().success());
}

#[test]
#[ignore = "Requires an interactive Windows desktop and the packaged release EXE"]
fn font_uploads_and_screenshots_survive_background_startup() {
    let data = tempfile::tempdir().unwrap();
    let capture_temp = tempfile::tempdir().unwrap();
    let capture = std::env::var_os("SUBSCRIPTIONS_TEST_CAPTURE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| capture_temp.path().join("frames"));
    std::fs::create_dir_all(&capture).unwrap();
    let child = Command::new(executable())
        .arg("--data-dir")
        .arg(data.path())
        .arg("--demo")
        .arg("--background")
        .arg("--capture-dir")
        .arg(&capture)
        .spawn()
        .unwrap();
    let mut running = Running { child, data };
    wait("background main and tray windows", || {
        let state = windows(running.child.id());
        !state.main.is_null() && !state.tray.is_null()
    });
    thread::sleep(Duration::from_millis(800));
    let state = windows(running.child.id());
    unsafe {
        assert_eq!(IsWindowVisible(state.main), 0);
        assert_ne!(
            PostMessageW(state.tray, WM_APP + 3, 1, WM_LBUTTONUP as LPARAM),
            0
        );
    }
    wait("restored renderer captures all pages and exits", || {
        running.child.try_wait().unwrap().is_some()
    });
    assert!(running.child.wait().unwrap().success());
    for name in [
        "subscriptions.png",
        "settings.png",
        "storage-updates.png",
        "google-drive.png",
        "image-import.png",
    ] {
        let path = capture.join(name);
        assert!(path.metadata().unwrap().len() > 1000, "No rendered {name}");
        let image = image::open(path).unwrap();
        assert!(image.width() >= 860 && image.height() >= 620);
    }
}
