//! Files the system hands wave9000: named on the command line (Windows,
//! Linux, or a sister 9000 app starting it), or opened from Finder, the
//! Dock or `open -a` (macOS, as Apple Events, including the one that
//! launches it). They're opened like files dropped on the window.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static WAKE: OnceLock<egui::Context> = OnceLock::new();

/// Queues a file and wakes the window to open it.
fn deliver(path: PathBuf) {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).push(path);
    if let Some(ctx) = WAKE.get() {
        ctx.request_repaint();
    }
}

/// What's arrived since the last call.
pub fn take() -> Vec<PathBuf> {
    std::mem::take(&mut *PENDING.lock().unwrap_or_else(|e| e.into_inner()))
}

/// The files named on the command line (after the program), leaving out
/// options such as `--version` and the `-psn_…` old macOS adds.
pub fn files_in(args: impl IntoIterator<Item = OsString>) -> Vec<PathBuf> {
    args.into_iter().skip(1).filter(|a| !a.to_string_lossy().starts_with('-')).map(PathBuf::from).collect()
}

/// Takes the command line's files and starts listening, before the event
/// loop runs, so the event that launched the app isn't missed.
pub fn listen_early() {
    for path in files_in(std::env::args_os()) {
        deliver(path);
    }
    #[cfg(target_os = "macos")]
    {
        mac::listen();
        mac::add_open_urls();
    }
}

/// Once the window is up: files that arrive from now on wake it, and the
/// handler goes in again in case starting the app replaced it.
pub fn started(ctx: &egui::Context) {
    let _ = WAKE.set(ctx.clone());
    #[cfg(target_os = "macos")]
    mac::listen();
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{AnyThread, class, define_class, msg_send, sel};
    use objc2_foundation::NSString;

    use super::deliver;

    /// Apple Event codes (four-character codes).
    const CORE: u32 = u32::from_be_bytes(*b"aevt");
    const OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
    const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and this class
        // doesn't implement Drop.
        #[unsafe(super(NSObject))]
        #[name = "Wave9000OpenHandler"]
        struct Handler;

        impl Handler {
            #[unsafe(method(handleOpen:withReply:))]
            fn handle_open(&self, event: &AnyObject, _reply: &AnyObject) {
                // SAFETY: Apple Event descriptors respond to these selectors;
                // the direct object is a list of file URLs.
                unsafe {
                    let list: Option<Retained<AnyObject>> = msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT];
                    let Some(list) = list else { return };
                    let count: isize = msg_send![&list, numberOfItems];
                    for i in 1..=count {
                        let item: Option<Retained<AnyObject>> = msg_send![&list, descriptorAtIndex: i];
                        let url: Option<Retained<AnyObject>> = item.and_then(|d| msg_send![&d, fileURLValue]);
                        let path: Option<Retained<NSString>> = url.and_then(|u| msg_send![&u, path]);
                        if let Some(path) = path {
                            deliver(path.to_string().into());
                        }
                    }
                }
            }
        }
    );

    /// `application:openURLs:`, which Cocoa calls on the app delegate for
    /// files opened from Finder, including the one that launches the app.
    /// winit's delegate has no such method, so it's added to NSObject (which
    /// every delegate inherits from) before the app starts.
    unsafe extern "C-unwind" fn open_urls(_this: &AnyObject, _cmd: objc2::runtime::Sel, _app: &AnyObject, urls: &AnyObject) {
        // SAFETY: `urls` is an NSArray of NSURL.
        unsafe {
            let count: usize = msg_send![urls, count];
            for i in 0..count {
                let url: Retained<AnyObject> = msg_send![urls, objectAtIndex: i];
                let path: Option<Retained<NSString>> = msg_send![&url, path];
                if let Some(path) = path {
                    deliver(path.to_string().into());
                }
            }
        }
    }

    pub fn add_open_urls() {
        let sel = sel!(application:openURLs:);
        let class = objc2::runtime::AnyClass::get(c"NSObject").expect("NSObject exists");
        // SAFETY: the function matches the method's signature (v@:@@), and
        // NSObject doesn't have this method already (checked).
        unsafe {
            if objc2::ffi::class_respondsToSelector(class, sel).as_bool() {
                return;
            }
            let imp: objc2::runtime::Imp = std::mem::transmute(
                open_urls as unsafe extern "C-unwind" fn(&AnyObject, objc2::runtime::Sel, &AnyObject, &AnyObject),
            );
            objc2::ffi::class_addMethod(class as *const _ as *mut _, sel, imp, c"v@:@@".as_ptr());
        }
    }

    pub fn listen() {
        // SAFETY: plain init of an NSObject subclass; the handler is kept
        // for the life of the app (the event manager doesn't retain it).
        unsafe {
            let handler: Retained<Handler> = msg_send![Handler::alloc(), init];
            let manager: Retained<AnyObject> = msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
            let _: () = msg_send![&manager, setEventHandler: &*handler, andSelector: sel!(handleOpen:withReply:), forEventClass: CORE, andEventID: OPEN_DOCUMENTS];
            std::mem::forget(handler);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_on_the_command_line() {
        let args = |a: &[&str]| files_in(a.iter().map(OsString::from));
        assert_eq!(args(&["wave9000"]), Vec::<PathBuf>::new());
        assert_eq!(args(&["wave9000", "/a/b c.wav"]), [PathBuf::from("/a/b c.wav")]);
        assert_eq!(args(&["wave9000", "-psn_0_1234", "x.wav", "y.wav"]), [PathBuf::from("x.wav"), PathBuf::from("y.wav")]);
        assert_eq!(args(&["wave9000", "--version"]), Vec::<PathBuf>::new());
    }

    #[test]
    fn handed_files_are_taken_once() {
        deliver(PathBuf::from("one.wav"));
        assert!(take().contains(&PathBuf::from("one.wav")));
        assert!(!take().contains(&PathBuf::from("one.wav")));
    }
}
