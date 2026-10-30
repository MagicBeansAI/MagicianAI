import SwiftUI

/// One changed file's diff, with previous/next and a file list.
struct VibeDevDiffView: View {
    @StateObject private var model: VibeDevDiffModel
    @ObservedObject private var theme = ThemeManager.shared
    @AppStorage("vibedev.diff.wrap") private var wrap = true
    @State private var showFiles = false

    init(target: VibeDevDiffTarget, context: VibeDevContext) {
        _model = StateObject(wrappedValue: VibeDevDiffModel(target: target, context: context))
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                if let file = model.file {
                    Text(file.path)
                        .font(.themedMono(12))
                        .foregroundStyle(theme.secondaryTextColor)
                        .textSelection(.enabled)
                    HStack(spacing: 8) {
                        Text("+\(file.additions)").foregroundStyle(theme.successColor)
                        Text("−\(file.deletions)").foregroundStyle(theme.dangerColor)
                        Text("File \(model.index + 1) of \(model.files.count)")
                            .foregroundStyle(theme.secondaryTextColor)
                    }
                    .font(.themed(12, weight: .semibold))
                }
                if let patch = model.patch, patch.truncated {
                    Label("This diff was cut short. Open the run on the desktop for the rest.",
                          systemImage: "scissors")
                        .font(.themed(12))
                        .foregroundStyle(theme.warningColor)
                }
                if let diff = model.diff {
                    UnifiedDiffView(diff: diff, wrap: wrap, startCollapsed: model.startCollapsed)
                        .id(model.file?.path)
                } else if let error = model.errorMessage {
                    VibeDevEmpty(text: error)
                } else if model.loading {
                    ProgressView().tint(theme.accentColor).frame(maxWidth: .infinity).padding(.top, 30)
                }
            }
            .padding(.horizontal, 16)
            .padding(.top, 2)
            .padding(.bottom, 24)
        }
        .background(theme.backgroundColor.ignoresSafeArea())
        .navigationTitle(model.file?.fileName ?? "Diff")
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(true)
        .toolbarBackground(theme.backgroundColor, for: .navigationBar)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) { VibeDevBackButton() }
                .roundedSquareToolbarBackground()
            ToolbarItemGroup(placement: .topBarTrailing) {
                Button(wrap ? "Wrap" : "No wrap") { wrap.toggle() }
                    .font(.themed(12, weight: .bold))
                    .buttonStyle(VibeDevButtonStyle())
                    .accessibilityLabel(wrap ? "Lines wrap; turn wrapping off" : "Lines scroll; turn wrapping on")
                Button { showFiles = true } label: { Image(systemName: "list.bullet") }
                    .buttonStyle(VibeDevButtonStyle())
                    .accessibilityLabel("Files")
                    .disabled(model.files.count < 2)
            }
            .roundedSquareToolbarBackground()
        }
        .safeAreaInset(edge: .bottom) {
            if model.files.count > 1 {
                VibeDevBottomBar {
                    Button { model.move(by: -1) } label: { Text("‹ Previous file") }
                        .buttonStyle(VibeDevButtonStyle(style: .plain, expand: true, large: true))
                        .disabled(!model.canGoBack)
                    Button { model.move(by: 1) } label: { Text("Next file ›") }
                        .buttonStyle(VibeDevButtonStyle(style: .plain, expand: true, large: true))
                        .disabled(!model.canGoForward)
                }
            }
        }
        .sheet(isPresented: $showFiles) {
            fileList
                .presentationDetents([.medium, .large])
                .presentationBackground(theme.cardColor)
        }
        .task { await model.load() }
    }

    private var fileList: some View {
        NavigationStack {
            List {
                ForEach(Array(model.files.enumerated()), id: \.element.path) { index, file in
                    Button {
                        model.select(index)
                        showFiles = false
                    } label: {
                        HStack(spacing: 8) {
                            Image(systemName: index == model.index ? "checkmark" : "doc.text")
                                .foregroundStyle(index == model.index ? theme.accentColor : theme.secondaryTextColor)
                                .frame(width: 18)
                            Text(file.path)
                                .font(.themedMono(12.5))
                                .foregroundStyle(theme.textColor)
                                .vibeDevLineLimit(1)
                                .truncationMode(.head)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            Text("+\(file.additions)").foregroundStyle(theme.successColor)
                            Text("−\(file.deletions)").foregroundStyle(theme.dangerColor)
                        }
                        .font(.themedMono(12, weight: .semibold))
                    }
                    .listRowBackground(theme.cardColor)
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .background(theme.cardColor)
            .navigationTitle("Files")
            .navigationBarTitleDisplayMode(.inline)
        }
    }
}
