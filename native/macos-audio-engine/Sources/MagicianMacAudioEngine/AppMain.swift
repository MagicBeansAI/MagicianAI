import Darwin
import Foundation
import Hummingbird

@main
enum MagicianMacAudioEngineMain {
    static func main() async throws {
        let loaded = try EngineConfiguration.load()
        let runtime = EngineRuntime(configuration: loaded.config)

        Task { await runtime.prewarm() }

        Task {
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(5))
                if await runtime.performIdleSweep() { Darwin.exit(EXIT_SUCCESS) }
            }
        }

        let application = makeApplication(runtime: runtime, token: loaded.token, port: loaded.port)
        try await application.run()
    }
}
