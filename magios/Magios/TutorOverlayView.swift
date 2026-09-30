import SwiftUI
import UIKit

struct TutorOverlayView: View {
    /// nil in blackboard mode (source-free) — the canvas is a plain dark board.
    let screenshot: UIImage?
    let autoStart: Bool
    let voiceAdmissionID: UUID?
    @StateObject private var viewModel: TutorOverlayViewModel
    @State private var question: String
    @State private var didAutoStart = false
    @Environment(\.dismiss) private var dismiss
    @StateObject private var theme = ThemeManager.shared

    init(screenshot: UIImage? = nil, canvasMode: TutorCanvasMode = .screenOverlay,
         initialQuestion: String = "", autoStart: Bool = false,
         voiceAdmissionID: UUID? = nil) {
        self.screenshot = screenshot
        self.autoStart = autoStart
        self.voiceAdmissionID = voiceAdmissionID
        _question = State(initialValue: initialQuestion)
        _viewModel = StateObject(wrappedValue:
            TutorOverlayViewModel(screenshot: screenshot, canvasMode: canvasMode))
    }

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            GeometryReader { proxy in
                // screen_overlay projects into the fitted image rect using the image's
                // pixel space; blackboard has no image — the whole canvas is the board
                // and shapes use the 2048-clamped model space (web parity).
                let source = screenshot.map { screenshotPixelSize(of: $0) }
                    ?? TutorBlackboardCanvas.modelSize(viewport: proxy.size)
                let fitted = screenshot.map { tutorFittedRect(imageSize: $0.size, in: proxy.size) }
                    ?? CGRect(origin: .zero, size: proxy.size)
                if let screenshot {
                    Image(uiImage: screenshot)
                        .resizable()
                        .scaledToFit()
                        .frame(width: proxy.size.width, height: proxy.size.height)
                }

                TimelineView(.animation) { timeline in
                    Canvas { context, _ in
                        let labelOffsets = TutorLabelLayout.offsets(
                            for: viewModel.visibleShapes.map { ($0.id, $0.shape) },
                            fittedRect: fitted, fallbackSpace: source
                        )
                        // Fills, then strokes, then text — a STABLE sort, so
                        // reveal order survives inside each layer and only the
                        // previously arbitrary part changes. Without it an
                        // opaque background emitted after a label painted over
                        // it, since arrival order was the only order.
                        let painted = viewModel.visibleShapes
                            .enumerated()
                            .sorted { lhs, rhs in
                                let left = TutorShapeRenderer.paintLayer(lhs.element.shape)
                                let right = TutorShapeRenderer.paintLayer(rhs.element.shape)
                                return left == right ? lhs.offset < rhs.offset : left < right
                            }
                            .map(\.element)
                        for visible in painted {
                            let elapsed = timeline.date.timeIntervalSince(visible.appearedAt)
                            let progress = visible.duration <= 0 ? 1 : min(1, elapsed / visible.duration)
                            TutorShapeRenderer.render(
                                visible.shape,
                                in: &context,
                                fittedRect: fitted,
                                screenshotSize: source,
                                progress: progress,
                                labelYOffset: labelOffsets[visible.id] ?? 0
                            )
                        }
                    }
                }
            }
            .ignoresSafeArea()

