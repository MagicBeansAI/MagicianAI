// swift-tools-version: 5.9

import PackageDescription
import Foundation

// The two host-side speech helpers the desktop tray and the media rails
// spawn. They lived in the retired `macos-presence-host` package beside the
// Orb mascot; the mascot moved into the Tauri frontend, the helpers did not.
// Each embeds its own Info.plist so macOS attributes the Speech / Microphone /
// Screen Recording prompts to the helper, not to whichever shell spawned it.
let packageDirectory = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()
    .path
let speechHelperInfoPlist = "\(packageDirectory)/SpeechHelper.Info.plist"
let meetAudioInfoPlist = "\(packageDirectory)/MeetAudio.Info.plist"

let package = Package(
    name: "MagicianMacSpeechHelper",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(
            name: "magician-macos-speech-helper",
            targets: ["MagicianMacSpeechHelper"]
        ),
        .executable(
            name: "magician-macos-meet-audio",
            targets: ["MagicianMacMeetAudio"]
        )
    ],
    targets: [
        .executableTarget(
            name: "MagicianMacSpeechHelper",
            linkerSettings: [
                .unsafeFlags([
                    "-Xlinker", "-sectcreate",
                    "-Xlinker", "__TEXT",
                    "-Xlinker", "__info_plist",
                    "-Xlinker", speechHelperInfoPlist
                ])
            ]
        ),
        .executableTarget(
            name: "MagicianMacMeetAudio",
            linkerSettings: [
                .unsafeFlags([
                    "-Xlinker", "-sectcreate",
                    "-Xlinker", "__TEXT",
                    "-Xlinker", "__info_plist",
                    "-Xlinker", meetAudioInfoPlist
                ])
            ]
        )
    ]
)
