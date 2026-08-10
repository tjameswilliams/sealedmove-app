# TeachMeChess — iOS app

A SwiftUI prototype of the real product: play a full game against the
embedded engine while the coach judges every move. The Rust core
(`coach-ffi` via UniFFI) supplies everything — board legality, the
statically linked Stockfish 11 analyst + opponent, move judgments,
accuracy/rating tracking, and the LLM coach loop.

## Setup

Requirements: Xcode (iOS 17+ SDK; iOS 26 SDK for the Foundation Models
coach), [rustup](https://rustup.rs) (the Homebrew cargo cannot cross-compile
to iOS), and [xcodegen](https://github.com/yonaskolb/XcodeGen).

From the repo root:

```sh
# 1. Build the Rust static libs, generate Swift bindings, assemble
#    apps/ios/Frameworks/CoachFFI.xcframework and
#    apps/ios/TeachMeChess/Generated/coach_ffi.swift
./scripts/build-ios-xcframework.sh

# 2. Generate the Xcode project
cd apps/ios && xcodegen generate

# 3. Open or build
open TeachMeChess.xcodeproj
# or:
xcodebuild -project TeachMeChess.xcodeproj -scheme TeachMeChess \
    -destination 'generic/platform=iOS Simulator' build
```

Re-run step 1 whenever the Rust surface changes; re-run step 2 whenever
files are added/removed under `TeachMeChess/`. The `.xcodeproj`, the
XCFramework, and the generated Swift are all build products and gitignored.

Note: the simulator slice is built for `aarch64-apple-ios-sim` only (Apple
Silicon); `x86_64` is excluded via `EXCLUDED_ARCHS`.

## Run on your iPhone

The project uses automatic code signing, but a physical device needs your
Apple Developer team ID. That lives in a LOCAL, gitignored file —
`apps/ios/signing.xcconfig` — pulled in through the committed
`Config.xcconfig` via xcconfig's optional `#include?` directive, so
simulator builds keep working when the file is absent. (xcodegen itself has
no optional-include flag for spec ymls — a missing included yml is a hard
parse error — which is why this is an xcconfig rather than an xcodegen
include.)

1. Create `apps/ios/signing.xcconfig` containing exactly one line (your
   team ID is on <https://developer.apple.com/account> under Membership):

   ```
   DEVELOPMENT_TEAM = ABCDE12345
   ```

2. `cd apps/ios && xcodegen generate` (only needed if you haven't generated
   the project yet — the xcconfig itself is read at build time).
3. `open TeachMeChess.xcodeproj`, select your iPhone as the run
   destination, and Run. First deploy to a new device: enable Developer
   Mode on the phone and trust the developer certificate under
   Settings → General → VPN & Device Management.

On real hardware with Apple Intelligence (iOS 26+), the on-device
Foundation Models coach activates automatically — the simulator usually
falls back to the canned coach.

## Layout

The board (plus the thin status strip above it) is docked to the top and
never scrolls. Below it:

- **Moves pill** — a single compact line ("6. Bd3 Nf6 · 12 moves ›")
  between board and coach panel. Tapping it opens the move-review sheet.
- **Coach panel** — fills the remaining height with its own internal
  scroller: a chronological feed (auto-scrolls to the newest message) with
  the chat input pinned at the panel's bottom, above the keyboard. The
  header is a single compact row (cap icon + "Coach") under a grab bar —
  the active backend/opponent info that used to live in a header subtitle
  now appears in Settings and as system lines in the feed.

### Drag-to-resize coach panel

A standard iOS grab bar (36×5pt pill) tops the coach panel; dragging it —
or anywhere on the slim header row (~44pt hit zone) — continuously resizes
the board/panel split. Dragging up grows the panel while the board shrinks,
keeping its square aspect, staying horizontally centered, with the
captured-piece trays hugging the shrunken board's edges; dragging down
returns the board toward full width. Below a ~200pt board edge the board
morphs (animated spring) into the compact context strip — a ~132pt
thumbnail beside whose-turn text and the latest move pair — and the moves
pill hides; keep dragging for maximum reading space. The split is stored
as a 0…1 fraction (0 = full board, 1 = max read), clamped at both ends
with drags settling exactly at an extreme when they end within 5% of it,
and persists in UserDefaults (`coach.panelFraction`) so the layout
survives relaunch. **Double-tapping the grab bar snaps between the two
presets: full board and max read.** The resize gesture lives on the grab
zone only — never on the feed, so feed scrolling and message links are
unaffected; the strip is non-interactive (tapping it only dismisses the
keyboard) and updates live if an opponent move lands while reading. The
board stays fully playable at any reduced size.

### Captured-pieces trays

Two slim rows hug the board: above it, the pieces the opponent (Black) has
captured — white glyphs; below it, the student's haul — black glyphs. Rows
render most-valuable-first with a slight overlap, the side that's ahead on
material gets a bold "+N" badge (pawn units, computed board-side so
promotions count at full value), and a row collapses to nothing while
empty. The data comes from `boardMirror.materialSummary()` (Rust
`MaterialSummary` JSON) on every refresh — student and opponent moves,
resume, reset, and in-game review all update it. The History detail's
replay board shows the same trays, stepping with ◀ ▶; the compact reading-
mode strip shows just the badge (with a tiny glyph of the biggest captured
piece) when material is uneven.

### Markdown in coach messages

Coach, system, and opponent-note messages render light markdown (student
messages stay verbatim plain text): inline bold/italic/code/links via
Foundation's `AttributedString(markdown:)` in
`inlineOnlyPreservingWhitespace` mode, plus hand-rolled block handling
(`- `/`* ` bullets, `### `/full-line-`**bold**` headers) in
`MarkdownText.swift` — the parsing helpers are pure and unit-testable, and
malformed markdown falls back to plain text. The same `MarkdownText` view
is reused in the History detail chat log. Prompt-side, the text-modality
system prompt permits light markdown (bold move names, short dashed
lists); the voice modality stays markdown-free for TTS.

### Tappable chess mentions (coach ↔ board linking)

Chess entities inside coach and opponent-note messages become tappable
links (`ChessMentions.swift` scans the markdown-parsed text — bold
markers never shift ranges): SAN moves (`Nf3`, `exd5+`, `O-O`), bare
squares (`e4`), and "knight on f3"-style phrases render teal; lexicon
terms render indigo-underlined. Each is a `coachref://` URL intercepted
by an `OpenURLAction` on the message row, so real web links still open
normally.

Tapping a **piece/square** mention pulses that square teal on the board
(and the compact reading-mode strip), plus draws arrows for any moves
the same message mentions for that piece. Tapping a **move** mention
draws an animated arrow (`ArrowOverlay.swift`, spring draw-in from
source to destination). Every feed message is stamped with the FEN/ply
it was created at, and SAN resolves against contexts in order: the
moves actually played around that ply, the message's own position, then
the currently displayed board — falling back to highlighting the
destination square. Resolution replays SAN on a throwaway `BoardHandle`
and diffs the piece maps (`MoveResolver`), so castling maps to the
king's from/to. The spotlight clears on board interaction, any move,
review enter/exit, tapping the same mention again, or after 30 s.

### Chess lexicon

A curated library (`Resources/Lexicon.json`, validated by
`coach-ffi/tests/lexicon_data.rs` — every start FEN must parse and
every SAN demo line must be legal) of ~45 openings, tactics, and
concepts: textbook definition, strengths/weaknesses, and an animated
demo board (`LexiconSheet.swift`) that auto-plays the defining line
with move arrows, loop-around, scrub controls, and a tappable SAN
ticker. Reachable two ways: tapping a term the coach mentions (aliases
cover defence/defense spellings and apostrophe variants), or browsing
the whole library from the toolbar book icon (`LexiconBrowser`,
grouped + searchable). Opening names outside the curated set fall back
to the full vendored lichess ECO database (`Resources/Openings/*.tsv`,
CC0, same data coach-core embeds): the base name before the colon
indexes to its shortest book line, so any named opening still gets an
animated line and ECO badge.

### Keyboard behavior

The board's size derives from the screen width and the user's split
fraction — the keyboard changes neither, so the board never resizes when
the keyboard appears; only the coach panel absorbs the height change. A
full-width board plus keyboard plus chrome physically exceeds an iPhone
screen, so while the chat field is focused the secondary chrome (nav bar,
status strip, trays, moves pill, coach-panel grab bar + header) collapses
and the split drag is suspended (the grab zone hides with the header); the
board keeps its exact user-chosen size, the chat input stays above the
keyboard with the newest feed message in view, and dismissing the keyboard
restores the chrome and the user's split unchanged. Dismissal: drag the feed down
(`.scrollDismissesKeyboard(.interactively)`), tap anywhere outside the
input (the tap-to-dismiss is a *simultaneous* gesture, so a tap on the
board also selects/plays as normal — no dead first tap), or press return
(which also sends the message).

### Move review (rewind)

Tap any ply in the move sheet to enter review mode: the board shows the
position after that ply (rebuilt by replaying the SAN history onto a
temporary board — the live session is never touched), an orange "Reviewing
3\. Nf3 — Back to live" banner replaces the turn banner, move input is
disabled, and the last-move highlight tracks the reviewed ply. Chat still
works while reviewing: the outgoing message is invisibly prefixed with the
reviewed move number and FEN so the coach answers about *that* position,
and replies are tagged with a small "review" label. "Back to live" (or a
new game) restores the live board.

## Persistence, resume, and history

All games persist to SQLite at `Documents/coach.db` (schema owned by the
Rust core — `coach-core`'s `GameStore`, WAL mode). On session creation the
app calls `resumeFromStore(dbPath:)`: once the store is attached, the Rust
session auto-records every move, verdict, chat message, and the LLM
transcript. Feed lines the app generates locally (canned greeting,
backend-switch notices, `briefReaction` verdict lines, error notes, the
resume marker) are persisted via `logFeed`, so a resumed feed is complete.

**Resume**: quitting mid-game leaves the game row open; the next launch
restores it — board position (SAN history replayed onto the mirror), move
pill, and the full chat feed (review-mode questions are stored with their
invisible grounding prefix; the app strips it back off and re-tags them
"review") — and posts a subtle "Resumed game in progress (N moves)" system
line. If the app died before the opponent's reply, the opponent moves on
resume. A corrupt stored game is closed as aborted and the app starts
fresh — resume never errors over stale data.

**Game lifecycle**: `finishGame` finalizes the stored row (result, ACL,
accuracy, est. rating, opening/ECO); "New Game" → `resetGame` begins a
fresh row (any still-open row is closed as aborted). Mid-game the New Game
button asks first: **Resign** (records a loss, shows the summary sheet)
or **Abandon** (aborts the row).

**History** (clock toolbar button): a sheet listing finished games newest
first via the `historyList`/`storeStats`/`gameDetail` FFI free functions —
short-lived read-only SQLite connections, so browsing never touches the
live session. Header: games played, W-L-D, best accuracy. Rows: result
badge (W green / L red / D gray / A dim), opening, date, accuracy %, est.
rating, move count, with "Load more" pagination. Tapping a game opens a
detail view: a read-only board stepped with ◀ ▶ (stored SAN replayed onto
a fresh `BoardHandle`), the verdict chip on the stepped student move, a
tappable move grid, the game's chat log, and an "Ask about this position…"
field that queries the LIVE session's coach with a past-game/move/FEN
grounding prefix (review-tagged replies, shown inline in the detail view).

## Coach backend settings

The gear button opens Settings with the two product tiers:

- **On-device** — Foundation Models when available, else the canned coach.
  Free, runs entirely on the device, no key or account.
- **Pro Coach** — the managed coach service. The device registers itself
  with the proxy (anonymous device id + JWT, no account) and gets a free
  trial; afterwards it's a $5.99/month subscription. No API key ever
  reaches the app, and the UI never names the underlying model — that's a
  server-side detail the proxy swaps freely.

(BYO-key providers from earlier dev builds are gone; the Rust core still
supports arbitrary OpenAI-compatible backends for CLI/dev use.)

Applying calls the session's runtime `setBackend*` FFI methods — the
embedded engine is a per-process singleton, so the backend swaps on the
LIVE session (game, engines, and profile survive; the coach's conversation
transcript resets). The active backend/engine/opponent summary shows in a
"Session" row at the top of Settings and is posted to the feed as a system
line on session start; a system line carrying the same info (backend ·
engine · opponent level) announces every switch.

### Coach chattiness

A segmented control at the top of Settings (Quiet / Balanced / Chatty,
default **Chatty**) sets how much the coach talks. The cadence itself lives
in the Rust core's commentary policy (`setCommentaryStyle` +
`reactToStudentMove` / `reactToOpponentMove` FFI methods) — the app just
renders what the policy decides to say. Opponent-move observations (threat
warnings, phase changes, game-story summaries) appear as distinct
eye-icon notes in the feed.

- **Quiet** — the coach only speaks up about inaccuracies, mistakes, and
  blunders; everything else gets a one-line canned verdict, and opponent
  moves pass without comment.
- **Balanced** — full reactions to notable moves and milestones, a "why
  that was good" note every third good move, and opponent commentary only
  on a real threat or big eval swing (plus periodic summaries).
- **Chatty** — a full reaction to every move, opponent commentary whenever
  anything is worth flagging, and frequent development summaries.

The choice persists in UserDefaults (`coach.commentaryStyle`), applies
immediately (no Apply needed), is re-applied on launch and on resume, and
posts a "Coach chattiness set to …" system line to the feed. With the
canned/on-device backend the "full LLM" paths degrade to deterministic
canned lines and engine-built notes — nothing breaks without a network
model. The "Coach is thinking…" indicator only appears when a reply takes
more than ~300 ms, so canned lines feel instant.

Storage: API keys go in the **iOS Keychain** (generic password, service
`dev.teachmechess.app`, one account per provider) — never UserDefaults.
Non-secrets (provider, base URL, model names) persist in UserDefaults. On
launch the saved provider is restored, falling back to On-device when its
key is missing. Network/model failures during coaching degrade to friendly
feed messages (and the engine's templated verdict), never a crash.

## What's wired up

- **A real `CoachSessionHandle` session** using the embedded-Stockfish
  constructors (`newWithForeignModelEmbedded` + `attachOpponentAnalyst`) —
  no subprocess, so it runs on-device. The session is the source of truth
  for the game; a `BoardHandle` mirror serves the UI's synchronous needs
  (selection, legal-move dots) and is replayed/resynced after every move.
- **Full move pipeline** per student move: engine judgment (`judgeMove`) →
  color-coded verdict chip on the board → commentary via the Rust
  commentary engine (`reactToStudentMove`, cadence per the chattiness
  setting; the "coach is thinking" indicator appears only for slow
  replies) → engine opponent reply, animated onto the board →
  `reactToOpponentMove` opponent notes (eye icon) when the policy speaks.
  Input is locked while the pipeline runs.
- **Runtime backend switching** (`setBackendOpenai` / `setBackendAnthropic`
  / `setBackendForeign` / `setBackendEngineOnly`) — see "Coach backend
  settings" above.
- **Move review / rewind** with review-grounded chat — see "Move review"
  above.
- **Game over**: `finishGame` (student plays White) produces a summary
  sheet — moves judged, accuracy %, average centipawn loss, estimated
  rating, and an advancement banner when the profile is ready to level up.
  "New game" reuses the same session via `resetGame` (the embedded engine
  is a per-process singleton — a second session cannot be created).
- **Student profile persistence** — saved to the app's Documents directory
  after each finished game and passed back into the session on launch, so
  the rating EMA and history survive restarts.
- **Game persistence + resume + history** — SQLite via the Rust core; see
  "Persistence, resume, and history" above.
- **Coach chat** — a text field pinned under the coach feed drives
  `chat(...)`; the feed is chronological and auto-scrolls.
- **Coach backends** (`ForeignCoachModel`): `FoundationModelsCoach` (Apple
  Foundation Models, on-device, iOS 26+) when
  `SystemLanguageModel.default.isAvailable`, else `CannedCoachModel`
  (canned, phase-appropriate lines). The active backend and opponent level
  show in Settings ("Session" row) and as system lines in the coach feed.

## Licensing note (GPL)

The app links `stockfish-embedded`, a GPL-3.0-only crate vendoring
Stockfish 11. Statically linking it makes the combined app binary a GPLv3
work — see [`crates/stockfish-embedded/README.md`](../../crates/stockfish-embedded/README.md)
before any public distribution.

## Future work: human-like opponent play

The opponent is currently the analyst engine with Stockfish's "Skill Level"
dialed down (level 3 by default). Skill-limited Stockfish does not blunder
the way a human at that strength does — the plan remains an in-process
Maia (lc0 + maia weights) opponent for human-like play at calibrated rating
bands.
