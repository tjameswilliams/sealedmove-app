import Foundation

/// One lexicon entry: a named opening or chess concept with a textbook
/// definition and an animated demo line. Curated entries (Lexicon.json)
/// carry strengths/weaknesses; ECO-book fallback entries carry just the
/// name and line.
struct LexiconEntry: Identifiable, Equatable, Decodable {
    enum Category: String, Decodable {
        case opening, tactic, concept

        var label: String {
            switch self {
            case .opening: return "Opening"
            case .tactic: return "Tactic"
            case .concept: return "Concept"
            }
        }
    }

    var id: String { slug }
    let slug: String
    let name: String
    let eco: String?
    let category: Category
    /// Alternate spellings the coach might use ("vienna", "spanish game").
    /// American/British "defense"/"defence" variants are generated, not
    /// listed.
    let aliases: [String]
    let definition: String
    let strengths: [String]
    let weaknesses: [String]
    /// Demo start position; nil = the standard starting position.
    let startFen: String?
    /// SAN demo line played on the sheet's animated board. Empty = static
    /// diagram (or no board at all when startFen is also nil).
    let moves: [String]
    /// One-line label under the demo board ("The knight forks king and
    /// rook.").
    let caption: String?

    private enum CodingKeys: String, CodingKey {
        case slug, name, eco, category, aliases, definition
        case strengths, weaknesses, startFen, moves, caption
    }

    init(slug: String, name: String, eco: String?, category: Category,
         aliases: [String] = [], definition: String,
         strengths: [String] = [], weaknesses: [String] = [],
         startFen: String? = nil, moves: [String] = [], caption: String? = nil) {
        self.slug = slug
        self.name = name
        self.eco = eco
        self.category = category
        self.aliases = aliases
        self.definition = definition
        self.strengths = strengths
        self.weaknesses = weaknesses
        self.startFen = startFen
        self.moves = moves
        self.caption = caption
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        slug = try c.decode(String.self, forKey: .slug)
        name = try c.decode(String.self, forKey: .name)
        eco = try c.decodeIfPresent(String.self, forKey: .eco)
        category = try c.decode(Category.self, forKey: .category)
        aliases = try c.decodeIfPresent([String].self, forKey: .aliases) ?? []
        definition = try c.decode(String.self, forKey: .definition)
        strengths = try c.decodeIfPresent([String].self, forKey: .strengths) ?? []
        weaknesses = try c.decodeIfPresent([String].self, forKey: .weaknesses) ?? []
        startFen = try c.decodeIfPresent(String.self, forKey: .startFen)
        moves = try c.decodeIfPresent([String].self, forKey: .moves) ?? []
        caption = try c.decodeIfPresent(String.self, forKey: .caption)
    }
}

/// The chess vocabulary the coach panel can link into: curated entries
/// from Lexicon.json plus a name index over the full vendored ECO book
/// (the same lichess database coach-core embeds), so any opening the
/// coach names by its book name gets at least an animated line.
final class Lexicon {
    static let shared: Lexicon? = Lexicon()

    /// Curated entries, in JSON order (used by any future browse UI).
    let entries: [LexiconEntry]

    /// Normalized alias -> curated slug.
    private let aliasIndex: [String: String]
    private let entryBySlug: [String: LexiconEntry]
    /// Normalized ECO base name (text before ":") -> (display name, eco,
    /// SAN line). Kept to the shortest line per name = the main line.
    private let ecoIndex: [String: (name: String, eco: String, moves: [String])]
    /// One alternation regex over every curated alias, longest first.
    private let aliasRegex: NSRegularExpression?
    /// Proper-noun phrases ending in an opening keyword — candidates for
    /// ECO index lookup ("Vienna Game", "Latvian Gambit").
    private let ecoPhraseRegex: NSRegularExpression?

