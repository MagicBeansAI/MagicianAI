import XCTest
import Foundation
import Combine
@testable import Magician

final class MockURLProtocol: URLProtocol {
    static var handler: ((URLRequest) throws -> (HTTPURLResponse, Data))?

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let handler = Self.handler else {
            XCTFail("MockURLProtocol received an unexpected request: \(request)")
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }
        do {
            let (response, data) = try handler(request)
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
        } catch {
            client?.urlProtocol(self, didFailWithError: error)
        }
    }

    override func stopLoading() {}
}

/// A URLProtocol whose response is completed explicitly by the test without
/// blocking the URL loading thread. Use it for real overlap/race tests: a
/// semaphore inside `startLoading` can starve requests from another session and
/// turn the intended interleaving into a timeout artifact.
final class DeferredMockURLProtocol: URLProtocol {
    typealias ResultValue = Result<(HTTPURLResponse, Data), Error>
    static var handler: ((URLRequest, @escaping (ResultValue) -> Void) -> Void)?

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let handler = Self.handler else {
            XCTFail("DeferredMockURLProtocol received an unexpected request: \(request)")
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }
        handler(request) { [weak self] result in
            guard let self else { return }
            switch result {
            case .success(let (response, data)):
                client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                client?.urlProtocol(self, didLoad: data)
                client?.urlProtocolDidFinishLoading(self)
            case .failure(let error):
                client?.urlProtocol(self, didFailWithError: error)
            }
        }
    }

    override func stopLoading() {}
}

func makeMockSession(chatCatalogFixture: Bool = false) -> URLSession {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = chatCatalogFixture ? [ChatCatalogFixtureURLProtocol.self, MockURLProtocol.self] : [MockURLProtocol.self]
    return URLSession(configuration: configuration)
}

func makeDeferredMockSession(chatCatalogFixture: Bool = false) -> URLSession {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = chatCatalogFixture ? [ChatCatalogFixtureURLProtocol.self, DeferredMockURLProtocol.self] : [DeferredMockURLProtocol.self]
    return URLSession(configuration: configuration)
}

func response(for request: URLRequest, status: Int = 200) -> HTTPURLResponse {
    HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!
}

func jsonData(_ object: Any) -> Data {
    try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
}

func requestBody(_ request: URLRequest) -> Data? {
    if let body = request.httpBody { return body }
    guard let stream = request.httpBodyStream else { return nil }
    stream.open()
    defer { stream.close() }
    var result = Data()
    var buffer = [UInt8](repeating: 0, count: 4096)
    while stream.hasBytesAvailable {
        let count = stream.read(&buffer, maxLength: buffer.count)
        guard count > 0 else { break }
        result.append(buffer, count: count)
    }
    return result
}

extension XCTestCase {
    func waitForMainQueue(file: StaticString = #filePath, line: UInt = #line) {
        let expectation = expectation(description: "main queue drained")
        DispatchQueue.main.async { expectation.fulfill() }
        wait(for: [expectation], timeout: 2)
    }
}

/// Streaming fixtures exercise the same typed model gate as production chat.
/// Catalog requests stay independent of each test's strict stream/event handler.
final class ChatCatalogFixtureURLProtocol: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { request.url?.path.hasSuffix("/chat/models") == true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let catalog: [String: Any] = [
            "models": [["id": "fixture/chat", "provider": "fixture", "connection": "fixture", "model": "chat", "label": "Fixture chat",
                "thinking": ["mode": "none", "levels": []], "choices": ["auto": "fixture-auto"]]],
            "default_selection": ["model_id": "fixture/chat", "thinking": "auto"]]
        client?.urlProtocol(self, didReceive: response(for: request), cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: jsonData(catalog))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

extension XCTestCase {
    func prepareChatModelCatalog(_ model: ChatViewModel, file: StaticString = #filePath, line: UInt = #line) {
        guard ["magician", "pi"].contains(model.selectedHarnessEngine) else { return }
        let selection = ChatModelSelection(modelId: "fixture/chat", thinking: "auto")
        UserDefaults.standard.set(try! JSONEncoder().encode(selection), forKey: model.chatLocalStorageKey + "|" + ChatModelPreference.storageKey)
        let ready = expectation(description: "typed chat model catalog ready")
        var fulfilled = false
        var observation: AnyCancellable?
        observation = model.$selectedProfile.dropFirst().sink { route in
            if !fulfilled, route == "fixture-auto" {
                fulfilled = true
                DispatchQueue.main.async { ready.fulfill() }
            }
        }
        model.fetchProfiles()
        wait(for: [ready], timeout: 5)
        observation?.cancel()
        model.selectedProfile = "fixture-auto"
        XCTAssertNil(model.modelSelectionIssue, file: file, line: line)
    }
}
