import SwiftUI

/// The emoji layer: a scrollable, categorised emoji grid with a bottom bar
/// (ABC · category jumps · delete). Reached from the 🙂 key on the bottom row.
struct EmojiKeyboardView: View {
    @ObservedObject var model: KeyboardModel

    private let columns = Array(repeating: GridItem(.flexible(), spacing: 2), count: 8)

    var body: some View {
        ScrollViewReader { scroll in
            VStack(spacing: 4) {
                ScrollView(.vertical, showsIndicators: false) {
                    LazyVGrid(columns: columns, spacing: 2) {
                        ForEach(KeyboardEmoji.categories) { category in
                            Section {
                                ForEach(Array(category.emojis.enumerated()), id: \.offset) { _, emoji in
                                    Button {
                                        KeyboardHaptics.keyTap()
                                        model.insertEmoji(emoji)
                                    } label: {
                                        Text(emoji)
                                            .font(.system(size: 28))
                                            .frame(maxWidth: .infinity, minHeight: 40)
                                            .contentShape(Rectangle())
                                    }
                                    .buttonStyle(.plain)
                                }
                            } header: {
                                HStack {
                                    Text(category.id.capitalized)
                                        .font(.system(size: 11, weight: .semibold))
                                        .foregroundColor(KeyboardTheme.keyText.opacity(0.4))
                                    Spacer()
                                }
                                .padding(.top, 4)
                                .id(category.id)
                            }
                        }
                    }
                    .padding(.horizontal, 4)
                }
                bottomBar(scroll: scroll)
            }
        }
        .frame(height: 4 * KeyboardTheme.rowHeight + 3 * KeyboardTheme.rowSpacing)
    }

    private func bottomBar(scroll: ScrollViewProxy) -> some View {
        HStack(spacing: KeyboardTheme.keySpacing) {
            KeyView(key: KeyCap(id: "emoji-abc", kind: .layer(.letters)), model: model)
                .frame(width: 58, height: KeyboardTheme.rowHeight)
            if model.needsGlobe {
                KeyView(key: KeyCap(id: "emoji-globe", kind: .globe), model: model)
                    .frame(width: 44, height: KeyboardTheme.rowHeight)
            }
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 16) {
                    ForEach(KeyboardEmoji.categories) { category in
                        Button {
                            KeyboardHaptics.special()
                            withAnimation { scroll.scrollTo(category.id, anchor: .top) }
                        } label: {
                            Image(systemName: category.symbol)
                                .font(.system(size: 16))
                                .foregroundColor(KeyboardTheme.keyText.opacity(0.6))
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 8)
            }
            .frame(maxWidth: .infinity)
            KeyView(key: KeyCap(id: "emoji-del", kind: .delete), model: model)
                .frame(width: 48, height: KeyboardTheme.rowHeight)
        }
        .frame(height: KeyboardTheme.rowHeight)
    }
}
