import SwiftUI

/// "How to use" — a grouped list of commands / usage tips. Pushed from Settings.
struct HowToUseView: View {
    @StateObject private var themeManager = ThemeManager.shared
    @ObservedObject private var siriIdentity = PrimaryAgentSiriAdvertiser.shared

    private struct Tip: Identifiable { let id = UUID(); let icon: String; let title: String; let detail: String }
    private struct Group: Identifiable { let id = UUID(); let header: String; let tips: [Tip] }

    private var groups: [Group] { [
        Group(header: "Connect", tips: [
            Tip(icon: "qrcode.viewfinder", title: "Connect to your Magician", detail: "Choose the same route on both devices: Same Wi-Fi when this iPhone shares a trusted network with the computer, or Remote when it does not. On the computer, open Magician Settings → Mobile devices → Connect iPhone and create that route's QR. Here, open Settings → Self-hosted connection, select the route, scan its QR, and confirm the hostname.")
        ]),
        Group(header: "At a glance", tips: [
            Tip(icon: "rectangle.3.group.fill", title: "Add one Magican widget", detail: "Long-press the Home Screen, tap Edit → Add Widget, and choose Magican at a glance. Its sizes are one product: something needing you comes first, then active work, then Talk when the day is clear."),
            Tip(icon: "bolt.horizontal.circle.fill", title: "Keep task progress live", detail: "Settings → At a glance has one remote-update setup. It covers generic Attention alerts and lets task Live Activities keep moving when Magican is not open. Listen and ambient Talk activities remain local to this iPhone and can never be started or extended by a server push.")
        ]),
        Group(header: "Siri & Shortcuts", tips: [
            Tip(
                icon: "mic.fill",
                title: siriIdentity.preferredName.map { "\"Hey Siri, ask \($0) using Magican\"" }
                    ?? "\"Hey Siri, Ask Magican\"",
                detail: siriIdentity.alternateNames.isEmpty
                    ? "Siri asks for your request and sends it to the current primary assistant."
                    : "Every advertised primary-assistant alias works with the same “Ask … using Magican” form: \(siriIdentity.alternateNames.joined(separator: ", "))."
            ),
            Tip(icon: "bubble.left.and.bubble.right.fill", title: "\"New chat in Magican\"", detail: "Opens the app straight to a fresh chat."),
            Tip(icon: "graduationcap.fill", title: "\"Ask Tutor on Magican\"", detail: "Opens a blackboard Tutor ready to explain a concept — no screenshot needed."),
            Tip(icon: "bolt.circle", title: "Action Button / Shortcuts", detail: "Add Talk to Magican, New Chat, Ask Assistant, or Ask Tutor to your Action Button or a Shortcut in the Shortcuts app.")
        ]),
        Group(header: "Chat", tips: [
            Tip(icon: "paperplane.fill", title: "Send a message", detail: "Type and tap Send. Toggle PLAN to plan instead of act."),
            Tip(icon: "at", title: "Mention with @", detail: "Type @ to reference an agent, tool, or personality — e.g. @tutor, @agent, @skill."),
            Tip(icon: "graduationcap.fill", title: "Ask the Tutor", detail: "Type @tutor and a concept for a blackboard explanation — it darkens the screen and draws + narrates. Attach an image with @tutor to have it annotate that image instead. From any app: screenshot → Share → Magios → Ask Tutor. Completed guides can Replay, Keep Showing, or Ask Again.")
        ]),
        Group(header: "Thinking Maps", tips: [
            Tip(icon: "point.3.connected.trianglepath.dotted", title: "Start with an idea", detail: "Open Thinking Map from the app drawer, tap Brainstorm an idea in Observe, or type @brainstorm followed by your thought in Chat. A bare @brainstorm opens capture for a fresh map."),
            Tip(icon: "mic.fill", title: "Grow the active branch", detail: "Speak or type the messy version. Select any thought before capturing the next one to branch from it, and use Break open when you want several different directions."),
            Tip(icon: "square.and.arrow.up", title: "Return, organize, and harvest", detail: "Use Ideas to resume earlier maps. Switch between Canvas, Focus, and Outline; edit or connect thoughts; then Harvest to copy or share a concise result without deleting the map.")
        ]),
        Group(header: "Tasks", tips: [
            Tip(icon: "note.text.badge.plus", title: "Publish a finished task to Notes", detail: "For a completed, failed, or cancelled task, open Actions → Publish to Notes, or tap the note-plus button at the top of its full task view. Magican uses your scoped Notes provider settings and updates the task's stable page when you publish it again.")
        ]),
        Group(header: "Today", tips: [
            Tip(icon: "sun.max.fill", title: "See what matters now", detail: "Use the Today tab for Needs You, follow-ups, active work, deliveries, durable changes, resurfaced context, and briefings in one compact view."),
            Tip(icon: "hand.draw", title: "Act without opening every card", detail: "Swipe or long-press a Today card to snooze or dismiss it. Hidden cards can be restored from the collapsed Hidden section."),
            Tip(icon: "clock.badge", title: "Create an Apple Reminder", detail: "On any Worth a look card that offers Create reminder, choose a title and time. Magican asks for Reminders access once, creates the reminder with its note and alert, then opens Apple Reminders. This is separate from Create task."),
            Tip(icon: "clock.arrow.circlepath", title: "Review the history", detail: "Expand Activity at the bottom of Today to search and filter durable learnings, outcomes, failures, and deliveries.")
        ]),
        Group(header: "Observe", tips: [
            Tip(icon: "ear.badge.waveform", title: "Listen to the room", detail: "Open the Observe tab and tap Listen here. Put your phone on the table — Magican transcribes the meeting or conversation and writes a live transcript, summary, and notes into a meeting thread. No Mac needed."),
            Tip(icon: "waveform.badge.plus", title: "Share screen, system audio + my mic", detail: "Capture the meeting's audio (what you hear), your own voice as a separate \"You\" track, AND your screen — slides and shared docs get noted into the same meeting thread. Tap Prepare session, then Start Broadcast with Microphone on. iOS won't capture protected FaceTime/VoIP audio."),
            Tip(icon: "calendar", title: "Upcoming meetings", detail: "Your next calendar meetings show under Upcoming. Join (Me) opens the link so you attend it yourself, Listen captures it in-app, and Send bot dispatches the agent attendee."),
            Tip(icon: "person.wave.2.fill", title: "Send the bot to a Google Meet", detail: "For a call running elsewhere, paste a meet.google.com link under \"Join as bot\" — the agent joins the call directly and takes notes."),
            Tip(icon: "text.bubble", title: "Open the transcript", detail: "Tap \"Open transcript\" to jump to the meeting thread with the live transcript and summaries as they arrive."),
            Tip(icon: "book.pages.fill", title: "Browse Published Notes", detail: "Published task pages appear at the bottom of Observe. Search them, choose a page size, move between server pages, open a page in the protected Notes editor, publish the next completed-task batch, or deliberately send a page to memory review."),
            Tip(icon: "exclamationmark.triangle", title: "What it captures", detail: "Listen here records the whole room from the phone's mic — a meeting app's mute won't stop it, and nearby conversations are transcribed too. A phone call or another app taking the mic pauses capture; tap Resume to continue. Tap Stop when you're done.")
        ]),
        Group(header: "Voice", tips: [
            Tip(icon: "mic", title: "Dictate without keeping audio", detail: "Tap the Chat mic once to start and again to stop, or press and hold it while speaking and release when finished. By default only the transcript enters the composer; the temporary recording is removed. Auto-send starts after 3s, and Cancel lets you edit first."),
            Tip(icon: "waveform.badge.plus", title: "Choose whether to keep Audio Notes", detail: "Open the composer's voice settings → Dictation and enable Keep dictation recordings. The mic then says when it is saving. Saved and pending recordings appear under Menu → Audio Notes, where you can play, search, retry, or delete them. New notes use your scoped default Notes provider and its configured fallback."),
            Tip(icon: "speaker.wave.2.fill", title: "Voice options", detail: "The speaker menu in the composer: auto-speak on/off, and On-device vs Magican for speech-to-text / text-to-speech."),
            Tip(icon: "text.bubble", title: "Hear replies", detail: "Tap \"Speak\" under any reply. If you dictated the message, the reply is spoken back automatically."),
            Tip(
                icon: "graduationcap.fill",
                title: "Say “Tutor …” or “Tutor Quick …”",
                detail: "In Dictate, Hands-free, or Live, begin with “Tutor explain recursion” for a full lesson or “Tutor Quick explain recursion” for a shorter one. Both open the source-free blackboard and start automatically; you can also say “Tutor blackboard …” explicitly."
            ),
            Tip(
                icon: "iphone.slash",
                title: "Screen Tutor and App Copilot",
                detail: "iPhone voice cannot capture another app's screen. “Tutor screen …” and “App Copilot …” tell you they are unavailable instead of starting the wrong flow. To tutor a screen, take a screenshot and use Share → Magican."
            ),
            Tip(
                icon: "lock.fill",
                title: "Unlock before starting Tutor",
                detail: "If the iPhone is locked, Magican asks you to unlock and starts nothing. Unlock, then say the Tutor request again — a rejected request is never queued to appear later."
            )
        ]),
        Group(header: "Share into Magican", tips: [
            Tip(icon: "square.and.arrow.up", title: "Share sheet", detail: "From any app, Share → Magican. A single screenshot can open Tutor directly; other images, text, links, and files go into chat.")
        ]),
        Group(header: "Results & files", tips: [
            Tip(icon: "doc.richtext", title: "Open a result", detail: "Tap a result card to view it in-app — HTML, PDF, image, video, audio, or text."),
            Tip(icon: "square.and.arrow.up", title: "Keep it handy", detail: "Open a result in the authenticated viewer, then use Share file. External links still offer browser and copy actions.")
        ]),
        Group(header: "Attention", tips: [
            Tip(icon: "bell.badge.fill", title: "Respond to requests", detail: "The Attention tab lists things needing you — approvals, questions, passwords, choices. Tap one to respond.")
        ])
    ] }

    var body: some View {
        List {
            ForEach(groups) { group in
                Section(header: Text(group.header).foregroundColor(themeManager.secondaryTextColor)) {
                    ForEach(group.tips) { tip in
                        HStack(alignment: .top, spacing: 14) {
                            Image(systemName: tip.icon)
                                .font(.system(size: 16))
                                .foregroundColor(themeManager.accentColor)
                                .frame(width: 26)
                                .padding(.top, 2)
                            VStack(alignment: .leading, spacing: 3) {
                                Text(tip.title).font(themeManager.font(15, weight: .semibold)).foregroundColor(themeManager.textColor)
                                Text(tip.detail).font(themeManager.font(13)).foregroundColor(themeManager.secondaryTextColor)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        }
                        .padding(.vertical, 4)
                    }
                }
                .listRowBackground(themeManager.surfaceColor)
            }
        }
        .listStyle(InsetGroupedListStyle())
        .scrollContentBackground(.hidden)
        .background(themeManager.backgroundColor.ignoresSafeArea())
        .navigationTitle("How to Use")
        .navigationBarTitleDisplayMode(.inline)
    }
}
