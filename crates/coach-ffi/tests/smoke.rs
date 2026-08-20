//! End-to-end smoke test through the public FFI surface, using a real
//! Stockfish if one is on PATH. Skips (stays green) when it isn't, so CI
//! without an engine still passes.

use coach_ffi::session::{CoachSessionHandle, FfiCommentaryStyle, FfiGameResult};

/// Locate a `stockfish` binary on PATH, or `None` to skip.
fn stockfish_on_path() -> Option<String> {
    let out = std::process::Command::new("which")
        .arg("stockfish")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!path.is_empty()).then_some(path)
}

#[test]
fn engine_only_session_judges_and_finishes() {
    let Some(stockfish) = stockfish_on_path() else {
        eprintln!("skipping: no stockfish on PATH");
        return;
    };

    let handle = CoachSessionHandle::new_engine_only(stockfish, None, None, false)
        .expect("engine-only session constructs");

    // Judge 1. e4 — verdict JSON must parse and carry the played move.
    let verdict_json = handle.judge_move("e4".into()).expect("judge_move succeeds");
    let verdict: serde_json::Value =
        serde_json::from_str(&verdict_json).expect("verdict is valid JSON");
    assert_eq!(verdict["played_san"], "e4");
    assert!(verdict.get("judgment").is_some());
    assert!(verdict.get("cp_loss").is_some());

    // Templated commentary works without an LLM.
    assert!(!handle.brief_reaction().is_empty());

    // Board state reflects the move.
    assert!(handle.fen().contains("b KQkq")); // black to move
    assert_eq!(handle.history_san(), vec!["e4".to_string()]);
    assert!(!handle.is_game_over());

    // Finish the game — summary JSON must parse with the expected fields.
    let summary_json = handle.finish_game(FfiGameResult::Aborted);
    let summary: serde_json::Value =
        serde_json::from_str(&summary_json).expect("summary is valid JSON");
    assert_eq!(summary["moves_judged"], 1);
    assert!(summary.get("acl").is_some());
    assert!(summary.get("accuracy").is_some());
    assert!(summary.get("est_rating").is_some());

    // The profile absorbed the game and round-trips as JSON.
    let profile: serde_json::Value =
        serde_json::from_str(&handle.profile_json()).expect("profile is valid JSON");
    assert_eq!(profile["games_played"], 1);

    // Engine-only sessions make no LLM calls.
    let stats: serde_json::Value =
        serde_json::from_str(&handle.stats_json()).expect("stats are valid JSON");
    assert_eq!(stats["llm_calls"], 0);
}

#[test]
fn store_attach_resume_and_history_roundtrip() {
    let Some(stockfish) = stockfish_on_path() else {
        eprintln!("skipping: no stockfish on PATH");
        return;
    };
    let dir = std::env::temp_dir().join(format!("coach-ffi-store-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("smoke.db").to_string_lossy().into_owned();

    // Session 1: attach, judge a move, log a UI message, quit without
    // finishing — the game must stay open in the store.
    {
        let handle = CoachSessionHandle::new_engine_only(stockfish.clone(), None, None, false)
            .expect("engine-only session constructs");
        handle.attach_store(db.clone()).expect("attach_store");
        handle.log_feed("system".into(), false, "greeting".into());
        handle.judge_move("e4".into()).expect("judge_move");
        // Session dropped here: no finish_game.
    }

    // Session 2: resume must restore the same position and history.
    {
        let handle = CoachSessionHandle::new_engine_only(stockfish, None, None, false)
            .expect("second session constructs");
        let report_json = handle
            .resume_from_store(db.clone())
            .expect("resume_from_store")
            .expect("an open game to resume");
        let report: serde_json::Value = serde_json::from_str(&report_json).unwrap();
        assert_eq!(report["history_san"], serde_json::json!(["e4"]));
        assert_eq!(report["chat"][0]["text"], "greeting");
        assert_eq!(handle.history_san(), vec!["e4".to_string()]);
        assert!(handle.fen().contains(" b "));

        // Finish it; the free-function history surface must see it — read
        // concurrently while the session still holds its write connection.
        handle.finish_game(FfiGameResult::Win);
        let rows: serde_json::Value =
            serde_json::from_str(&coach_ffi::session::history_list(db.clone(), 10, 0).unwrap())
                .unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["result"], "win");
        let game_id = rows[0]["id"].as_i64().unwrap();

        let detail_json = coach_ffi::session::game_detail(db.clone(), game_id)
            .unwrap()
            .expect("game exists");
        let detail: serde_json::Value = serde_json::from_str(&detail_json).unwrap();
        assert_eq!(detail["moves"][0]["san"], "e4");
        assert_eq!(detail["chat"][0]["role"], "system");
        assert!(coach_ffi::session::game_detail(db.clone(), 999_999).unwrap().is_none());

        let stats: serde_json::Value =
            serde_json::from_str(&coach_ffi::session::store_stats(db.clone()).unwrap()).unwrap();
        assert_eq!(stats["games"], 1);
        assert_eq!(stats["wins"], 1);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn commentary_surface_engine_only() {
    let Some(stockfish) = stockfish_on_path() else {
        eprintln!("skipping: no stockfish on PATH");
        return;
    };

    let handle = CoachSessionHandle::new_engine_only(stockfish, None, None, false)
        .expect("engine-only session constructs");
    handle.set_commentary_style(FfiCommentaryStyle::Chatty);

    // Student move: with a NullBackend the Full path degrades to the canned
    // line, which cites the SAN.
    handle.judge_move("e4".into()).expect("judge_move");
    let reaction = handle
        .react_to_student_move()
        .expect("react_to_student_move")
        .expect("chatty always speaks");
    assert!(reaction.contains("e4"), "reaction cites the move: {reaction}");

    // No opponent move has landed yet — the policy has no context and must
    // stay silent, not error.
    assert_eq!(handle.react_to_opponent_move().expect("no context ok"), None);

    // Quiet style: opponent commentary is always silent.
    handle.set_commentary_style(FfiCommentaryStyle::Quiet);
    handle.judge_move("d4".into()).ok(); // it's Black's turn; ignore outcome
    assert_eq!(handle.react_to_opponent_move().expect("quiet is silent"), None);
}

#[test]
fn speak_san_and_version_exports() {
    assert_eq!(coach_ffi::speak_san("Nf3".into()), "knight to f3");
    assert!(!coach_ffi::version().is_empty());
}
