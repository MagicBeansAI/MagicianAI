import Foundation
import SwiftUI

enum ServiceHealthState: Equatable {
    case checking
    case online
    case offline
    case notReported

    var label: String {
        switch self {
        case .checking: return "Checking…"
        case .online: return "Online"
        case .offline: return "Offline"
        case .notReported: return "Not reported"
        }
    }

    var color: Color {
        let theme = ThemeManager.shared
        switch self {
        case .online: return theme.successColor
        case .offline: return theme.dangerColor
        case .checking, .notReported: return theme.warningColor
        }
    }
}

class HealthViewModel: ObservableObject {
    static let shared = HealthViewModel()

    @Published var magicianStatus: Bool = false
    @Published var magicutorStatus: Bool = false
    @Published var tauriStatus: Bool = false
    @Published private(set) var magicianState: ServiceHealthState = .checking
    @Published private(set) var magicutorState: ServiceHealthState = .checking
    @Published private(set) var tauriState: ServiceHealthState = .checking
    @Published private(set) var isChecking = false
    
    private var timer: Timer?
    private let networkSession: URLSession
    
    init(networkSession: URLSession = .shared, startsPolling: Bool = true) {
        self.networkSession = networkSession
        if startsPolling { startPolling() }
    }
    
    deinit {
        timer?.invalidate()
    }
    
    func startPolling() {
        guard !isRunningUnderTests, timer == nil else { return }   // no real polling in unit tests
        checkHealth()
        timer = Timer.scheduledTimer(withTimeInterval: 10.0, repeats: true) { [weak self] _ in
            self?.checkHealth()
        }
    }
    
    func checkHealth() {
        guard !isChecking else { return }
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/health") else { return }
        isChecking = true
        
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        request.httpMethod = "GET"
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            DispatchQueue.main.async {
                guard let self = self else { return }
                self.isChecking = false
                
                if let error = error {
                    debugLog("Health check error: \(error)")
                    self.setAllOffline()
                    return
                }
                
                guard let httpResponse = response as? HTTPURLResponse else {
                    self.setAllOffline()
                    return
                }
                
                if httpResponse.statusCode == 200 {
                    self.magicianStatus = true
                    self.magicianState = .online
                    
                    if let data,
                       let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                        self.magicutorState = Self.state(json, keys: ["magicutor_status", "magicutor"])
                        self.tauriState = Self.state(json, keys: ["tauri_status", "tauri"])
                        self.magicutorStatus = self.magicutorState == .online
                        self.tauriStatus = self.tauriState == .online
                    } else {
                        // Magician is reachable, but dependency status was not reported.
                        self.setDependenciesUnreported()
                    }
                } else {
                    self.setAllOffline()
                }
            }
        }.resume()
    }
    
    private func setAllOffline() {
        magicianStatus = false
        magicutorStatus = false
        tauriStatus = false
        magicianState = .offline
        // If the aggregator is unreachable, dependency state is unknown rather than offline.
        magicutorState = .notReported
        tauriState = .notReported
    }

    private func setDependenciesUnreported() {
        magicutorStatus = false
        tauriStatus = false
        magicutorState = .notReported
        tauriState = .notReported
    }

    private static func state(_ json: [String: Any], keys: [String]) -> ServiceHealthState {
        guard let raw = keys.compactMap({ json[$0] as? String }).first?.lowercased() else {
            return .notReported
        }
        if ["healthy", "online", "ok", "ready"].contains(raw) { return .online }
        if ["offline", "unhealthy", "unreachable", "failed", "error"].contains(raw) { return .offline }
        return .notReported
    }
}
