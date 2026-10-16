import XCTest
import Foundation
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

func makeMockSession() -> URLSession {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [MockURLProtocol.self]
    return URLSession(configuration: configuration)
}

func makeDeferredMockSession() -> URLSession {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [DeferredMockURLProtocol.self]
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
