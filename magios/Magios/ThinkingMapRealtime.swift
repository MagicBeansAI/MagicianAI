//
//  ThinkingMapRealtime.swift
//  Magios
//
//  Push subscriber for `ThinkingMapUpdated` change notices — replaces the old
//  ~2s Listen-mode poll. Connects to the SAME `/api/magician/v2/realtime/ws`
//  every other surface uses (chat, tasks, attention, …), filters notices for
//  ONE map id, and fires `onNotice` so the model re-fetches the authoritative
//  map via `getMap`.
//
//  Push is an ACCELERATOR, not the correctness path: the model keeps a slow
//  safety poll while listening, so a dropped socket or a missed notice costs
//  seconds — never a stale board. The notice carries no map payload by design
//  (the backend pushes `{map_id, revision}` only); the single authoritative
//  fetch path stays `LTM.SyncStore.refresh`.
//
//  Reconnects with capped exponential backoff while active. Inert under unit
//  tests (`isRunningUnderTests`) — the routing itself is unit-testable by
//  feeding crafted event JSON straight into `handleIncomingJSON`.

import Foundation

@MainActor
final class ThinkingMapRealtime {
    private var webSocket: URLSessionWebSocketTask?
    private var session: URLSession?
    private var mapID: String?
    private var onNotice: (() -> Void)?
    private var onInterpretProgress: ((_ utteranceID: String, _ stage: String, _ nodeCount: Int?) -> Void)?
    private var reconnectAttempt = 0
    private var reconnectTask: Task<Void, Never>?
    private(set) var active = false

    /// Begin streaming notices for `mapID`. Replaces any prior subscription.
    ///
    /// `onInterpretProgress` additionally routes `ThinkingMapInterpretProgress`
    /// stage events for the same map — the narration of an owner-triggered
    /// `/interpret` run. The stage arrives as the server's wire string so this
    /// layer stays a router; mapping it onto display states (and deciding
    /// whether the run is *ours*, by utterance id) is the subscriber's job.
    func start(
        mapID: String,
        onNotice: @escaping () -> Void,
        onInterpretProgress: ((_ utteranceID: String, _ stage: String, _ nodeCount: Int?) -> Void)? = nil
    ) {
        stop()
        self.mapID = mapID
        self.onNotice = onNotice
        self.onInterpretProgress = onInterpretProgress
        active = true
        reconnectAttempt = 0
        connect()
    }

    /// Tear the socket down and stop reconnecting. Idempotent.
    func stop() {
        active = false
        reconnectTask?.cancel()
        reconnectTask = nil
        webSocket?.cancel(with: .goingAway, reason: nil)
        webSocket = nil
        session?.invalidateAndCancel()
        session = nil
        mapID = nil
        onNotice = nil
        onInterpretProgress = nil
    }

    private func connect() {
        guard active, !isRunningUnderTests else { return }
        // The paired bearer authorizes the upgrade and binds the event scope.
        guard let url = URL(
            string: "\(MagicianAccess.webSocketBaseURL.absoluteString)/api/magician/v2/realtime/ws")
        else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        let session = URLSession(
            configuration: .default, delegate: nil, delegateQueue: OperationQueue.main)
        self.session = session
        webSocket = session.webSocketTask(with: request)
        webSocket?.resume()
        receive()
    }

    private func receive() {
        webSocket?.receive { [weak self] result in
            DispatchQueue.main.async {
                guard let self, self.active else { return }
                switch result {
                case .success(let message):
                    if case .string(let text) = message {
                        self.handleIncomingJSON(text)
                    }
                    // A live frame means the connection is healthy again.
                    self.reconnectAttempt = 0
                    self.receive()
                case .failure(let error):
                    debugLog("[thinking-map-realtime] receive error: \(error)")
                    self.scheduleReconnect()
                }
            }
        }
    }

    /// Route one realtime frame. Internal (not private) so the filtering can be
    /// unit-tested by feeding crafted JSON without a socket.
    func handleIncomingJSON(_ jsonString: String) {
        guard let mapID,
              let data = jsonString.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let eventType = object["event_type"] as? String,
              let payload = object["data"] as? [String: Any],
              payload["map_id"] as? String == mapID
        else { return }
        switch eventType {
        case "ThinkingMapUpdated":
            onNotice?()
        case "ThinkingMapInterpretProgress":
            // A stage event without its run id can never be claimed by anyone;
            // the id is what separates this device's run from an ambient
            // auto-map narrated against the same board.
            guard let utteranceID = payload["utterance_id"] as? String,
                  let stage = payload["stage"] as? String
            else { return }
            onInterpretProgress?(utteranceID, stage, payload["node_count"] as? Int)
        default:
            return
        }
    }

    private func scheduleReconnect() {
        guard active else { return }
        webSocket = nil
        // 1s, 2s, 4s, 8s, then 15s — capped so a long backend outage keeps a
        // slow heartbeat of attempts while the safety poll carries correctness.
        let delay = min(15.0, pow(2.0, Double(min(reconnectAttempt, 3))))
        reconnectAttempt += 1
        reconnectTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            guard let self, self.active, !Task.isCancelled else { return }
            self.connect()
        }
    }
}
