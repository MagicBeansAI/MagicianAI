package ai.magicbeans.magdroid.ui

import ai.magicbeans.magdroid.identity.ProductIdentity

/**
 * The searchable index of Android Settings.
 *
 * Settings on the phone keeps only what belongs to this device. Everything
 * shared across devices (budgets, harness credentials, preferences, storage
 * maintenance, speaking voices, trust rules, workspaces, models…) lives in
 * the web Settings, which [DeviceSettingsRow.WebSettings] opens embedded.
 *
 * Each row has a title and keywords. A query matches a row when every word
 * of the query appears in its title, keywords or section title (case
 * insensitive), the rule How to Use already applies to its topics.
 */
internal enum class DeviceSettingsSection(val title: String) {
    VoiceAndAudio("Voice & audio"),
    NotificationsAndPermissions("Notifications & permissions"),
    KeyboardAndPrivacy("Keyboard & privacy"),
    Connection("Connection"),
    HelpAndAbout("Help & about"),
}

internal enum class DeviceSettingsRow(
    /** Null for the top row, which sits above the sections. */
    val section: DeviceSettingsSection?,
    val title: String,
    val keywords: String,
) {
    WebSettings(
        null,
        "${ProductIdentity.productName} settings",
        "web shared account budgets budget spend spending limits cost money harness credentials " +
            "coding engines cli login sign-in preferences memory storage maintenance voices speaking " +
            "voice persona tts trust rules approvals safety workspaces models engines providers " +
            "notes notifications devices mobile pairing agents themes",
    ),
    TalkToAssistant(
        DeviceSettingsSection.VoiceAndAudio,
        "Talk to your assistant",
        "wake word wake phrase listen listening hands-free hotword microphone mic stop listening",
    ),
    WakesOn(DeviceSettingsSection.VoiceAndAudio, "Wakes on", "wake word wake phrase name alias assistant"),
    PhoneVoice(
        DeviceSettingsSection.VoiceAndAudio,
        "This phone · speaking voice",
        "device voice system voice tts text to speech speech",
    ),
    StayAvailable(
        DeviceSettingsSection.VoiceAndAudio,
        "Stay available for",
        "leash availability ambient timeout talk behavior behaviour",
    ),
    CaptureScreen(
        DeviceSettingsSection.VoiceAndAudio,
        "Also capture the screen",
        "screen capture keyframes meeting observe talk behavior behaviour",
    ),
    ConversationMode(
        DeviceSettingsSection.VoiceAndAudio,
        "Conversation mode",
        "ambient voice mode talk behavior behaviour",
    ),
    SpeakReplies(
        DeviceSettingsSection.VoiceAndAudio,
        "Speak replies",
        "read aloud spoken replies tts voice talk behavior behaviour",
    ),
    Notifications(
        DeviceSettingsSection.NotificationsAndPermissions,
        "Notifications",
        "alerts push permission permissions allow notification settings",
    ),
    RefreshRegistration(
        DeviceSettingsSection.NotificationsAndPermissions,
        "Refresh registration",
        "push fcm firebase alerts notifications route",
    ),
    LockScreenQuestions(
        DeviceSettingsSection.NotificationsAndPermissions,
        "Show questions on lock screen",
        "lock screen privacy questions answers notifications",
    ),
    HomeScreenWidget(
        DeviceSettingsSection.NotificationsAndPermissions,
        "Home Screen widget",
        "widget at a glance launcher today",
    ),
    RemoteDelivery(
        DeviceSettingsSection.NotificationsAndPermissions,
        "Remote delivery",
        "push firebase fcm background updates at a glance widget",
    ),
    Keyboard(
        DeviceSettingsSection.KeyboardAndPrivacy,
        "${ProductIdentity.productName} Keyboard",
        "keyboard ime typing input skills write ask act language",
    ),
    ResetLearnedWords(
        DeviceSettingsSection.KeyboardAndPrivacy,
        "Reset learned words",
        "keyboard vocabulary dictionary forget learned words",
    ),
    ProtectedApps(
        DeviceSettingsSection.KeyboardAndPrivacy,
        "Protected apps",
        "privacy app pilot exclude sensitive apps observation control",
    ),
    MagicianConnection(
        DeviceSettingsSection.Connection,
        "Connect to Magician",
        "connection connected pair pairing qr machine machines host server address route wifi wi-fi remote switch",
    ),
    Services(
        DeviceSettingsSection.Connection,
        "Services",
        "service status health backend magician magicutor desktopproxy refresh online offline accounts login sign-in oauth metabase higgsfield swiggy zepto",
    ),
    HowToUse(DeviceSettingsSection.HelpAndAbout, "How to Use", "help guide tips tutorial instructions"),
    About(
        DeviceSettingsSection.HelpAndAbout,
        "About",
        "version versions build app android backend core api magicutor supervisor desktop desktopproxy scope workspace principal",
    ),
}

/** The query's words; blank words are dropped. */
internal fun settingsSearchTerms(query: String): List<String> =
    query.trim().split(Regex("\\s+")).filter(String::isNotEmpty)

/** True for an empty query, else when every word appears in the row's title, keywords or section. */
internal fun DeviceSettingsRow.matches(query: String): Boolean {
    val terms = settingsSearchTerms(query)
    if (terms.isEmpty()) return true
    val haystack = "$title $keywords ${section?.title.orEmpty()}"
    return terms.all { haystack.contains(it, ignoreCase = true) }
}

/**
 * The sections to draw, in order, each with its matching rows. A section with
 * no matching rows is left out. [available] hides rows this phone cannot show
 * right now (Refresh registration before notifications are allowed).
 */
internal fun visibleDeviceSettings(
    query: String,
    available: (DeviceSettingsRow) -> Boolean = { true },
): List<Pair<DeviceSettingsSection, List<DeviceSettingsRow>>> =
    DeviceSettingsSection.entries.mapNotNull { section ->
        DeviceSettingsRow.entries
            .filter { it.section == section && available(it) && it.matches(query) }
            .takeIf { it.isNotEmpty() }
            ?.let { section to it }
    }

/**
 * The section the Settings chip bar marks active while the list scrolls.
 *
 * [headerOffsets] holds each drawn section's top, in display order, in the
 * scrolled content's coordinates. A section is active once its top has
 * scrolled to or above a line [linePx] below the top of the viewport; the
 * last such section wins. Before any has, the first section is active. At the
 * end of a list that scrolls, the last section is active even if its top never
 * reached the line (a short last section cannot be scrolled that far).
 * Returns null when there are no sections.
 */
internal fun <K> activeSettingsSection(
    headerOffsets: List<Pair<K, Int>>,
    scrollY: Int,
    maxScroll: Int,
    linePx: Int,
): K? {
    if (headerOffsets.isEmpty()) return null
    if (maxScroll > 0 && scrollY >= maxScroll) return headerOffsets.last().first
    return headerOffsets.lastOrNull { (_, top) -> top <= scrollY + linePx }?.first
        ?: headerOffsets.first().first
}