            VStack(spacing: 12) {
                topBar
                Spacer()
                if viewModel.stepLabel != nil || viewModel.stepNarration != nil {
                    stepBubble
                } else if let caption = viewModel.caption, !caption.isEmpty {
                    captionStrip(caption)
                }
                if let error = viewModel.errorMessage { errorStrip(error) }
                controls
            }
            .padding()
        }
        .preferredColorScheme(.dark)
        .task {
            guard autoStart, !didAutoStart else { return }
            didAutoStart = true
            switch TutorOverlayRouter.shared.consumeVoiceAdmission(voiceAdmissionID) {
            case .admitted:
                break
            case .locked:
                SpeechSynthesizer.shared.speak(
                    DeviceScreenLock.message(for: .tutor),
                    messageId: "guided-flow-screen-gate"
                )
                dismiss()
                return
            case .invalidated:
                // The lock notification already spoke and removed the request.
                // Never turn unlock into an implicit retry.
                dismiss()
                return
            }
            let initialQuestion = question.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !initialQuestion.isEmpty else { return }
            viewModel.start(question: initialQuestion)
        }
        .onChange(of: viewModel.dismissalRequested) { _, requested in
            if requested {
                viewModel.dismissTutor()
                dismiss()
            }
        }
        .onDisappear { viewModel.dismissTutor() }
    }

    private var topBar: some View {
        HStack {
            Label("Tutor", systemImage: "graduationcap.fill")
                .font(.system(size: 17, weight: .bold))
            Spacer()
            Button {
                viewModel.dismissTutor(); dismiss()
            } label: {
                Image(systemName: "xmark").font(.system(size: 15, weight: .bold))
                    .frame(width: 36, height: 36).background(.black.opacity(0.7)).clipShape(Circle())
            }
        }
        .foregroundColor(.white)
    }

    @ViewBuilder
    private var controls: some View {
        if viewModel.phase == .ready || viewModel.phase == .failed {
            VStack(spacing: 10) {
                TextField("What do you want help with?", text: $question, axis: .vertical)
                    .lineLimit(1...3).padding(12)
                    .background(.black.opacity(0.78)).foregroundColor(.white)
                    .clipShape(RoundedRectangle(cornerRadius: 14))
                Button {
                    viewModel.start(question: question)
                } label: {
                    Label(viewModel.phase == .failed ? "Try Again" : "Ask Tutor", systemImage: "wand.and.stars")
                        .font(.system(size: 16, weight: .semibold))
                        .frame(maxWidth: .infinity).padding(.vertical, 12)
                        .background(theme.accentColor).foregroundColor(theme.onAccentColor)
                        .clipShape(RoundedRectangle(cornerRadius: 14))
                }
            }
        } else if viewModel.phase == .completed || viewModel.canExplainDeeper {
            VStack(spacing: 10) {
                HStack(spacing: 12) {
                    Button { viewModel.replay() } label: {
                        Label("Replay", systemImage: "arrow.counterclockwise")
                    }
                    .disabled(!viewModel.canReplay)
                    // Available at a settled live-step boundary and after the
                    // guide completes. Never interrupts drawing or narration.
                    Button { viewModel.explainDeeper() } label: {
                        Label("Explain deeper", systemImage: "arrow.down.circle")
                    }
                    .disabled(!viewModel.canExplainDeeper)
                    Button { viewModel.keepShowing() } label: {
                        Label(
                            viewModel.isKeptShowing ? "Kept" : "Keep Showing",
                            systemImage: viewModel.isKeptShowing ? "pin.fill" : "pin"
                        )
                    }
                    .disabled(viewModel.isKeptShowing)
                }
                HStack(spacing: 12) {
                    Button { viewModel.askAgain() } label: {
                        Label("Ask Again", systemImage: "bubble.left.and.bubble.right")
                    }
                    Button {
                        viewModel.dismissTutor(); dismiss()
                    } label: {
                        Label("Done", systemImage: "checkmark")
                    }
                }
            }
            .font(.system(size: 15, weight: .semibold)).buttonStyle(.borderedProminent)
            .tint(theme.accentColor)
            .foregroundColor(theme.onAccentColor)
        } else {
            ProgressView().tint(.white).padding(12).background(.black.opacity(0.72)).clipShape(Capsule())
        }
    }

    /// Structured step bubble (web parity): a "Tutor" kicker + step title +
    /// narration, with a speaking indicator while the current status is active.
    private var stepBubble: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Image(systemName: "waveform").font(.system(size: 10, weight: .bold))
                Text("TUTOR").font(.system(size: 10, weight: .heavy)).tracking(1.5)
                if let caption = viewModel.caption, !caption.isEmpty {
                    Text("· \(caption)").font(.system(size: 10, weight: .semibold)).lineLimit(1)
                }
            }
            .foregroundColor(theme.accentColor)
            if let label = viewModel.stepLabel, !label.isEmpty {
                Text(label).font(.system(size: 17, weight: .bold)).foregroundColor(.white)
            }
            if let narration = viewModel.stepNarration, !narration.isEmpty {
                Text(narration).font(.system(size: 14)).foregroundColor(.white.opacity(0.85))
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 16).padding(.vertical, 12)
        .background(.black.opacity(0.82)).clipShape(RoundedRectangle(cornerRadius: 14))
    }

    private func captionStrip(_ text: String) -> some View {
        Text(text).font(.system(size: 16, weight: .semibold))
            .foregroundColor(.white).multilineTextAlignment(.center)
            .padding(.horizontal, 16).padding(.vertical, 10)
            .background(.black.opacity(0.78)).clipShape(RoundedRectangle(cornerRadius: 12))
    }

    private func errorStrip(_ text: String) -> some View {
        Text(text).font(.system(size: 14, weight: .medium))
            .foregroundColor(theme.contrastingTextColor(for: theme.dangerColor))
            .padding(12).frame(maxWidth: .infinity)
            .background(theme.dangerColor.opacity(0.92)).clipShape(RoundedRectangle(cornerRadius: 12))
    }

    private func screenshotPixelSize(of image: UIImage) -> CGSize {
        CGSize(
            width: CGFloat(image.cgImage?.width ?? Int(image.size.width * image.scale)),
            height: CGFloat(image.cgImage?.height ?? Int(image.size.height * image.scale))
        )
    }
}

/// Blackboard coordinate space — a port of the web `refreshCoordinateSpace`
/// (`draw-overlay/+page.svelte`). Source-free tutor shapes are authored in a model
/// space whose largest side is clamped to 2048; mirroring that keeps blackboard
/// shapes projecting 1:1 onto the overlay canvas.
enum TutorBlackboardCanvas {
    static let maxSide: CGFloat = 2048

    static func modelSize(viewport: CGSize, maxSide: CGFloat = maxSide) -> CGSize {
        let w = max(1, viewport.width)
        let h = max(1, viewport.height)
        let largest = max(w, h)
        let scale = largest > maxSide ? maxSide / largest : 1
        return CGSize(width: w * scale, height: h * scale)
    }
}
