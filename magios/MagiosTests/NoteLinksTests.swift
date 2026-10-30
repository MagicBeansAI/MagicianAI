import XCTest
@testable import Magician

final class NoteLinksTests: XCTestCase {
    private func profile(_ origin: String, workspace: String) throws -> MobileConnectionProfile {
        try MobileConnectionProfile(
            publicOrigin: try XCTUnwrap(URL(string: origin)),
            principal: "owner",
            workspace: workspace,
            deviceID: "iphone-1",
            deviceToken: "device-token"
        )
    }

    func testRelativeLinkDecodesPathAndWorkspace() {
        let link = NoteLink.parse("/notes?path=Tasks%2F2026-10-21%2FWeekly%20report%2Bnotes.md&workspace=personal")
        XCTAssertEqual(link, NoteLink(path: "Tasks/2026-10-21/Weekly report+notes.md", workspace: "personal", origin: nil))
    }

    func testRelativeLinkWithoutWorkspaceOpensInTheCurrentWorkspace() throws {
        let active = try profile("https://mac.example.com", workspace: "personal")
        let link = try XCTUnwrap(NoteLink.parse("/notes?path=Inbox.md"))
        XCTAssertNil(link.workspace)
        XCTAssertEqual(NoteLinkResolution.resolve(link, active: active, profiles: [active]), .open(path: "Inbox.md"))
    }

    func testRelativeLinkInTheCurrentWorkspaceOpens() throws {
        let active = try profile("https://mac.example.com", workspace: "personal")
        let link = try XCTUnwrap(NoteLink.parse("/notes/?path=a.md&workspace=personal#top"))
        XCTAssertEqual(NoteLinkResolution.resolve(link, active: active, profiles: [active]), .open(path: "a.md"))
    }

    func testAbsoluteLinkOnThePairedOriginOpens() throws {
        let active = try profile("https://mac.example.com", workspace: "personal")
        let link = try XCTUnwrap(NoteLink.parse("https://MAC.example.com/notes?path=a.md&workspace=personal"))
        XCTAssertEqual(link.origin, "https://mac.example.com")
        XCTAssertEqual(NoteLinkResolution.resolve(link, active: active, profiles: [active]), .open(path: "a.md"))
    }

    func testAbsoluteLinkOnAnotherPairedMachineSwitchesToIt() throws {
        let active = try profile("https://mac.example.com", workspace: "personal")
        let other = try profile("https://studio.example.com", workspace: "team")
        let link = try XCTUnwrap(NoteLink.parse("https://studio.example.com/notes?path=a.md&workspace=team"))
        XCTAssertEqual(
            NoteLinkResolution.resolve(link, active: active, profiles: [active, other]),
            .switchMachine(origin: "https://studio.example.com", path: "a.md")
        )
    }

    func testAbsoluteLinkOnAnUnpairedOriginIsLeftToTheBrowser() throws {
        let active = try profile("https://mac.example.com", workspace: "personal")
        let link = try XCTUnwrap(NoteLink.parse("https://elsewhere.example.com/notes?path=a.md"))
        XCTAssertEqual(NoteLinkResolution.resolve(link, active: active, profiles: [active]), .notOurs)
    }

    func testAbsoluteLinkNamingAWorkspaceTheOriginIsNotSignedInToIsRefused() throws {
        let active = try profile("https://mac.example.com", workspace: "personal")
        let link = try XCTUnwrap(NoteLink.parse("https://mac.example.com/notes?path=a.md&workspace=team"))
        XCTAssertEqual(NoteLinkResolution.resolve(link, active: active, profiles: [active]), .otherWorkspace("team"))
    }

    func testSchemeLinkParsesLikeTheRelativeOne() throws {
        let link = try XCTUnwrap(NoteLink.parse("magican://notes?path=Daily%2F2026-10-21.md&workspace=personal"))
        XCTAssertEqual(link, NoteLink(path: "Daily/2026-10-21.md", workspace: "personal", origin: nil))
        let url = try XCTUnwrap(URL(string: "magican://notes/?path=x.md"))
        XCTAssertEqual(NoteLink.parse(url)?.path, "x.md")
    }

    func testOtherWorkspaceIsRefusedWithoutLoading() throws {
        let active = try profile("https://mac.example.com", workspace: "personal")
        let link = try XCTUnwrap(NoteLink.parse("/notes?path=a.md&workspace=team"))
        XCTAssertEqual(NoteLinkResolution.resolve(link, active: active, profiles: [active]), .otherWorkspace("team"))
    }

    func testOtherWorkspaceOnExactlyOnePairedMachineSwitches() throws {
        let active = try profile("https://mac.example.com", workspace: "personal")
        let team = try profile("https://studio.example.com", workspace: "team")
        let link = try XCTUnwrap(NoteLink.parse("magican://notes?path=a.md&workspace=team"))
        XCTAssertEqual(
            NoteLinkResolution.resolve(link, active: active, profiles: [active, team]),
            .switchMachine(origin: "https://studio.example.com", path: "a.md")
        )
        let second = try profile("https://office.example.com", workspace: "team")
        XCTAssertEqual(
            NoteLinkResolution.resolve(link, active: active, profiles: [active, team, second]),
            .otherWorkspace("team")
        )
    }

    func testMalformedAndForeignLinksAreNotNoteLinks() {
        XCTAssertNil(NoteLink.parse(""))
        XCTAssertNil(NoteLink.parse("/notes"))
        XCTAssertNil(NoteLink.parse("/notes?path="))
        XCTAssertNil(NoteLink.parse("/notes?path=%20%20"))
        XCTAssertNil(NoteLink.parse("/notes?workspace=personal"))
        XCTAssertNil(NoteLink.parse("/notesx?path=a.md"))
        XCTAssertNil(NoteLink.parse("/notes/a.md"))
        XCTAssertNil(NoteLink.parse("notes?path=a.md"))
        XCTAssertNil(NoteLink.parse("/tasks?selected=task_1"))
        XCTAssertNil(NoteLink.parse("magican://task/abc?path=a.md"))
        XCTAssertNil(NoteLink.parse("ftp://mac.example.com/notes?path=a.md"))
        XCTAssertNil(NoteLink.parse("https://user:secret@mac.example.com/notes?path=a.md"))
        XCTAssertNil(NoteLink.parse("http://public.example.com/notes?path=a.md"))
    }
}
