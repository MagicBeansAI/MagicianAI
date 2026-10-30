import SwiftUI

/// Task-based help for the screens shipped in the iPhone app.
struct HowToUseView: View {
    @ObservedObject private var theme = ThemeManager.shared
    @State private var search = ""
    @State private var section = ""

    private struct Tip: Identifiable {
        let title: String
        let detail: String
        var id: String { title }
    }
    private struct Group: Identifiable {
        let header: String
        let icon: String
        let tips: [Tip]
        var id: String { header }
    }

    private static let topics: [Group] = [
        Group(header: "Getting around", icon: "square.grid.2x2", tips: [
            Tip(title: "Find your way around",
                detail: "Use the bottom tabs for Chat, Work, Today, Attention and Observe. Open the menu at the top left for Thinking Map, Notes, VibeDev, Audio Notes, Apps, Settings and Themes. Back returns from a detail screen; close a sheet to return to the screen underneath.")
        ]),
        Group(header: "Connect", icon: "qrcode.viewfinder", tips: [
            Tip(title: "Connect this iPhone to your computer",
                detail: "On the computer, open Settings → Connected devices → Connect iPhone and choose Same Wi-Fi or Remote. On this iPhone, open Settings → Connection, choose the same route, scan the QR and confirm the displayed address. Same Wi-Fi is direct on a trusted shared network; Remote uses your configured secure address."),
            Tip(title: "Switch between your machines",
                detail: "Open Settings → Connection to switch to a machine you already paired with. A newly discovered machine still needs that machine’s connection QR. Check Current Endpoint and Current Route under Connection when data appears to come from the wrong computer. Forget removes the saved pairing from this iPhone.")
        ]),
        Group(header: "Chat", icon: "bubble.left.and.bubble.right", tips: [
            Tip(title: "Send a message or make a plan",
                detail: "Type your request and send it. Do asks the assistant to carry out the request; Plan asks it to prepare a plan first. The Ask/Accept control chooses whether edits need your review. Review the proposed plan and choose the action you want before starting it."),
            Tip(title: "Keep conversations organized",
                detail: "Tap the composer’s conversation title or history control to open sessions and threads. Pick one to continue it, or use + for a new session. The three-dot menu contains actions for the current conversation. Changing sessions keeps each conversation’s messages together."),
            Tip(title: "Choose an engine, provider and model",
                detail: "Open the model selector in the composer. Choose the engine that will handle the conversation, then a provider and model where those choices are offered. Engines such as Claude Code and Codex offer their own models. An unavailable model list may mean the connected computer needs attention."),
            Tip(title: "Set thinking effort",
                detail: "Open the model selector and move the Thinking slider to a supported level. Auto uses that model or engine’s default; it does not start a separate adaptive escalation policy. Off is offered only when the model supports it. More thinking can improve difficult answers and take longer."),
            Tip(title: "Attach a file or use the camera",
                detail: "Use the attachment icon to choose images, documents, audio or video. Use the camera control when it is offered to add a photo. If a model cannot accept images, choose a compatible model before sending an image request."),
            Tip(title: "Use mentions and slash commands",
                detail: "Type @ to choose an agent, personality, skill or Tutor from the suggestions. Type / to see available composer commands, including engine and model choices. Select a suggestion rather than guessing a command name."),
            Tip(title: "Read, copy and check an answer",
                detail: "Use the reply’s playback control to hear it and its message actions to copy or share it. An info icon means the answer has a verification note: tap once to read the saved reason. The main answer keeps its headings, paragraphs, links and figures."),
            Tip(title: "Inspect what happened in a run",
                detail: "Tap Steps or a run’s activity control to inspect its execution. Run shows progress, steps, events, model usage and timing; Output or Result shows the delivered answer and files. Plan and History are available in the full task view. An info icon beside a summary opens its verification reason."),
            Tip(title: "Understand the assistant orb",
                detail: "The animated orb indicates assistant activity. Tapping it changes God mode for that conversation when the control is available. God mode permits broader actions on the connected computer; review its explanation before enabling it. Your normal approval controls still matter.")
        ]),
        Group(header: "Voice", icon: "waveform", tips: [
            Tip(title: "Switch between typing, dictation and conversation",
                detail: "Use the composer’s microphone and voice menu to choose the available Dictation, Hands-free or Live mode. Dictation turns speech into a message; conversation modes let you speak and hear responses. Use Type instead to return to the keyboard, and stop the voice session when finished."),
            Tip(title: "Choose speech and speaking voices",
                detail: "Open voice options in the composer to choose the available speech services and speaking voice. Settings → Voice & audio sets this iPhone’s own speaking voice; provider speaking voices are in Settings → Magican settings. Live calls have their own engine and voice choices; changing a chat model or thinking slider does not replace the live call engine."),
            Tip(title: "Keep or discard dictation recordings",
                detail: "Dictation normally uses the transcript without keeping a permanent recording. Enable Keep dictation recordings in the dictation settings if you want Audio Notes. Open Menu → Audio Notes to search recordings, play them, retry pending work or delete recordings you no longer need."),
            Tip(title: "Use dictation without sending too soon",
                detail: "Tap the mic to start and again to stop, or hold it while speaking and release when finished. Dictation fills the composer. When an auto-send countdown appears, Cancel gives you time to edit the transcript before sending."),
            Tip(title: "Keep Talk available",
                detail: "Open Settings → Voice & audio to enable availability, choose Conversation mode and set Stay available for. Use the displayed Talk control or the Magican Control Center control to start and stop. Availability and room listening are separate sessions; stop the one you no longer need.")
        ]),
        Group(header: "Work", icon: "checklist", tips: [
            Tip(title: "Choose the right Work view",
                detail: "All work brings together Tasks, Monitors and System work. Needs you collects decisions blocking progress. Crew is your AI team, Playbooks hold reusable goals and instructions, Quality checks review outputs, and Team routines coordinates scheduled company work. Tap the info controls for a reminder of what a view does."),
            Tip(title: "Create, find and manage tasks",
                detail: "Open Work → All work → Tasks. Use search, filters and page controls to find a task, or New work to set an outcome, agent and schedule. Open a task to review its result, plan and run. Its actions menu offers the controls allowed by its current state, such as edit, run, stop, retry, reset or mark done."),
            Tip(title: "Use monitors and inspect system work",
                detail: "Choose Monitors for scheduled checks and their history. Choose System work for supporting background jobs. Open a row to inspect the exact run rather than starting another task. Search and filters help narrow long lists; show more or change the page when the current list is incomplete."),
            Tip(title: "Read results and partial completion",
                detail: "A task marked Partly done has finished with requirements still open. Read Open: and the result before deciding what to do next. Open delivered files from Output. Run contains the step checklist and activity; expand technical details when you need older runs, IDs, related tools or intermediate evidence."),
            Tip(title: "Build your Crew",
                detail: "Open Crew to create or edit a member’s role, instructions, tools and model choices. Choose a primary assistant when you want that member to answer ordinary conversations. A crew member’s detail shows its work and controls; pause or resume it there when those actions are available."),
            Tip(title: "Set Autopilot, work cycles and focus areas",
                detail: "Autopilot lets a crew member choose useful work from its playbook. Configure work cycles, task and step limits, and focus areas to bound that work. Give each focus area a priority, instructions and schedule, and choose its shared documents. Delegation controls which teammates the member can ask for help."),
            Tip(title: "Edit Playbooks and shared documents",
                detail: "Open Work → Playbooks to manage reusable goals, instructions, checks and documents. Edit a document and save it; if someone changed it while you were editing, reload the latest version before deciding how to reapply your draft. Assign relevant documents to the crew members or focus areas that need them."),
            Tip(title: "Run Quality checks",
                detail: "Open Quality checks, choose the output to improve, describe the criteria and select the participating agents. Set model and thinking choices where offered, then start the review. Inspect each round and the resulting output before accepting it; use the run controls to stop or continue a review."),
            Tip(title: "Manage Team routines",
                detail: "Open Team routines to inspect the shared company schedule, start a sequence manually or review a problem. Individual crew members also have their own work-cycle settings. Use the member’s controls when you want to change only that member’s schedule.")
        ]),
        Group(header: "VibeDev", icon: "hammer", tips: [
            Tip(title: "Find recent runs",
                detail: "Open VibeDev from the menu for all runs, newest activity first. Each row names the project, repository and work location; Needs you counts belong to that run. Pull down to refresh; load older runs from the server at the end of the list. The header folder opens projects; + adds one. Project details hold settings and crews. Use the header’s More menu to turn the Magician at work Lock Screen and Dynamic Island summary on or off."),
            Tip(title: "Set up a project and start a run",
                detail: "Tap New run for a guided welcome. Choose a starter to prefill a draft; Explore the code selects Discuss. Starters never send automatically. Choose the project and work location above the input. The model chip opens the same engine, model and thinking controls as Chat. Voice mode has the same reply mute, voice settings and Live controls. Use the paperclip or camera for files, photos or a new picture; wait for uploads to finish before sending. Build changes code; Discuss shows read-only guidance; Autopilot runs unattended with an optional budget and, for a new run, a nightly schedule at 02:00 UTC. Enter adds a line; use Done to dismiss the keyboard without sending. Choose Original folder to work on the current branch and uncommitted files, Isolated workspace for a separate copy, or Isolated branch for branch review. Original-folder edits show their exact diff before writing: Allow once, for this session or always for the selected scope. Work location is independent of Super god mode."),
            Tip(title: "Review Turns and Changes",
                detail: "Open a run and switch between Turns and Changes. Turns shows requests, progress and follow-ups; Changes shows the affected files and diffs. When the run needs you, read its proposed edits before approving or rejecting them. Partly done means requirements remain open even though the run ended."),
            Tip(title: "Steer, stop, follow up and finish",
                detail: "While a run is working, use Steer to add direction or Stop to end it. After it ends, type in the bottom composer to continue that run. Follow-ups keep the original run’s work location. Where the run offers Open PR or Merge, review the changes and confirm the action. Discard branch is in the run’s menu when available and asks for confirmation."),
            Tip(title: "Coordinate a VibeDev crew",
                detail: "Open a project’s VibeDev crews or choose New crew to coordinate coding work. Split the goal plans parallel lanes; Race engines tries the goal with multiple attempts. Set the optional crew budget and Review first when offered. Open a crew to inspect its runs, answer requests and follow progress. Work → Crew manages your general AI team.")
        ]),
        Group(header: "Attention", icon: "bell.badge", tips: [
            Tip(title: "Answer questions and approvals",
                detail: "Open Attention or tap a Needs you card. Read the request, then choose an option, type an answer or approve or reject the proposed action. Plan approvals include a review action. Password and verification-code requests have their own protected entry flow; respond in that request rather than posting a secret in chat.")
        ]),
        Group(header: "Today", icon: "sun.max", tips: [
            Tip(title: "See what matters now",
                detail: "Today brings together Needs You, follow-ups, active work, deliveries, changes, resurfaced context and briefings. Tap a card to open its related work. Swipe or long-press cards that offer snooze or dismiss; use Hidden to restore a card you hid."),
            Tip(title: "Read briefings and activity",
                detail: "Open a briefing to view its cards, charts, tables and notes. Expand Activity to search and filter outcomes, learnings, failures and deliveries. A related task or conversation link takes you to the source rather than creating new work."),
            Tip(title: "Create an Apple Reminder",
                detail: "On a Worth a look card offering Create reminder, choose a title and time. Allow Reminders access if asked. The app creates an Apple Reminder and opens Apple Reminders; Create task instead creates work for your assistant.")
        ]),
        Group(header: "Observe", icon: "ear", tips: [
            Tip(title: "Listen to a meeting or the room",
                detail: "Open Observe and choose Listen here. Start capture, then open the transcript to follow the meeting thread, summaries and notes. The microphone hears nearby speech even if a meeting app is muted. Stop when finished; if another call or app interrupts the microphone, check the session and resume when offered."),
            Tip(title: "Join meetings and send a bot",
                detail: "Upcoming lists available calendar meetings. Join or Join (Me) opens a meeting for you; Listen captures your observation; Send bot asks the connected service to send an agent attendee. For a meeting elsewhere, use Join as bot with its supported meeting link and inspect the resulting session."),
            Tip(title: "Browse Published Notes",
                detail: "Find published task pages in Observe. Search, choose the page size and move through the pages, then open a result in Notes. Use the available publish action for a completed-task batch or send a selected page to memory review when you want it considered for later recall."),
            Tip(title: "Broadcast the screen and meeting audio",
                detail: "Choose Share screen, system audio + my mic, prepare the session and start the broadcast with Microphone on. The meeting’s audio, your voice and screen observations join the meeting thread. Protected call audio may be unavailable. Stop the broadcast using the system recording control when finished.")
        ]),
        Group(header: "Notes and files", icon: "doc.text", tips: [
            Tip(title: "Read and edit Notes",
                detail: "Open Menu → Notes. Use the folders panel to choose a note, or search to find it. Open the editor to make changes and save them; use the note and folder actions to create, rename or delete where offered. Notes are stored on the connected service, so check the connection if they do not load."),
            Tip(title: "Preview, share and publish results",
                detail: "Tap a delivered file to open its preview: documents, web pages, images, audio and video use the available viewer. Use Share file to keep a copy. A completed task’s Publish to Notes action creates or updates its published page. External links can be opened in the browser.")
        ]),
        Group(header: "Thinking Map", icon: "point.3.connected.trianglepath.dotted", tips: [
            Tip(title: "Capture and branch an idea",
                detail: "Open Thinking Map from the menu, choose Brainstorm an idea in Observe, or use @brainstorm in Chat. Speak or type a thought. Select an existing thought before adding another to branch from it, and use Break open to explore different directions."),
            Tip(title: "Organize and harvest a map",
                detail: "Use Ideas to resume saved maps. Canvas, Focus and Outline offer different views of the same ideas. Edit or connect thoughts, use Undo when needed, and Harvest to copy or share a concise result. When offered, turn a thought into a task or send it for memory review.")
        ]),
        Group(header: "Tutor and sharing", icon: "graduationcap", tips: [
            Tip(title: "Ask Tutor for an explanation",
                detail: "Mention @tutor with a concept to open a narrated blackboard lesson. Attach an image when you want an explanation of that image. In voice, begin with Tutor or Tutor Quick for a full or shorter lesson. Use Replay, Keep Showing or Ask Again on a completed guide."),
            Tip(title: "Share text, files or a screenshot",
                detail: "From another app, choose Share → Magican. A single screenshot can open Tutor; other text, links, images and files can go into Chat. Use Magican Assist in the share actions when you want the available text or webpage actions. For another app’s screen, take a screenshot and share it; a voice request alone cannot capture it.")
        ]),
        Group(header: "Siri and Shortcuts", icon: "mic", tips: [
            Tip(title: "Ask your primary assistant with Siri",
                detail: "Settings → Voice & audio → Talk to Magican shows the Siri phrases advertised for your primary assistant and its aliases. Use the displayed Ask … using Magican phrase, then speak your request. New chat in Magican opens a fresh conversation, and Ask Tutor on Magican starts a concept explanation."),
            Tip(title: "Use Shortcuts, the Action Button and Control Center",
                detail: "In Apple Shortcuts, add Talk to Magican, New Chat, Ask Assistant or Ask Tutor, then assign the shortcut to an Action Button if your iPhone has one. Add Magican’s Talk control in Control Center for quick access. Unlock before a Tutor request; if a locked request is refused, repeat it after unlocking.")
        ]),
        Group(header: "Magican Keyboard", icon: "keyboard", tips: [
            Tip(title: "Enable the keyboard and try its tutorial",
                detail: "Open Settings → Keyboard → Magican Keyboard for live setup status and the interactive tutorial. In iOS Settings → General → Keyboard → Keyboards, add Magican and enable Full Access for network-backed skills. Switch keyboards with the globe key. Ordinary typing works without Full Access."),
            Tip(title: "Rewrite, Ask or Act from a text field",
                detail: "Tap ✦ in the suggestion strip, or hold the spacebar still to open actions. Rewrite previews a replacement and offers Undo; Ask streams an answer you can Insert; an Act skill shows a confirmation before creating work. Password, OTP and number fields do not supply text to these actions."),
            Tip(title: "Customize skills, language and learned words",
                detail: "In Settings → Keyboard → Magican Keyboard, choose the keyboard language and add, edit, reorder or delete skill chips. Each skill uses Write, Ask or Act. Changes appear on the next keyboard launch. Reset learned words in Settings forgets the typing vocabulary after confirmation; Reset to default skills resets the skill list.")
        ]),
        Group(header: "Apps", icon: "square.grid.2x2", tips: [
            Tip(title: "Open and use an installed app",
                detail: "Open Menu → Apps and choose an installed app. Use its own controls and configuration screen where provided. Its actions can launch related work; open the activity or task link to inspect those runs. The app catalogue depends on what is installed on your selected computer.")
        ]),
        Group(header: "Widgets and notifications", icon: "rectangle.3.group", tips: [
            Tip(title: "Add the at-a-glance widget and follow work",
                detail: "Long-press the Home Screen, open the widget gallery and add Magican at a glance. It prioritizes requests needing you, then active work, then Talk. Live Activities show supported task progress; use Follow on a VibeDev run or crew when offered. iOS Settings controls whether Live Activities are allowed."),
            Tip(title: "Allow alerts and choose lock-screen question visibility",
                detail: "Open Settings → Notifications to allow notifications or open iOS Settings after a denial. Refresh registration reconnects push alerts when supported by the build. Show questions on lock screen controls question previews; answering still requires unlocking. Listen and Talk activities stay local to this iPhone.")
        ]),
        Group(header: "Settings", icon: "gearshape", tips: [
            Tip(title: "Find a setting and open shared settings",
                detail: "Settings keeps only this iPhone’s own settings. Type in its search field to filter the rows; a setting that is not on the phone leads to Magican settings. Magican settings opens the paired machine’s shared Settings inside the app: budgets, preferences, speaking voices, harness credentials, storage, workspaces, models and more. Sign in once with your web account there and it stays signed in. Links to other pages open in Safari."),
            Tip(title: "Choose themes and appearance",
                detail: "Open Themes from the menu to preview and choose a theme. Use the appearance controls for System, Day or Night where offered. The chosen theme supplies the colours and type used throughout the app; the device’s text-size settings can also affect how much fits on screen."),
            Tip(title: "Set your preferences and review check notes",
                detail: "Open Settings → Magican settings → Preferences to tell the assistant what matters to you and how firmly actions should be checked. Review notes from recent checks where offered, and save a note as a preference when you want it treated as something you explicitly requested."),
            Tip(title: "Choose whose account coding engines use",
                detail: "Open Settings → Magican settings → Harness credentials. Choose a default or per-engine override: Own sign-in first, Own sign-in only or Magician’s keys. Own sign-in first uses the engine’s account when available and provider keys otherwise. Each engine’s status and picker label show which credentials it will use."),
            Tip(title: "Check services, maintenance and versions",
                detail: "Settings → Connection shows service health and offers a refresh. Storage maintenance is in Settings → Magican settings. Help & about shows app and service versions. If a page fails to load, check the selected machine and route first, then refresh service status.")
        ]),
    ]

