//! How each output device is connected (built-in, USB, HDMI, Bluetooth, AirPlay …), so the
//! output picker can say up front which devices can never be bit-perfect — AirPlay
//! re-encodes to ALAC at 44.1 kHz on the receiver's clock, Bluetooth is a lossy codec —
//! instead of the user finding out from an exclusive-mode fall-back after pressing play.
//!
//! macOS reads `system_profiler SPAudioDataType -json` (≈0.2 s; run it off the UI thread):
//! a public, stable report that needs no CoreAudio FFI, so this stays `forbid(unsafe_code)`.
//! Other platforms return an empty map (the picker then shows names only).
#![forbid(unsafe_code)]

use std::collections::HashMap;

/// An output device's connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    BuiltIn,
    Usb,
    Hdmi,
    DisplayPort,
    Thunderbolt,
    Bluetooth,
    AirPlay,
    Virtual,
    Aggregate,
    Other(String),
}

impl Transport {
    /// Whether bit-perfect output to this device is possible at all. AirPlay and Bluetooth
    /// always transcode, whatever the player does.
    pub fn can_be_bit_perfect(&self) -> bool {
        !matches!(self, Transport::AirPlay | Transport::Bluetooth)
    }

    fn from_profiler(tag: &str) -> Self {
        match tag.strip_prefix("coreaudio_device_type_").unwrap_or(tag) {
            "builtin" => Transport::BuiltIn,
            "usb" => Transport::Usb,
            "hdmi" => Transport::Hdmi,
            "displayport" => Transport::DisplayPort,
            "thunderbolt" => Transport::Thunderbolt,
            "bluetooth" | "bluetooth_le" => Transport::Bluetooth,
            "airplay" => Transport::AirPlay,
            "virtual" => Transport::Virtual,
            "aggregate" => Transport::Aggregate,
            other => Transport::Other(other.to_string()),
        }
    }
}

/// Output device name → transport, parsed from `system_profiler SPAudioDataType -json`.
/// Input-only devices (no `coreaudio_device_output`) are skipped. Malformed input yields an
/// empty map — the picker just shows plain names.
pub fn parse_system_profiler(json: &str) -> HashMap<String, Transport> {
    let mut out = HashMap::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return out;
    };
    let groups = v
        .get("SPAudioDataType")
        .and_then(|g| g.as_array())
        .cloned()
        .unwrap_or_default();
    for g in groups {
        for item in g.get("_items").and_then(|i| i.as_array()).into_iter().flatten() {
            let (Some(name), Some(tag)) = (
                item.get("_name").and_then(|n| n.as_str()),
                item.get("coreaudio_device_transport").and_then(|t| t.as_str()),
            ) else {
                continue;
            };
            if item.get("coreaudio_device_output").is_none() {
                continue;
            }
            out.insert(name.to_string(), Transport::from_profiler(tag));
        }
    }
    out
}

/// Query the current output devices' transports. Blocking (spawns `system_profiler`) —
/// call it from a background thread.
pub fn query() -> HashMap<String, Transport> {
    #[cfg(target_os = "macos")]
    {
        match std::process::Command::new("/usr/sbin/system_profiler")
            .args(["SPAudioDataType", "-json"])
            .output()
        {
            Ok(o) if o.status.success() => {
                parse_system_profiler(&String::from_utf8_lossy(&o.stdout))
            }
            _ => HashMap::new(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        HashMap::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real MacBook Pro report, plus AirPlay / Bluetooth / USB entries in the
    /// same shape `system_profiler` emits for them.
    const REPORT: &str = r#"{"SPAudioDataType":[{"_name":"coreaudio_device","_items":[
      {"_name":"VX2718-2KPC","coreaudio_device_output":2,"coreaudio_device_transport":"coreaudio_device_type_displayport"},
      {"_name":"MacBook Pro Microphone","coreaudio_device_input":1,"coreaudio_device_transport":"coreaudio_device_type_builtin"},
      {"_name":"MacBook Pro Speakers","coreaudio_device_output":2,"coreaudio_device_transport":"coreaudio_device_type_builtin"},
      {"_name":"Living Room","coreaudio_device_output":2,"coreaudio_device_transport":"coreaudio_device_type_airplay"},
      {"_name":"AirPods Pro","coreaudio_device_output":2,"coreaudio_device_transport":"coreaudio_device_type_bluetooth"},
      {"_name":"Topping E50","coreaudio_device_output":2,"coreaudio_device_transport":"coreaudio_device_type_usb"},
      {"_name":"Odd Box","coreaudio_device_output":2,"coreaudio_device_transport":"coreaudio_device_type_firewire"}
    ]}]}"#;

    #[test]
    fn parses_output_devices_and_skips_input_only_ones() {
        let m = parse_system_profiler(REPORT);
        assert_eq!(m.get("VX2718-2KPC"), Some(&Transport::DisplayPort));
        assert_eq!(m.get("MacBook Pro Speakers"), Some(&Transport::BuiltIn));
        assert_eq!(m.get("Living Room"), Some(&Transport::AirPlay));
        assert_eq!(m.get("AirPods Pro"), Some(&Transport::Bluetooth));
        assert_eq!(m.get("Topping E50"), Some(&Transport::Usb));
        assert_eq!(m.get("Odd Box"), Some(&Transport::Other("firewire".into())));
        assert!(!m.contains_key("MacBook Pro Microphone"), "input-only");
    }

    #[test]
    fn only_airplay_and_bluetooth_can_never_be_bit_perfect() {
        assert!(!Transport::AirPlay.can_be_bit_perfect());
        assert!(!Transport::Bluetooth.can_be_bit_perfect());
        for t in [Transport::BuiltIn, Transport::Usb, Transport::Hdmi, Transport::DisplayPort] {
            assert!(t.can_be_bit_perfect(), "{t:?}");
        }
    }

    #[test]
    fn malformed_reports_give_an_empty_map() {
        assert!(parse_system_profiler("not json").is_empty());
        assert!(parse_system_profiler("{}").is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_real_report_lists_at_least_one_output() {
        // Every Mac (and CI runner) has some output device; this proves the field names
        // still match what this macOS version emits.
        assert!(!query().is_empty(), "system_profiler reported no output transports");
    }
}
