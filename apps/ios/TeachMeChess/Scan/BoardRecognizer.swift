import Foundation
import UIKit

/// A chess position read out of a photo, before the student confirms it.
/// Recognition is best-effort: the confirm/edit screen is part of the flow,
/// not a fallback, so a mostly-right answer is still a good answer.
struct RecognizedPosition {
    /// FEN board field only ("rnbqkbnr/pppppppp/8/…"), White's perspective.
    var boardField: String
    /// Side to move when the source made it explicit (a caption, a move
    /// indicator); nil when the photo doesn't say.
    var whiteToMove: Bool?
    /// One short line worth surfacing ("the h-file is cropped").
    var notes: String?
}

/// Anything that can turn a board photo into a position. The shipping
/// implementation is the on-device CoreML recognizer (`LocalBoardRecognizer`);
/// the protocol keeps a second backend possible without touching the flow.
protocol BoardRecognizer {
    func recognize(_ image: UIImage) async throws -> RecognizedPosition
}
