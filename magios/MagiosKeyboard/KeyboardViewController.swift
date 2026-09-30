import SwiftUI
import UIKit

/// Host `UIInputViewController` for the Magican keyboard. Owns the text-document
/// proxy bridge and hosts the SwiftUI keyboard. Typing works fully without Full
/// Access; the agentic surfaces gate on `hasFullAccess`.
final class KeyboardViewController: UIInputViewController {
    private let model = KeyboardModel()
    private var heightConstraint: NSLayoutConstraint?
    private var host: UIHostingController<KeyboardRootView>?

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .clear
        primaryLanguage = KeyboardLanguageStore.current.primaryLanguage   // chosen in the app (default en-IN)
        // Adopt the chosen theme's day/night variant per the SYSTEM appearance.
        // A keyboard extension's `traitCollection` doesn't reliably report dark mode
        // this early (in the window it becomes correct, and `traitCollectionDidChange`
        // fires), so seed the FIRST paint from the last appearance we actually observed
        // in-window; only fall back to the current trait on the very first run. This
        // stops the visible light→dark flash on open.
        let seedDark = KeyboardThemeStore.lastKnownDark ?? (traitCollection.userInterfaceStyle == .dark)
        KeyboardTheme.refreshPalette(dark: seedDark)

        model.proxy = textDocumentProxy
        model.needsGlobe = needsInputModeSwitchKey
        model.hasFullAccess = hasFullAccess
        model.isSensitive = isSensitiveField()
        KeyboardHaptics.enabled = hasFullAccess
        KeyboardHaptics.prepare()
        KeyboardInstall.record(hasFullAccess: hasFullAccess)
        model.configure(for: textDocumentProxy.keyboardType ?? .default)
        model.advanceInputMode = { [weak self] in self?.advanceToNextInputMode() }
        model.dismissKeyboard = { [weak self] in self?.dismissKeyboard() }
        model.refresh()

        registerForTraitChanges([UITraitUserInterfaceStyle.self]) {
            (controller: KeyboardViewController, _: UITraitCollection) in
            controller.applyThemeAppearance()
        }

        // Local completion source (user shortcuts + contact names) — no network.
        requestSupplementaryLexicon { [weak self] lexicon in
            self?.model.lexicon = lexicon
        }

