import Foundation

/// The theme follows the account. `/api/magician/v2/ui/preferences` already
/// carries the web's theme choice; this joins the app to the same contract so
/// a theme picked on the web, Android or here shows everywhere:
///
/// - On launch/foreground the stored preference is adopted when one exists
///   (the account is the truth — the same rule the web applies on focus);
///   when none exists, the local choice seeds the account, again as the web
///   does.
/// - An explicit pick here is pushed at once (see `ThemeManager.themeSyncSink`).
///
/// Everything is fire-and-forget: launch never waits on the network, and an
/// unpaired app or a failing call leaves the locally saved theme untouched.
enum ThemeSync {
    private static var endpoint: URL {
        MagicianAccess.baseURL.appendingPathComponent("api/magician/v2/ui/preferences")
    }

    /// UI tests drive the appearance switcher directly; a concurrent account
    /// reconcile (or push) would fight the value under test.
    private static var disabled: Bool { isUITestLaunch }

    /// Push an explicit pick: the resolved variant plus the appearance mode.
    static func push(theme: String, appearance: String) {
        guard !disabled, MagicianAccess.isConfigured else { return }
        var request = URLRequest(url: endpoint)
        request.httpMethod = "PUT"
        request.addValue("application/json", forHTTPHeaderField: "content-type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: [
            "theme": theme,
            "appearance": appearance,
        ])
        MagicianAccess.authorize(&request)
        URLSession.shared.dataTask(with: request) { _, _, _ in }.resume()
    }

    /// Adopt the account's stored choice when one exists. Runs on the shared
    /// session; the completion hops to the main queue because ThemeManager is
    /// main-thread confined.
    static func reconcile(systemDark: Bool) {
        guard !disabled, MagicianAccess.isConfigured else { return }
        var request = URLRequest(url: endpoint)
        MagicianAccess.authorize(&request)
        URLSession.shared.dataTask(with: request) { data, _, _ in
            guard let data,
                  let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  (body["saved"] as? Bool) == true,
                  let rawTheme = body["theme"] as? String
            else { return }
            let appearance = body["appearance"] as? String
            DispatchQueue.main.async {
                ThemeManager.shared.adoptAccountTheme(rawTheme, appearance: appearance, systemDark: systemDark)
            }
        }.resume()
    }
}