    private var groups: [Group] {
        let terms = search.split(whereSeparator: \.isWhitespace).map(String.init)
        return Self.topics.compactMap { group in
            guard section.isEmpty || section == group.header else { return nil }
            let tips = group.tips.filter { tip in
                let text = group.header + " " + tip.title + " " + tip.detail
                return terms.allSatisfy { text.localizedCaseInsensitiveContains($0) }
            }
            return tips.isEmpty ? nil : Group(header: group.header, icon: group.icon, tips: tips)
        }
    }

    var body: some View {
        List {
            Section {
                Text("Find a screen or control, then tap a topic for its steps.")
                    .font(.themed(13)).foregroundColor(theme.secondaryTextColor)
                Picker("Section", selection: $section) {
                    Text("All sections").tag("")
                    ForEach(Self.topics) { Text($0.header).tag($0.header) }
                }
                .tint(theme.accentColor)
            }
            .listRowBackground(theme.surfaceColor)

            ForEach(groups) { group in
                Section(header: Text(group.header).foregroundColor(theme.secondaryTextColor)) {
                    ForEach(group.tips) { tip in
                        if search.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                            DisclosureGroup {
                                instructions(tip.detail)
                            } label: {
                                topicLabel(tip, icon: group.icon)
                            }
                            .tint(theme.accentColor)
                        } else {
                            VStack(alignment: .leading, spacing: 8) {
                                topicLabel(tip, icon: group.icon)
                                instructions(tip.detail)
                            }
                            .padding(.vertical, 4)
                        }
                    }
                }
                .listRowBackground(theme.surfaceColor)
            }
            if groups.isEmpty {
                Section {
                    Text("No matching topics. Try another word or choose All sections.")
                        .foregroundColor(theme.secondaryTextColor)
                    Button("Show all topics") { search = ""; section = "" }
                        .tint(theme.accentColor)
                }
                .listRowBackground(theme.surfaceColor)
            }
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .background(theme.backgroundColor.ignoresSafeArea())
        .searchable(text: $search, prompt: "Search screens and controls")
        .navigationTitle("How to Use")
        .navigationBarTitleDisplayMode(.inline)
    }

    private func topicLabel(_ tip: Tip, icon: String) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: icon).font(.system(size: 16))
                .foregroundColor(theme.accentColor).frame(width: 24)
            Text(tip.title).font(.themed(14, weight: .semibold))
                .foregroundColor(theme.textColor).fixedSize(horizontal: false, vertical: true)
        }
        .padding(.vertical, 3)
    }

    private func instructions(_ text: String) -> some View {
        Text(text).font(.themed(13)).foregroundColor(theme.secondaryTextColor)
            .frame(maxWidth: .infinity, alignment: .leading)
            .fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
            .padding(.vertical, 5)
    }
}
