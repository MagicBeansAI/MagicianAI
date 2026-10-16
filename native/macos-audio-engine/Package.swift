// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "MagicianMacAudioEngine",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "MagicianMacAudioEngineCore", targets: ["MagicianMacAudioEngineCore"]),
        .executable(name: "magician-macos-audio-engine", targets: ["MagicianMacAudioEngine"]),
    ],
    dependencies: [
        .package(url: "https://github.com/FluidInference/FluidAudio.git", exact: "0.12.4"),
        .package(url: "https://github.com/hummingbird-project/hummingbird.git", exact: "2.24.0"),
        .package(url: "https://github.com/hummingbird-project/hummingbird-websocket.git", exact: "2.7.0"),
    ],
    targets: [
        .target(name: "MagicianMacAudioEngineCore"),
        .executableTarget(
            name: "MagicianMacAudioEngine",
            dependencies: [
                "MagicianMacAudioEngineCore",
                .product(name: "FluidAudio", package: "FluidAudio"),
                .product(name: "Hummingbird", package: "hummingbird"),
                .product(name: "HummingbirdWebSocket", package: "hummingbird-websocket"),
            ]
        ),
        .testTarget(
            name: "MagicianMacAudioEngineCoreTests",
            dependencies: ["MagicianMacAudioEngineCore"]
        ),
        .testTarget(
            name: "MagicianMacAudioEngineTests",
            dependencies: [
                "MagicianMacAudioEngine",
                "MagicianMacAudioEngineCore",
                .product(name: "Hummingbird", package: "hummingbird"),
                .product(name: "HummingbirdTesting", package: "hummingbird"),
                .product(name: "HummingbirdWSClient", package: "hummingbird-websocket"),
                .product(name: "HummingbirdWSTesting", package: "hummingbird-websocket"),
            ]
        ),
    ],
    // Declared here, not only as a build flag: this package is written and
    // tested in Swift 5 language mode (actors and Sendable contracts, without
    // Swift 6's promoted region diagnostics). With tools-version 6.0 the
    // default would be Swift 6, and overriding that from the Makefile alone
    // made every target warn that its language mode was overridden.
    swiftLanguageModes: [.v5]
)
