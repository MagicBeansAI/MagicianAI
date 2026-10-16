import Foundation

/// Pure, UI-framework-free keyboard layout + state. Rendering lives in the
/// `MagiosKeyboard` target; this is app-host-testable in `MagiosTests`.

public enum KeyboardLayer: Equatable {
    case letters
    case numbers
    case symbols
    case emoji
}

/// Adapts the letters layer's bottom row to the field's `keyboardType`.
public enum KeyboardContentMode: Equatable {
    case normal
    case email   // adds @ and . beside space
    case url     // adds . and / beside space
}

public enum ShiftState: Equatable {
    case off
    case on
    case capsLock

    public var isUppercased: Bool { self != .off }
}

/// One key. `widthWeight` is relative to a standard character key (1.0).
public struct KeyCap: Identifiable, Equatable {
    public enum Kind: Equatable {
        case character(String)
        case shift
        case delete
        case globe          // input-mode switch
        case space
        case ret            // return / go / send …
        case layer(KeyboardLayer)
        case dismiss        // hide keyboard
    }

    public let id: String
    public let kind: Kind
    public let widthWeight: CGFloat

    public init(id: String, kind: Kind, widthWeight: CGFloat = 1) {
        self.id = id
        self.kind = kind
        self.widthWeight = widthWeight
    }

    /// The characters revealed by a long-press callout (diacritics / alternates).
    public var callouts: [String] {
        guard case .character(let c) = kind else { return [] }
        return KeyboardLayoutModel.callouts[c.lowercased()] ?? []
    }
}

public enum KeyboardLayoutModel {
    /// The rows for a layer, already shift-cased for character keys.
    public static func rows(
        layer: KeyboardLayer,
        shift: ShiftState,
        needsGlobe: Bool,
        contentMode: KeyboardContentMode = .normal
    ) -> [[KeyCap]] {
        switch layer {
        case .letters: return letterRows(shift: shift, needsGlobe: needsGlobe, contentMode: contentMode)
        case .numbers: return numberRows(needsGlobe: needsGlobe)
        case .symbols: return symbolRows(needsGlobe: needsGlobe)
        case .emoji: return []   // the emoji layer is rendered by EmojiKeyboardView, not a key grid
        }
    }

    // MARK: letters

    private static let lettersRow1 = ["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"]
    private static let lettersRow2 = ["a", "s", "d", "f", "g", "h", "j", "k", "l"]
    private static let lettersRow3 = ["z", "x", "c", "v", "b", "n", "m"]

    private static func letterRows(shift: ShiftState, needsGlobe: Bool, contentMode: KeyboardContentMode) -> [[KeyCap]] {
        let cased: (String) -> String = { shift.isUppercased ? $0.uppercased() : $0 }
        let r1 = lettersRow1.map { KeyCap(id: "k-\($0)", kind: .character(cased($0))) }
        let r2 = lettersRow2.map { KeyCap(id: "k-\($0)", kind: .character(cased($0))) }
        var r3: [KeyCap] = [KeyCap(id: "shift", kind: .shift, widthWeight: 1.4)]
        r3 += lettersRow3.map { KeyCap(id: "k-\($0)", kind: .character(cased($0))) }
        r3.append(KeyCap(id: "delete", kind: .delete, widthWeight: 1.4))
        return [r1, r2, r3, bottomRow(layerKey: .layer(.numbers), layerLabel: "123", needsGlobe: needsGlobe, contentMode: contentMode)]
    }

    // MARK: numbers

    private static let numbersRow1 = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"]
    private static let numbersRow2 = ["-", "/", ":", ";", "(", ")", "$", "&", "@", "\""]
    private static let numbersRow3 = [".", ",", "?", "!", "'"]

    private static func numberRows(needsGlobe: Bool) -> [[KeyCap]] {
        let r1 = numbersRow1.map { KeyCap(id: "n-\($0)", kind: .character($0)) }
        let r2 = numbersRow2.map { KeyCap(id: "n2-\($0)", kind: .character($0)) }
        var r3: [KeyCap] = [KeyCap(id: "to-symbols", kind: .layer(.symbols), widthWeight: 1.4)]
        r3 += numbersRow3.map { KeyCap(id: "n3-\($0)", kind: .character($0)) }
        r3.append(KeyCap(id: "delete", kind: .delete, widthWeight: 1.4))
        return [r1, r2, r3, bottomRow(layerKey: .layer(.letters), layerLabel: "ABC", needsGlobe: needsGlobe)]
    }

