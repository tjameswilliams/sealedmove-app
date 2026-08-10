import SwiftUI

/// Block-level structure recovered from raw coach text. Apple's
/// `inlineOnlyPreservingWhitespace` markdown parsing deliberately ignores
/// block constructs (bullets, headers), so the raw text is split into
/// simple blocks first and inline markdown is applied per block.
enum MarkdownBlock: Equatable {
    case paragraph(String)
    case bullet(String)
    case header(String)
}

/// Pure, unit-testable parsing helpers — no SwiftUI types. Malformed
/// markdown must always degrade to plain text, never crash.
enum MarkdownParser {
    /// Split raw text into paragraphs, `- ` / `* ` bullets, and headers
    /// (`### Header` or a line that is entirely `**Header**`). Consecutive
    /// plain lines merge into one paragraph, preserving their newlines;
    /// blank lines separate paragraphs.
    static func blocks(from raw: String) -> [MarkdownBlock] {
        var result: [MarkdownBlock] = []
        var paragraph: [String] = []

        func flushParagraph() {
            guard !paragraph.isEmpty else { return }
            result.append(.paragraph(paragraph.joined(separator: "\n")))
            paragraph = []
        }

        for line in raw.components(separatedBy: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("- ") || trimmed.hasPrefix("* ") {
                flushParagraph()
                result.append(.bullet(String(trimmed.dropFirst(2))
                    .trimmingCharacters(in: .whitespaces)))
            } else if let header = headerText(trimmed) {
                flushParagraph()
                result.append(.header(header))
            } else if trimmed.isEmpty {
                flushParagraph()
            } else {
                paragraph.append(line)
            }
        }
        flushParagraph()
        return result
    }

    /// Header content of a line, or nil when the line isn't a header.
    /// Recognizes `# ...` through `###### ...` and full-line `**bold**`.
    static func headerText(_ line: String) -> String? {
        if line.hasPrefix("#") {
            let hashes = line.prefix(while: { $0 == "#" })
            guard hashes.count <= 6 else { return nil }
            let rest = line.dropFirst(hashes.count)
            guard rest.hasPrefix(" ") else { return nil }
            let content = rest.trimmingCharacters(in: .whitespaces)
            return content.isEmpty ? nil : content
        }
        // A line that is entirely one **bold** span reads as a header.
        if line.hasPrefix("**"), line.hasSuffix("**"), line.count > 4 {
            let inner = String(line.dropFirst(2).dropLast(2))
            guard !inner.isEmpty, !inner.contains("**") else { return nil }
            return inner
        }
        return nil
    }

    /// Inline markdown (bold/italic/code/links) -> AttributedString,
    /// preserving newlines. Falls back to the plain text on any parse
    /// failure.
    static func inline(_ text: String) -> AttributedString {
        (try? AttributedString(
            markdown: text,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(text)
    }
}

/// Renders coach/system text with light markdown: inline styles via
/// Foundation's markdown parser plus hand-rolled bullets and headers.
/// Font and color come from the environment (set at the call site), so
/// the same view serves body-size feed messages and caption-size logs.
struct MarkdownText: View {
    let text: String
    /// Turn chess mentions (SAN moves, squares, "knight on f3", lexicon
    /// terms) into tappable `coachref://` links — the coach panel
    /// intercepts them via `OpenURLAction`.
    var chessLinks = false

    private func inline(_ content: String) -> AttributedString {
        let parsed = MarkdownParser.inline(content)
        return chessLinks ? ChessLinkifier.linkify(parsed) : parsed
    }

    var body: some View {
        let blocks = MarkdownParser.blocks(from: text)
        VStack(alignment: .leading, spacing: 4) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                switch block {
                case .paragraph(let content):
                    Text(inline(content))
                case .header(let content):
                    Text(inline(content))
                        .fontWeight(.semibold)
                case .bullet(let content):
                    HStack(alignment: .top, spacing: 6) {
                        Text("•")
                        Text(inline(content))
                    }
                }
            }
        }
    }
}