    private init?() {
        guard let url = Bundle.main.url(forResource: "Lexicon", withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let decoded = try? JSONDecoder().decode([LexiconEntry].self, from: data)
        else { return nil }
        entries = decoded
        entryBySlug = Dictionary(uniqueKeysWithValues: decoded.map { ($0.slug, $0) })

        // The alias index is keyed by canonical form; the regex matches the
        // RAW spellings (apostrophes, curly quotes, defence/defense) as
        // they appear in prose.
        var aliases: [String: String] = [:]
        var rawSpellings: Set<String> = []
        for entry in decoded {
            for alias in [entry.name] + entry.aliases {
                aliases[Self.canonicalKey(alias)] = entry.slug
                rawSpellings.formUnion(Self.rawSpellings(alias))
            }
        }
        aliasIndex = aliases

        ecoIndex = Self.loadEcoIndex()

        let sortedAliases = rawSpellings.sorted { $0.count > $1.count }
        let alternation = sortedAliases
            .map(NSRegularExpression.escapedPattern(for:))
            .joined(separator: "|")
        aliasRegex = alternation.isEmpty ? nil : try? NSRegularExpression(
            pattern: "\\b(?:\(alternation))\\b", options: [.caseInsensitive])
        ecoPhraseRegex = try? NSRegularExpression(
            pattern: "\\b(?:[A-Z][A-Za-z'’-]*+\\s+){0,4}"
                + "(?:Defense|Defence|Gambit|Opening|Attack|Game|Variation|System|Countergambit)\\b")
    }

    // MARK: Lookup

    /// Entry for a slug produced by `termMatches` — curated, or an
    /// on-the-fly ECO entry for "eco:<normalized name>" slugs.
    func entry(slug: String) -> LexiconEntry? {
        if let curated = entryBySlug[slug] { return curated }
        guard slug.hasPrefix("eco:"),
              let hit = ecoIndex[String(slug.dropFirst(4))] else { return nil }
        return LexiconEntry(
            slug: slug, name: hit.name, eco: hit.eco, category: .opening,
            definition: "A named line from the opening book (lichess ECO database). "
                + "Watch the moves that define it — the coach can tell you more "
                + "about the ideas behind it.",
            moves: hit.moves)
    }

    // MARK: Text matching

    struct TermHit {
        let start: Int
        let length: Int
        let slug: String
    }

    /// Occurrences of known terms in coach prose (character offsets).
    /// Curated aliases win over raw ECO names.
    func termMatches(in text: String) -> [TermHit] {
        let ns = text as NSString
        let full = NSRange(location: 0, length: ns.length)
        var hits: [TermHit] = []

        aliasRegex?.enumerateMatches(in: text, range: full) { result, _, _ in
            guard let result else { return }
            let matched = Self.canonicalKey(ns.substring(with: result.range))
            guard let slug = self.aliasIndex[matched] else { return }
            hits.append(TermHit(
                start: ns.substring(to: result.range.location).count,
                length: ns.substring(with: result.range).count,
                slug: slug))
        }

        ecoPhraseRegex?.enumerateMatches(in: text, range: full) { result, _, _ in
            guard let result else { return }
            let phrase = ns.substring(with: result.range)
            let key = Self.canonicalKey(phrase)
            guard self.ecoIndex[key] != nil else { return }
            let start = ns.substring(to: result.range.location).count
            let length = phrase.count
            // Skip when a curated alias already covers this stretch.
            let range = start..<(start + length)
            let covered = hits.contains { range.overlaps($0.start..<($0.start + $0.length)) }
            if !covered {
                hits.append(TermHit(start: start, length: length, slug: "eco:\(key)"))
            }
        }
        return hits
    }

    // MARK: Normalization

    /// Lowercased, punctuation-free, whitespace-collapsed key form.
    static func normalize(_ raw: String) -> String {
        raw.lowercased()
            .replacingOccurrences(of: "’", with: "'")
            .filter { !":,.()'".contains($0) }
            .split(separator: " ")
            .joined(separator: " ")
    }

    /// Canonical index key: normalized, with the American "defense"
    /// spelling.
    static func canonicalKey(_ raw: String) -> String {
        normalize(raw).replacingOccurrences(of: "defence", with: "defense")
    }

    /// Every raw spelling of an alias that could appear in prose:
    /// defence/defense twins, each with straight, curly, and absent
    /// apostrophes ("King's Gambit", "King’s Gambit", "Kings Gambit").
    static func rawSpellings(_ alias: String) -> Set<String> {
        var spellings: Set<String> = [alias]
        for base in [alias,
                     alias.replacingOccurrences(of: "efence", with: "efense"),
                     alias.replacingOccurrences(of: "efense", with: "efence")] {
            let straight = base.replacingOccurrences(of: "’", with: "'")
            spellings.insert(straight)
            spellings.insert(straight.replacingOccurrences(of: "'", with: "’"))
            spellings.insert(straight.replacingOccurrences(of: "'", with: ""))
        }
        return spellings
    }

    // MARK: ECO book

    /// Parse the bundled lichess TSVs (eco \t name \t pgn) into a base-
    /// name index. Multiple book lines share a base name; the shortest is
    /// kept as the family's main line.
    private static func loadEcoIndex() -> [String: (name: String, eco: String, moves: [String])] {
        var index: [String: (name: String, eco: String, moves: [String])] = [:]
        for volume in ["a", "b", "c", "d", "e"] {
            guard let url = Bundle.main.url(forResource: volume, withExtension: "tsv"),
                  let content = try? String(contentsOf: url, encoding: .utf8)
            else { continue }
            for line in content.split(separator: "\n").dropFirst() {
                let cols = line.split(separator: "\t", maxSplits: 2)
                guard cols.count == 3 else { continue }
                let eco = String(cols[0])
                let fullName = String(cols[1])
                // "1. e4 e5 2. Nf3" -> ["e4", "e5", "Nf3"]
                let moves = cols[2].split(separator: " ")
                    .filter { !$0.hasSuffix(".") }
                    .map(String.init)
                guard !moves.isEmpty else { continue }
                let baseName = fullName.split(separator: ":").first
                    .map(String.init) ?? fullName
                let key = canonicalKey(baseName)
                if let existing = index[key], existing.moves.count <= moves.count {
                    continue
                }
                index[key] = (baseName, eco, moves)
            }
        }
        return index
    }
}
