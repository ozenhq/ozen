//! Ozen.app's Info.plist (`ozen app`): what macOS shows when it asks for each permission.

/// The Bonjour service type Macs advertise and browse for same-network sync. macOS shows the Local Network
/// prompt, and lets the app use the network at all, only for service types listed in Info.plist.
pub const BONJOUR: &str = "_ozen-sync._tcp";

/// Info.plist for Ozen.app at this `version` (a git short hash, or "dev").
pub fn info_plist(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Ozen</string>
  <key>CFBundleDisplayName</key><string>Ozen</string>
  <key>CFBundleIdentifier</key><string>com.tupe12334.ozen</string>
  <key>CFBundleExecutable</key><string>Ozen</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>LSUIElement</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Ozen transcribes what you say in meetings, on this Mac only.</string>
  <key>NSAudioCaptureUsageDescription</key><string>Ozen transcribes the meeting audio, on this Mac only.</string>
  <key>NSLocationUsageDescription</key><string>Ozen starts or stops recording when you arrive at places you set, like Home or Work.</string>
  <key>NSLocalNetworkUsageDescription</key><string>Ozen syncs your meetings directly with your other Macs on this network; nothing goes through a server.</string>
  <key>NSBonjourServices</key><array><string>{BONJOUR}</string></array>
</dict></plist>
"#
    )
}

#[cfg(test)]
#[path = "app_plist_tests.rs"]
mod tests;
