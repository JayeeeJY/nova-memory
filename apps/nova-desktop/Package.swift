// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "NovaMemoryDesktop",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "NovaDesktopCore", targets: ["NovaDesktopCore"]),
        .executable(name: "NovaMemoryDesktop", targets: ["NovaMemoryDesktop"])
    ],
    targets: [
        .target(name: "NovaDesktopCore"),
        .executableTarget(name: "NovaMemoryDesktop", dependencies: ["NovaDesktopCore"]),
        .testTarget(name: "NovaDesktopCoreTests", dependencies: ["NovaDesktopCore"])
    ]
)
