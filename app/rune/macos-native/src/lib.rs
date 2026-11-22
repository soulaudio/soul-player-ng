//! The few AppKit hooks Rune doesn't expose. Today: Finder's **"open documents"** — a file
//! double-clicked, dropped on the Dock icon or opened with "Open With → Soul Player" arrives as
//! the core Apple event `'aevt'/'odoc'`, which Rune doesn't route (it handles only `myapp://`
//! links, `'GURL'`). Elsewhere [`on_open_documents`] is a no-op: Windows and Linux pass
//! opened files as launch arguments, which the app reads itself.
//!
//! This is the only crate in the app allowed `unsafe`. Every block is an Objective-C message
//! send or class definition with its contract spelled out in a `SAFETY:` comment.
#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]

use std::path::PathBuf;
use std::sync::OnceLock;

type Handler = Box<dyn Fn(Vec<PathBuf>) + Send + Sync>;
static HANDLER: OnceLock<Handler> = OnceLock::new();

/// Call `f` with the file paths each time the OS asks this app to open documents. Install
/// it **on the main thread before the event loop starts** so documents that launched the
/// app are delivered too. Only the first call installs; later calls are ignored. A no-op
/// off macOS.
pub fn on_open_documents(f: impl Fn(Vec<PathBuf>) + Send + Sync + 'static) {
    if HANDLER.set(Box::new(f)).is_ok() {
        #[cfg(target_os = "macos")]
        mac::install();
    }
}

#[cfg(target_os = "macos")]
fn deliver(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    if let Some(h) = HANDLER.get() {
        h(paths);
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::path::PathBuf;
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::NSObject;
    use objc2::{AllocAnyThread, define_class, msg_send, sel};
    use objc2_app_kit::NSApplicationWillFinishLaunchingNotification;
    use objc2_foundation::{
        NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter,
    };

    /// `kCoreEventClass` ('aevt'), `kAEOpenDocuments` ('odoc'), `keyDirectObject` ('----').
    const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
    const OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
    const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

    define_class!(
        // SAFETY: NSObject has no subclassing requirements; no `Drop` impl, no ivars.
        #[unsafe(super(NSObject))]
        #[name = "SoulOpenDocumentsHandler"]
        struct OpenDocs;

        impl OpenDocs {
            // SAFETY: the signature matches the selector NSAppleEventManager calls — two
            // `NSAppleEventDescriptor` objects (event, reply), returning void.
            #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
            fn handle(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
                super::deliver(paths(event));
            }
        }
    );

    impl OpenDocs {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(());
            // SAFETY: plain `-[NSObject init]` on a freshly allocated instance.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// File paths in an `odoc` event: its direct object is a list of file references (or a
    /// single one); each is coerced to a file URL.
    fn paths(event: &NSAppleEventDescriptor) -> Vec<PathBuf> {
        // SAFETY: `paramDescriptorForKeyword:` takes an `AEKeyword` (a `u32` four-char code)
        // and returns a nullable, autoreleased descriptor — `Option<Retained<_>>`.
        let list: Option<Retained<NSAppleEventDescriptor>> =
            unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT] };
        let Some(list) = list else {
            return Vec::new();
        };
        let path_of = |d: &NSAppleEventDescriptor| {
            d.fileURLValue()
                .and_then(|u| u.path())
                .map(|p| PathBuf::from(p.to_string()))
        };
        let n = list.numberOfItems();
        if n <= 0 {
            return path_of(&list).into_iter().collect();
        }
        (1..=n) // Apple-event lists are 1-based
            .filter_map(|i| list.descriptorAtIndex(i))
            .filter_map(|d| path_of(&d))
            .collect()
    }

    /// Point `'aevt'/'odoc'` at our handler object (kept alive for the process lifetime).
    fn set_handler() {
        let handler = OpenDocs::new();
        let manager = NSAppleEventManager::sharedAppleEventManager();
        // SAFETY: `handler` responds to the selector with the signature AppKit expects
        // (see `OpenDocs::handle`); the event class / id are `u32` four-char codes.
        unsafe {
            let _: () = msg_send![
                &*manager,
                setEventHandler: &*handler,
                andSelector: sel!(handleOpenDocuments:withReplyEvent:),
                forEventClass: CORE_EVENT_CLASS,
                andEventID: OPEN_DOCUMENTS
            ];
        }
        // NSAppleEventManager doesn't retain handlers — keep ours alive forever.
        std::mem::forget(handler);
    }

    pub(super) fn install() {
        // Now (covers an install after launch) …
        set_handler();
        // … and again when the app is about to finish launching: AppKit installs its own
        // core-event handlers before that notification, and Apple documents it as the place
        // where custom handlers override them. Launch-time documents are dispatched after.
        let block = RcBlock::new(|_n: NonNull<NSNotification>| set_handler());
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: the notification name is an AppKit-provided static `NSString`; a nil object
        // and nil queue (post on the posting thread — the main thread) are documented valid.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSApplicationWillFinishLaunchingNotification),
                None,
                None,
                &block,
            )
        };
        // The observer lives for the whole process.
        std::mem::forget(token);
    }
}
