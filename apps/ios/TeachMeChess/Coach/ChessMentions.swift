import Foundation
import SwiftUI

/// A chess entity recognized inside coach prose. Every mention is encoded
/// as a `coachref://` URL so it can ride inside an `AttributedString` link
/// and be intercepted by the coach panel's `OpenURLAction`.
enum ChessMention: Equatable {
    /// Bare square token ("e4"). Ambiguous by construction — it may name
    /// the square, the piece sitting on it, or a pawn move; the tap
    /// handler resolves it against the board.
    case square(String)
    /// SAN move token ("Nf3", "exd5+", "O-O", "e8=Q#").
    case move(String)
    /// A "knight on f3"-style phrase — the piece on that square.
    case piece(square: String)
    /// A lexicon term (opening or concept), by slug.
    case term(slug: String)

    static let urlScheme = "coachref"

    /// coachref://<kind>/<percent-encoded payload>
    var url: URL? {
        let (kind, payload): (String, String)
        switch self {
        case .square(let sq): (kind, payload) = ("square", sq)
        case .move(let san): (kind, payload) = ("move", san)
        case .piece(let sq): (kind, payload) = ("piece", sq)
        case .term(let slug): (kind, payload) = ("term", slug)
        }
        guard let escaped = payload.addingPercentEncoding(
            withAllowedCharacters: .alphanumerics) else { return nil }
        return URL(string: "\(Self.urlScheme)://\(kind)/\(escaped)")
    }

    /// Decode a `coachref://` URL back into a mention. `URL.path` returns
    /// the payload percent-decoded.
    static func from(_ url: URL) -> ChessMention? {
        guard url.scheme == urlScheme else { return nil }
        let payload = String(url.path.dropFirst())
        guard !payload.isEmpty else { return nil }
        switch url.host {
        case "square": return .square(payload)
        case "move": return .move(payload)
        case "piece": return .piece(square: payload)
        case "term": return .term(slug: payload)
        default: return nil
        }
    }
}

/// One recognized mention and where it sits in the scanned text
/// (character offsets, so the range maps 1:1 onto both `String` and
/// `AttributedString.characters`).
struct MentionMatch: Equatable {
    let start: Int
    let length: Int
    let mention: ChessMention
}

/// Pure text scanner: finds chess mentions in coach prose. No SwiftUI, no
/// board state — classification of ambiguous tokens happens at tap time.
enum ChessMentionScanner {
    /// SAN move or bare square, bounded so words never match mid-token.
    /// Alternates, in order: castling, piece move, pawn capture, bare
    /// square / pawn push (with optional promotion), each with optional
    /// check/mate suffix.
    private static let sanPattern =
        "(?<![A-Za-z0-9/=])"
        + "(O-O-O|O-O"
        + "|[KQRBN][a-h]?[1-8]?x?[a-h][1-8]"
        + "|[a-h]x[a-h][1-8](?:=[QRBN])?"
        + "|[a-h][1-8](?:=[QRBN])?"
        + ")[+#]?"
        + "(?![A-Za-z0-9-])"

    private static let sanRegex = try? NSRegularExpression(pattern: sanPattern)

    /// "knight on f3" / "bishop at c4" — the piece occupying a square.
    private static let piecePhraseRegex = try? NSRegularExpression(
        pattern: "\\b(?:king|queen|rook|bishop|knight|pawn)s?\\s+(?:on|at)\\s+([a-h][1-8])\\b",
        options: [.caseInsensitive])

    /// All mentions in `text`, non-overlapping, sorted by position.
    /// Precedence when ranges collide: lexicon terms, then piece phrases,
    /// then SAN tokens ("knight on f3" swallows the bare "f3" inside it).
    static func matches(in text: String, lexicon: Lexicon? = .shared) -> [MentionMatch] {
        let ns = text as NSString
        let full = NSRange(location: 0, length: ns.length)
        // Candidates carry a priority; lower wins on overlap.
        var candidates: [(priority: Int, match: MentionMatch)] = []

        if let lexicon {
            for hit in lexicon.termMatches(in: text) {
                candidates.append((0, MentionMatch(
                    start: hit.start, length: hit.length,
                    mention: .term(slug: hit.slug))))
            }
        }

        piecePhraseRegex?.enumerateMatches(in: text, range: full) { result, _, _ in
            guard let result, result.numberOfRanges > 1 else { return }
            let square = ns.substring(with: result.range(at: 1))
            candidates.append((1, MentionMatch(
                start: charOffset(of: result.range.location, in: ns),
                length: charLength(of: result.range, in: ns),
                mention: .piece(square: square.lowercased()))))
        }

        sanRegex?.enumerateMatches(in: text, range: full) { result, _, _ in
            guard let result else { return }
            let token = ns.substring(with: result.range)
            let bare = token.strippedSanDecorations
            let mention: ChessMention = bare.isBareSquare ? .square(bare) : .move(token)
            candidates.append((2, MentionMatch(
                start: charOffset(of: result.range.location, in: ns),
                length: charLength(of: result.range, in: ns),
                mention: mention)))
        }

        // Overlap resolution: by priority, then longest first; keep a
        // candidate only when it collides with nothing already accepted.
        var accepted: [MentionMatch] = []
        for (_, match) in candidates.sorted(by: {
            ($0.priority, -$0.match.length) < ($1.priority, -$1.match.length)
        }) {
            let range = match.start..<(match.start + match.length)
            let collides = accepted.contains {
                range.overlaps($0.start..<($0.start + $0.length))
            }
            if !collides { accepted.append(match) }
        }
        return accepted.sorted { $0.start < $1.start }
    }

