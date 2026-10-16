import Foundation

/// True when the process is under test — either hosting an XCTest bundle (unit
/// tests, where the tests run in the app process) OR launched by a UI test with
/// the `--ui-test` argument (where the app runs in its own process and would
/// otherwise have no XCTest env). Used to skip real network / WebSocket connects,
/// speech synthesis, and health polling so tests get a fast, deterministic,
/// offline app — and to suppress diagnostic logging. Keeping the app idle also
/// makes XCUITest's wait-for-idle fast + reliable (no flaky timeouts under load).
let isRunningUnderTests = ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
    || ProcessInfo.processInfo.arguments.contains("--ui-test")

/// True ONLY when the app is launched by a UI test (`--ui-test`), NOT during unit
/// tests. Used to skip on-appear HTTP data fetches that would otherwise hang on an
/// unreachable backend and keep the app "busy" — dragging XCUITest's wait-for-idle
/// out ~30s per launch. Kept separate from `isRunningUnderTests` because unit tests
/// DO exercise those fetches (through mocked sessions), so they must not be skipped.
let isUITestLaunch = ProcessInfo.processInfo.arguments.contains("--ui-test")

/// Diagnostic logging that is silent under XCTest and in release builds, so it
/// never pollutes the test console but stays available for local debug runs.
@inline(__always)
func debugLog(_ message: @autoclosure () -> String) {
    #if DEBUG
    if !isRunningUnderTests { print(message()) }
    #endif
}
