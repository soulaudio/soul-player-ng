//! Native OS toast notifications: an "update available" toast, and a
//! "now playing" toast when a new track starts while the app is backgrounded
//! (foreground users already see the now-playing UI, so those are suppressed).
//!
//! On Windows we build a `ToastGeneric` payload and show it through the WinRT
//! `ToastNotificationManager` on the `windows` crate souvlaki already compiled
//! for SMTC, no extra notification crate, no second `windows` version, no
//! `unsafe`. Windows only renders toasts whose AppUserModelID it knows, so we
//! register one under `HKCU` at startup ([`register_identity`]).
//!
//! On macOS we post through `rune-notify` (`UNUserNotificationCenter`), which reads the
//! app identity from the `.app` bundle (`co.soulaudio.soulplayer`) and asks the person for
//! permission on first post. An unbundled dev binary can't post (`available()` is false),
//! so it stays silent. Each kind reuses one notification id, so a new now-playing banner
//! replaces the previous one instead of piling up in Notification Center.
//! Linux is a no-op (now-playing is covered by the MPRIS media session).
#![forbid(unsafe_code)]

/// Our stable reverse-DNS AppUserModelID (matches the packager identifier).
/// Only referenced by the Windows toast + registry paths.
#[cfg(windows)]
pub(crate) const AUMID: &str = "co.soulaudio.soulplayer";

/// Escape text for inclusion in XML character data. `&` first so we don't
/// double-escape the entities we introduce. A track titled "Rock & Roll" or
/// "<3" would otherwise produce malformed toast XML that silently fails to show.
#[cfg(any(windows, test))]
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Build a two-line `ToastGeneric` payload (bold title + body line).
#[cfg(any(windows, test))]
fn toast_xml(title: &str, body: &str) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\">\
         <text>{}</text><text>{}</text>\
         </binding></visual></toast>",
        escape_xml(title),
        escape_xml(body),
    )
}

/// Whether to raise a now-playing toast: only when the app is backgrounded
/// (foreground users see the UI) and we actually have a track title.
fn should_notify(focused: bool, title: &str) -> bool {
    !focused && !title.trim().is_empty()
}

/// Register our AppUserModelID under `HKCU` so Windows attributes and renders
/// toasts for this unpackaged app. Call once at startup. No-op off Windows.
#[cfg(windows)]
pub(crate) fn register_identity() {
    use windows_registry::CURRENT_USER;
    let res: windows_registry::Result<()> = (|| {
        let key = CURRENT_USER.create(format!(r"Software\Classes\AppUserModelId\{AUMID}"))?;
        key.set_string("DisplayName", "Soul Player")?;
        key.set_u32("ShowInSettings", 1)?;
        Ok(())
    })();
    if let Err(e) = res {
        tracing::warn!("AUMID registration failed: {e}");
    }
}

#[cfg(not(windows))]
pub(crate) fn register_identity() {}

/// Toast that a newer signed release is available.
pub(crate) fn update_available(version: &str) {
    if cfg!(test) {
        return;
    }
    let title = t!("notify.updateTitle").to_string();
    let body = t!("notify.updateBody", version => version).to_string();
    show(ID_UPDATE, &title, &body);
}

/// Toast the now-playing track, only when backgrounded and we have a title.
pub(crate) fn now_playing(focused: bool, title: &str, artist: &str) {
    if cfg!(test) || !should_notify(focused, title) {
        return;
    }
    show(ID_NOW_PLAYING, title, artist);
}

/// Stable per-kind notification ids (a repost replaces the previous one).
const ID_UPDATE: &str = "update-available";
const ID_NOW_PLAYING: &str = "now-playing";

/// Show a notification. Failures are logged, never propagated, a missing notifier must
/// not disturb playback.
#[cfg(windows)]
fn show(_id: &str, title: &str, body: &str) {
    show_xml(&toast_xml(title, body));
}

#[cfg(target_os = "macos")]
fn show(id: &str, title: &str, body: &str) {
    if !rune_notify::available() {
        tracing::debug!("notifications need the bundled .app; skipped {id}");
        return;
    }
    let n = rune_notify::Notification::new(id, title).body(body);
    if !rune_notify::post(&n) {
        tracing::warn!("notification {id} was not posted");
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn show(_id: &str, _title: &str, _body: &str) {}

/// Display a prepared WinRT toast payload.
#[cfg(windows)]
fn show_xml(xml: &str) {
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::core::HSTRING;

    let res = (|| -> windows::core::Result<()> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?;
        notifier.Show(&toast)
    })();
    if let Err(e) = res {
        tracing::warn!("toast failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_xml_escapes_xml_special_chars() {
        let xml = toast_xml("Rock & Roll", "A <B> says \"hi\"");
        assert!(xml.contains("Rock &amp; Roll"), "ampersand escaped: {xml}");
        assert!(xml.contains("A &lt;B&gt;"), "angle brackets escaped: {xml}");
        // No raw `&`/`<` may survive into the payload.
        assert!(!xml.contains("Rock & Roll"));
        assert!(!xml.contains("<B>"));
    }

    #[test]
    fn toast_xml_wraps_both_texts_in_a_generic_binding() {
        let xml = toast_xml("Title", "Body");
        assert!(xml.contains("template=\"ToastGeneric\""));
        assert!(xml.contains("<text>Title</text>"));
        assert!(xml.contains("<text>Body</text>"));
    }

    #[test]
    fn now_playing_is_suppressed_when_focused_or_empty() {
        assert!(!should_notify(true, "Song"), "focused → no toast");
        assert!(!should_notify(false, ""), "empty title → no toast");
        assert!(!should_notify(false, "   "), "blank title → no toast");
        assert!(should_notify(false, "Song"), "backgrounded + title → toast");
    }
}