        let host = UIHostingController(rootView: KeyboardRootView(model: model, onHeightChange: { [weak self] height in
            self?.applyContentHeight(height)
        }))
        host.view.backgroundColor = .clear
        self.host = host
        addChild(host)
        host.view.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(host.view)
        NSLayoutConstraint.activate([
            host.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            host.view.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            host.view.topAnchor.constraint(equalTo: view.topAnchor),
            host.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
        host.didMove(toParent: self)
        applyThemeAppearance()

        // Custom keyboards must declare their own height. Seed it with an estimate;
        // the SwiftUI content then reports its *real* laid-out height via
        // `applyContentHeight`, so the input view always matches the content exactly
        // (nothing clipped, no empty band) whatever surfaces are open.
        let h = view.heightAnchor.constraint(equalToConstant: initialHeightEstimate())
        h.priority = .required - 1
        h.isActive = true
        heightConstraint = h
    }

    /// Size the input view to the measured SwiftUI content height (the single source
    /// of truth for the keyboard's height).
    private func applyContentHeight(_ height: CGFloat) {
        guard height > 120 else { return }   // ignore transient zero/partial layouts
        guard abs((heightConstraint?.constant ?? 0) - height) > 0.5 else { return }
        heightConstraint?.constant = height
    }

    override func textDidChange(_ textInput: UITextInput?) {
        super.textDidChange(textInput)
        model.proxy = textDocumentProxy
        model.hasFullAccess = hasFullAccess
        model.isSensitive = isSensitiveField()
        KeyboardHaptics.enabled = hasFullAccess
        KeyboardHaptics.prepare()
        KeyboardInstall.record(hasFullAccess: hasFullAccess)
        model.updateContentMode(for: textDocumentProxy.keyboardType ?? .default)
        applyThemeAppearance()
        model.refresh()
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        // On a keyboard switch iOS often REUSES this extension (viewDidLoad won't
        // re-run), and the system keyboard's top strip reappears — so re-theme +
        // repaint the container on every (re)appearance.
        applyThemeAppearance()
        paintContainerBackground()
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        paintContainerBackground()
        // The system can re-set the container background just after a switch; repaint
        // once more on the next runloop turn to cover that.
        DispatchQueue.main.async { [weak self] in self?.paintContainerBackground() }
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        paintContainerBackground()
    }

    /// Paint the system's keyboard container views that sit *behind* our view (the
    /// `UIInputView` wrapper + its host container) with the theme background, so the
    /// bare system grey no longer shows in the top/bottom safe-area slivers around
    /// the keyboard. Bounded to a few ancestors so it stays within the keyboard region.
    private func paintContainerBackground() {
        let bg = UIColor(KeyboardTheme.palette.background)
        var node: UIView? = view
        var depth = 0
        while let current = node, depth < 4 {
            current.backgroundColor = bg
            node = current.superview
            depth += 1
        }
    }

    /// Adopt the Magican app's active theme. The keys/pills/panels use the theme's
    /// explicit colors (`KeyboardTheme` ← `KeyboardPalette`); we also force the
    /// interface style to the theme's light/dark so system bits (the text caret,
    /// selection) stay coherent with the theme.
    private func applyThemeAppearance() {
        // Follow the SYSTEM appearance (iOS day/night): pick the matching variant of
        // the chosen theme family, regardless of the app's own light/dark mode.
        let dark = traitCollection.userInterfaceStyle == .dark
        KeyboardTheme.refreshPalette(dark: dark)
        // Persist the appearance ONLY once the view is in the window, where the trait
        // is trustworthy — never the possibly-wrong early `viewDidLoad` value — so the
        // next cold start seeds its first paint correctly (see `viewDidLoad`).
        if view.window != nil { KeyboardThemeStore.lastKnownDark = dark }
        // Paint the input view + host with the theme background so no bare system
        // keyboard grey shows through the safe-area slices at the top/bottom.
        let bg = UIColor(KeyboardTheme.palette.background)
        view.backgroundColor = bg
        host?.view.backgroundColor = bg
        // Don't force the interface style — let the caret/selection follow the system
        // too; the palette (explicit colors) already matches.
        if host?.overrideUserInterfaceStyle != .unspecified {
            host?.overrideUserInterfaceStyle = .unspecified
            host?.view.overrideUserInterfaceStyle = .unspecified
        }
        model.bumpAppearance()   // re-render SwiftUI with the freshly-picked variant
    }

    /// Secure / OTP / numeric fields: suppress all agentic surfaces and context
    /// capture (passwords, one-time codes, PINs, phone numbers).
    private func isSensitiveField() -> Bool {
        let proxy = textDocumentProxy
        switch proxy.keyboardType {
        case .numberPad, .phonePad, .asciiCapableNumberPad, .decimalPad:
            return true
        default:
            break
        }
        if let contentType = proxy.textContentType,
           [.password, .newPassword, .oneTimeCode].contains(contentType) {
            return true
        }
        return proxy.isSecureTextEntry ?? false
    }

    /// First-frame height only (the resting keyboard: strip + 4 rows). The SwiftUI
    /// content immediately reports its real height via `applyContentHeight`, which
    /// then owns the sizing for every state (aux row, coach, panels).
    private func initialHeightEstimate() -> CGFloat {
        let strip: CGFloat = 34
        let gaps = KeyboardTheme.rowSpacing * 4   // strip + 4 rows → 4 gaps
        let verticalPadding: CGFloat = 6 + 4
        return verticalPadding + strip + KeyboardTheme.rowHeight * 4 + gaps
    }
}
