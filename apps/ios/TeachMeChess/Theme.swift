import SwiftUI

/// Sealed Move brand palette — "the annotated game": paper and ink with a
/// red annotation pen, on the green-and-buff of a tournament vinyl board.
/// Annotation red is reserved for insight moments (coach arrows, spotlights,
/// blunder marks); it is never decoration.
enum Brand {
    /// Light squares and raised paper surfaces (buff).
    static let boardLight = Color(red: 0.929, green: 0.890, blue: 0.780)
    /// Dark squares — FIDE-vinyl green, deliberately deeper than the
    /// grass green other chess apps use.
    static let boardDark = Color(red: 0.357, green: 0.478, blue: 0.314)
    /// The single accent: the coach's red pen.
    static let annotation = Color(red: 0.753, green: 0.212, blue: 0.102)
    /// Secondary brand green for calm coach presence and success states.
    static let tournamentGreen = Color(red: 0.247, green: 0.353, blue: 0.235)
}

/// Legal links App Review requires wherever the subscription is offered
/// (guideline 3.1.2): terms + privacy at every purchase surface, and the
/// open-source/licensing page behind the Stockfish acknowledgement.
enum Legal {
    /// Apple's standard EULA — used instead of custom terms.
    static let termsURL = URL(string: "https://www.apple.com/legal/internet-services/itunes/dev/stdeula/")!
    static let privacyURL = URL(string: "https://sealedmove.com/privacy")!
    /// Source & licenses page: Stockfish GPLv3 credit + the app's
    /// corresponding-source location.
    static let sourceURL = URL(string: "https://sealedmove.com/source")!
}