    // MARK: symbols

    private static let symbolsRow1 = ["[", "]", "{", "}", "#", "%", "^", "*", "+", "="]
    private static let symbolsRow2 = ["_", "\\", "|", "~", "<", ">", "€", "£", "¥", "•"]
    private static let symbolsRow3 = [".", ",", "?", "!", "'"]

    private static func symbolRows(needsGlobe: Bool) -> [[KeyCap]] {
        let r1 = symbolsRow1.map { KeyCap(id: "s-\($0)", kind: .character($0)) }
        let r2 = symbolsRow2.map { KeyCap(id: "s2-\($0)", kind: .character($0)) }
        var r3: [KeyCap] = [KeyCap(id: "to-numbers", kind: .layer(.numbers), widthWeight: 1.4)]
        r3 += symbolsRow3.map { KeyCap(id: "s3-\($0)", kind: .character($0)) }
        r3.append(KeyCap(id: "delete", kind: .delete, widthWeight: 1.4))
        return [r1, r2, r3, bottomRow(layerKey: .layer(.letters), layerLabel: "ABC", needsGlobe: needsGlobe)]
    }

    // MARK: bottom row (shared)

    private static func bottomRow(
        layerKey: KeyCap.Kind,
        layerLabel: String,
        needsGlobe: Bool,
        contentMode: KeyboardContentMode = .normal
    ) -> [KeyCap] {
        var row: [KeyCap] = [KeyCap(id: "layer-\(layerLabel)", kind: layerKey, widthWeight: 1.4)]
        // Emoji key, immediately right of the 123 / ABC key (standard iOS placement).
        row.append(KeyCap(id: "emoji", kind: .layer(.emoji), widthWeight: 1.2))
        if needsGlobe {
            row.append(KeyCap(id: "globe", kind: .globe, widthWeight: 1.2))
        }
        // Space soaks up the width freed by dropping the far-right dismiss key
        // (dismiss now happens via tap-anywhere / the keyboard switch), so it's a
        // fat, easy thumb target and `return` sits rightmost.
        var spaceWeight: CGFloat = needsGlobe ? 4.0 : 4.8
        switch contentMode {
        case .email:
            row.append(KeyCap(id: "at", kind: .character("@"), widthWeight: 1.2))
            row.append(KeyCap(id: "dot", kind: .character("."), widthWeight: 1.2))
            spaceWeight -= 2.4
        case .url:
            row.append(KeyCap(id: "dot", kind: .character("."), widthWeight: 1.2))
            row.append(KeyCap(id: "slash", kind: .character("/"), widthWeight: 1.2))
            spaceWeight -= 2.4
        case .normal:
            break
        }
        row.append(KeyCap(id: "space", kind: .space, widthWeight: max(2.0, spaceWeight)))
        // Return is the rightmost key (system-parity thumb reach).
        row.append(KeyCap(id: "return", kind: .ret, widthWeight: 1.6))
        return row
    }

    // MARK: diacritic / alternate callouts

    static let callouts: [String: [String]] = [
        "a": ["à", "á", "â", "ä", "æ", "ã", "å", "ā"],
        "e": ["è", "é", "ê", "ë", "ē", "ė", "ę"],
        "i": ["î", "ï", "í", "ī", "į", "ì"],
        "o": ["ô", "ö", "ò", "ó", "œ", "ø", "ō", "õ"],
        "u": ["û", "ü", "ù", "ú", "ū"],
        "y": ["ÿ"],
        "s": ["ß", "ś", "š"],
        "l": ["ł"],
        "z": ["ž", "ź", "ż"],
        "c": ["ç", "ć", "č"],
        "n": ["ñ", "ń"],
        "'": ["’", "‘", "`"],
        "\"": ["”", "“", "„"],
        "-": ["–", "—", "•"],
        "?": ["¿"],
        "!": ["¡"],
        "$": ["€", "£", "¥", "₩", "₹", "¢"],
    ]
}
