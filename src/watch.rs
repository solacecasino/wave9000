//! Notices changes in the config folder, so an edited keys.toml or
//! settings.toml or layout.toml applies straight away. A thread sleeps in the system's own
//! file-change call (kqueue on macOS, inotify on Linux, change
//! notifications on Windows), so watching costs nothing while nothing
//! changes.

use std::path::Path;

/// Calls `changed` (from another thread) after anything in `dir` changes.
pub fn config_folder(dir: &Path, changed: impl Fn() + Send + 'static) {
    let dir = dir.to_path_buf();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::thread::Builder::new().name("config watcher".into()).spawn(move || imp::watch(&dir, changed));
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::CString;
    use std::path::Path;
    use std::time::Duration;

    pub fn watch(dir: &Path, changed: impl Fn()) {
        loop {
            // The folder (editors that save by replacing a file) and the two
            // files (editors that write in place). Opened afresh each time,
            // since a replaced file is a new file.
            let paths = [dir.to_path_buf(), dir.join("keys.toml"), dir.join("settings.toml"), dir.join("layout.toml")];
            // SAFETY: plain kqueue calls on descriptors opened and closed here.
            unsafe {
                let kq = libc::kqueue();
                if kq < 0 {
                    return;
                }
                let fds: Vec<i32> = paths
                    .iter()
                    .filter_map(|p| CString::new(p.as_os_str().as_encoded_bytes()).ok())
                    .map(|c| libc::open(c.as_ptr(), libc::O_EVTONLY))
                    .filter(|&fd| fd >= 0)
                    .collect();
                let changes: Vec<libc::kevent> = fds
                    .iter()
                    .map(|&fd| libc::kevent {
                        ident: fd as usize,
                        filter: libc::EVFILT_VNODE,
                        flags: libc::EV_ADD | libc::EV_CLEAR,
                        fflags: libc::NOTE_WRITE | libc::NOTE_EXTEND | libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_ATTRIB,
                        data: 0,
                        udata: std::ptr::null_mut(),
                    })
                    .collect();
                let mut out: [libc::kevent; 5] = std::mem::zeroed();
                let n = libc::kevent(kq, changes.as_ptr(), changes.len() as i32, out.as_mut_ptr(), 5, std::ptr::null());
                for fd in fds {
                    libc::close(fd);
                }
                libc::close(kq);
                if n < 0 {
                    return;
                }
            }
            // Let the editor finish saving.
            std::thread::sleep(Duration::from_millis(100));
            changed();
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::ffi::CString;
    use std::path::Path;
    use std::time::Duration;

    pub fn watch(dir: &Path, changed: impl Fn()) {
        let Ok(c) = CString::new(dir.as_os_str().as_encoded_bytes()) else { return };
        // SAFETY: an inotify descriptor read into a plain buffer.
        unsafe {
            let fd = libc::inotify_init1(libc::IN_CLOEXEC);
            if fd < 0 {
                return;
            }
            let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE | libc::IN_DELETE;
            if libc::inotify_add_watch(fd, c.as_ptr(), mask) < 0 {
                libc::close(fd);
                return;
            }
            let mut buf = [0u8; 4096];
            while libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) > 0 {
                std::thread::sleep(Duration::from_millis(100));
                changed();
            }
            libc::close(fd);
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::time::Duration;

    type Handle = *mut std::ffi::c_void;
    const FILE_NOTIFY_CHANGE_FILE_NAME: u32 = 0x1;
    const FILE_NOTIFY_CHANGE_LAST_WRITE: u32 = 0x10;
    const INFINITE: u32 = 0xFFFF_FFFF;
    const WAIT_OBJECT_0: u32 = 0;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn FindFirstChangeNotificationW(path: *const u16, subtree: i32, filter: u32) -> Handle;
        fn FindNextChangeNotification(handle: Handle) -> i32;
        fn FindCloseChangeNotification(handle: Handle) -> i32;
        fn WaitForSingleObject(handle: Handle, ms: u32) -> u32;
    }

    pub fn watch(dir: &Path, changed: impl Fn()) {
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: a change-notification handle opened, waited on and closed
        // here; `wide` is a NUL-terminated path that outlives the call.
        unsafe {
            let h = FindFirstChangeNotificationW(wide.as_ptr(), 0, FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE);
            // INVALID_HANDLE_VALUE is -1.
            if h.is_null() || h as isize == -1 {
                return;
            }
            while WaitForSingleObject(h, INFINITE) == WAIT_OBJECT_0 {
                std::thread::sleep(Duration::from_millis(100));
                changed();
                if FindNextChangeNotification(h) == 0 {
                    break;
                }
            }
            FindCloseChangeNotification(h);
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod imp {
    pub fn watch(_: &std::path::Path, _: impl Fn()) {}
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux", windows)))]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn notices_a_saved_file() {
        let dir = std::env::temp_dir().join(format!("wave9000-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.toml"), "").unwrap();
        let (tx, rx) = mpsc::channel();
        super::config_folder(&dir, move || {
            let _ = tx.send(());
        });
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(dir.join("settings.toml"), "progress = false").unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok(), "no change noticed");
    }
}
