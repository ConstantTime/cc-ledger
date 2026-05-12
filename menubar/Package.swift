// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "MenuBar",
    platforms: [.macOS(.v14)],
    targets: [
        .target(
            name: "MenuBarCore",
            linkerSettings: [.linkedLibrary("sqlite3")]
        ),
        .executableTarget(name: "MenuBar", dependencies: ["MenuBarCore"]),
        .executableTarget(name: "ClaudeUsageProbe", dependencies: ["MenuBarCore"]),
    ]
)