    /// UTF-16 location -> character offset (SAN/term text is ASCII-heavy,
    /// but coach prose can contain emoji/curly quotes before a token).
    private static func charOffset(of utf16Location: Int, in ns: NSString) -> Int {
        let prefix = ns.substring(to: utf16Location)
        return prefix.count
    }

    private static func charLength(of range: NSRange, in ns: NSString) -> Int {
        ns.substring(with: range).count
    }
}

extension String {
    /// SAN with annotation/check decorations stripped: "Nf3!?" -> "Nf3",
    /// "exd5+" -> "exd5".
    var strippedSanDecorations: String {
        var s = self
        while let last = s.last, "+#!?".contains(last) { s.removeLast() }
        return s
    }

    var isBareSquare: Bool {
        count == 2
            && "abcdefgh".contains(first ?? " ")
            && "12345678".contains(last ?? " ")
    }
}

// MARK: - Linkifier

/// Applies chess-mention links to an already-markdown-parsed
/// `AttributedString`. Scanning happens on the flattened characters, so
/// bold/italic markers never shift the ranges.
enum ChessLinkifier {
    static func linkify(_ attributed: AttributedString, lexicon: Lexicon? = .shared) -> AttributedString {
        var result = attributed
        let plain = String(result.characters)
        for match in ChessMentionScanner.matches(in: plain, lexicon: lexicon) {
            guard let url = match.mention.url,
                  let lower = result.characters.index(
                      result.startIndex, offsetBy: match.start,
                      limitedBy: result.endIndex),
                  let upper = result.characters.index(
                      lower, offsetBy: match.length, limitedBy: result.endIndex)
            else { continue }
            let range = lower..<upper
            // Don't stomp real markdown links the model may have written.
            guard result[range].runs.allSatisfy({ $0.link == nil }) else { continue }
            result[range].link = url
            switch match.mention {
            case .term:
                result[range].foregroundColor = .indigo
                result[range].underlineStyle = .single
            default:
                result[range].foregroundColor = .teal
                result[range].font = .body.weight(.semibold)
            }
        }
        return result
    }
}

// MARK: - Move resolution

/// One arrow drawn over the board, from square center to square center.
struct BoardArrow: Equatable, Hashable, Identifiable {
    let from: String
    let to: String
    var id: String { "\(from)-\(to)" }
}

/// Resolves SAN tokens to concrete from/to squares by replaying the move
/// on a throwaway `BoardHandle` and diffing the piece maps. Pure
/// computation (the Rust board layer), safe from the main thread.
enum MoveResolver {
    /// (from, to) for `san` played in `fen`, or nil when illegal there.
    /// Castling resolves to the king's from/to squares.
    static func resolve(san: String, inFen fen: String) -> BoardArrow? {
        let clean = san.strippedSanDecorations
        guard !clean.isEmpty,
              let board = try? BoardHandle.fromFen(fen: fen) else { return nil }
        let before = GameViewModel.parseFenBoard(board.fen())
        let moverIsWhite = board.turnWhite()
        guard (try? board.playSan(san: clean)) != nil else { return nil }
        let after = GameViewModel.parseFenBoard(board.fen())

        // Vacated mover-color squares. One for a normal move; king + rook
        // for castling; en passant's captured pawn is opponent-colored and
        // filtered out.
        let vacated = before.filter { square, piece in
            piece.isWhite == moverIsWhite && after[square] != piece
        }
        // Squares newly holding a mover-color piece.
        let landed = after.filter { square, piece in
            piece.isWhite == moverIsWhite && before[square] != piece
        }

        if clean.hasPrefix("O-O") {
            guard let from = vacated.first(where: { $0.value.kind == "k" })?.key,
                  let to = landed.first(where: { $0.value.kind == "k" })?.key
            else { return nil }
            return BoardArrow(from: from, to: to)
        }
        guard vacated.count == 1, let from = vacated.first?.key,
              let to = landed.first?.key else { return nil }
        return BoardArrow(from: from, to: to)
    }

    /// Destination square encoded in a SAN token, if any — the last
    /// file+rank pair ("Nbxd5+" -> "d5"). Fallback highlighting for moves
    /// that resolve in no known position.
    static func destinationSquare(of san: String) -> String? {
        let clean = san.strippedSanDecorations
        // Promotion suffix hides the square: strip "=Q".
        let core = clean.split(separator: "=").first.map(String.init) ?? clean
        guard core.count >= 2 else { return nil }
        let chars = Array(core)
        for i in stride(from: chars.count - 2, through: 0, by: -1) {
            let pair = String(chars[i...i + 1])
            if pair.isBareSquare { return pair }
        }
        return nil
    }
}
