//
//  ThinkingMapRealtime.swift
//  Magios
//
//  Push subscriber for `ThinkingMapUpdated` change notices — replaces the old
//  ~2s Listen-mode poll. Subscribes to the app's `RealtimeHub` (the one
//  `/api/magician/v2/realtime/ws` socket every surface shares), filters
//  notices for ONE map id, and fires `onNotice` so the model re-fetches the
//  authoritative map via `getMap`.
//
//  Push is an ACCELERATOR, not the correctness path: the model keeps a slow
//  safety poll while listening, so a dropped socket or a missed notice costs
//  seconds — never a stale board. The notice carries no map payload by design
//  (the backend pushes `{map_id, revision}` only); the single authoritative
//  fetch path stays `LTM.SyncStore.refresh`.
//
//  The hub owns reconnects and resume; a hub resync (events may have been
//  missed) fires `onNotice` too, so the board re-reads. The hub is inert
//  under unit tests — the routing itself is unit-testable by feeding crafted
//  event JSON straight into `handleIncomingJSON`.

import Foundation

@MainActor
final class ThinkingMapRealtime {
    private var subscription: RealtimeHubSubscription?
    private var mapID: String?
    private var onNotice: (() -> Void)?
    private var onInterpretProgress: ((_ utteranceID: String, _ stage: String, _ nodeCount: Int?) -> Void)?
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
        subscription = RealtimeHub.shared.subscribe(
            RealtimeHubFilter(eventTypes: ["ThinkingMapUpdated", "ThinkingMapInterpretProgress"]),
            onEvent: { [weak self] event in self?.handleIncomingJSON(event.text) },
            onResync: { [weak self] in
                guard let self, self.active else { return }
                self.onNotice?()
            }
        )
    }

    /// Stop receiving notices. Idempotent.
    func stop() {
        active = false
        subscription?.cancel()
        subscription = nil
        mapID = nil
        onNotice = nil
        onInterpretProgress = nil
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
}
