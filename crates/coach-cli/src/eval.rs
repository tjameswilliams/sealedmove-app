//! The eval harness: run scripted coaching scenarios through N backends side
//! by side, capture transcripts and cost stats, optionally score each
//! transcript with a judge model. Because `CoachModel` is a trait, adding a
//! backend to the comparison is a TOML stanza, not new plumbing.

use anyhow::{Context, Result};
use coach_core::coach::{CoachSession, Modality, MoveVerdict, SessionStats};
use coach_core::engine::UciEngine;
use coach_core::game::GameState;
use coach_core::llm::{
    build_backend, BackendConfig, Capabilities, CoachModel, CompletionRequest, ModelTier,
};
use coach_core::student::StudentProfile;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------- suite file format ----------

#[derive(Debug, Deserialize)]
pub struct Suite {
    /// Analyst engine binary. Default "stockfish".
    pub engine: Option<String>,
    pub backends: Vec<BackendSpec>,
    pub scenarios: Vec<Scenario>,
    /// Optional judge model; when present each transcript gets scored.
    pub judge: Option<BackendSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BackendSpec {
    pub name: String,
    pub base_url: String,
    pub model: String,
    /// Env var holding the API key (never the key itself — suites are
    /// checked into the repo).
    pub api_key_env: Option<String>,
    /// "full" (default) or "compact".
    pub tier: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub name: String,
    /// Starting FEN; defaults to the initial position.
    pub fen: Option<String>,
    /// The move the scripted student plays.
    pub student_move: String,
    /// What this scenario is probing — given to the judge as grading context.
    pub note: Option<String>,
    /// Optional follow-up question the student asks after the commentary.
    pub chat: Option<String>,
}

// ---------- results ----------

#[derive(Debug, Serialize)]
pub struct ScenarioResult {
    pub backend: String,
    pub scenario: String,
    pub verdict: Option<MoveVerdict>,
    pub commentary: Option<String>,
    pub chat_reply: Option<String>,
    pub stats: SessionStats,
    pub duration_ms: u128,
    pub error: Option<String>,
    pub judge: Option<JudgeScores>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeScores {
    pub groundedness: u8,
    pub tone: u8,
    pub level_fit: u8,
    pub brevity: u8,
    pub rationale: String,
}

impl JudgeScores {
    fn mean(&self) -> f32 {
        (self.groundedness + self.tone + self.level_fit + self.brevity) as f32 / 4.0
    }
}

// ---------- runner ----------

const SCENARIO_TIMEOUT: Duration = Duration::from_secs(240);

fn make_backend(spec: &BackendSpec) -> Arc<dyn CoachModel> {
    let api_key = spec
        .api_key_env
        .as_deref()
        .and_then(|var| std::env::var(var).ok());
    let tier = match spec.tier.as_deref() {
        Some("compact") => ModelTier::Compact,
        _ => ModelTier::Full,
    };
    build_backend(BackendConfig::OpenAiCompat {
        base_url: spec.base_url.clone(),
        api_key,
        model: spec.model.clone(),
        capabilities: Some(Capabilities {
            tier,
            ..Capabilities::default()
        }),
    })
}

pub async fn run(suite_path: &Path, report_path: &Path, json_path: &Path) -> Result<()> {
    let raw = std::fs::read_to_string(suite_path)
        .with_context(|| format!("cannot read suite file {}", suite_path.display()))?;
    let suite: Suite = toml::from_str(&raw).context("invalid suite file")?;
    let engine_path = suite.engine.clone().unwrap_or_else(|| "stockfish".into());

    let judge_model = suite.judge.as_ref().map(make_backend);
    let mut results: Vec<ScenarioResult> = Vec::new();

    for backend_spec in &suite.backends {
        for scenario in &suite.scenarios {
            eprintln!("▶ {} / {}", backend_spec.name, scenario.name);
            let started = Instant::now();
            let outcome = tokio::time::timeout(
                SCENARIO_TIMEOUT,
                run_scenario(&engine_path, backend_spec, scenario),
            )
            .await;
            let duration_ms = started.elapsed().as_millis();

            let mut result = match outcome {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => error_result(backend_spec, scenario, e.to_string()),
                Err(_) => error_result(
                    backend_spec,
                    scenario,
                    format!("timed out after {}s", SCENARIO_TIMEOUT.as_secs()),
                ),
            };
            result.duration_ms = duration_ms;

            if let (Some(judge), Some(commentary)) = (&judge_model, &result.commentary) {
                match score_with_judge(judge.as_ref(), scenario, result.verdict.as_ref(), commentary)
                    .await
                {
                    Ok(scores) => result.judge = Some(scores),
                    Err(e) => eprintln!("  judge failed: {e}"),
                }
            }
            results.push(result);
        }
    }

    let report = render_report(&results);
    std::fs::write(report_path, &report)?;
    std::fs::write(json_path, serde_json::to_string_pretty(&results)?)?;

    print_summary(&results);
    eprintln!(
        "\nreport: {} · raw results: {}",
        report_path.display(),
        json_path.display()
    );
    Ok(())
}

fn error_result(backend: &BackendSpec, scenario: &Scenario, error: String) -> ScenarioResult {
    ScenarioResult {
        backend: backend.name.clone(),
        scenario: scenario.name.clone(),
        verdict: None,
        commentary: None,
        chat_reply: None,
        stats: SessionStats::default(),
        duration_ms: 0,
        error: Some(error),
        judge: None,
    }
}

async fn run_scenario(
    engine_path: &str,
    backend_spec: &BackendSpec,
    scenario: &Scenario,
) -> Result<ScenarioResult> {
    let game = match &scenario.fen {
        Some(f) => GameState::from_fen(f)?,
        None => GameState::new(),
    };
    let analyst = UciEngine::spawn(engine_path).await?;
    let mut session = CoachSession::new(
        make_backend(backend_spec),
        analyst,
        game,
        StudentProfile::default(),
        Modality::Text,
    );

    let reply = session.comment_on_move(&scenario.student_move).await?;
    let chat_reply = match &scenario.chat {
        Some(question) => Some(session.chat(question).await?),
        None => None,
    };

    Ok(ScenarioResult {
        backend: backend_spec.name.clone(),
        scenario: scenario.name.clone(),
        verdict: Some(reply.verdict),
        commentary: Some(reply.commentary),
        chat_reply,
        stats: session.stats,
        duration_ms: 0, // caller fills in
        error: None,
        judge: None,
    })
}

async fn score_with_judge(
    judge: &dyn CoachModel,
    scenario: &Scenario,
    verdict: Option<&MoveVerdict>,
    commentary: &str,
) -> Result<JudgeScores> {
    let verdict_json = verdict
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .unwrap_or_else(|| "unknown".into());
    let note = scenario.note.as_deref().unwrap_or("(none)");

    let prompt = format!(
        "You are grading one piece of chess-coach commentary.\n\n\
         Engine facts (ground truth): {verdict_json}\n\
         Scenario intent: {note}\n\
         Coach commentary being graded:\n\"{commentary}\"\n\n\
         Score each 1-5:\n\
         - groundedness: makes no chess claims beyond the engine facts; never contradicts them\n\
         - tone: encouraging and respectful; a blunder is treated as a learning moment\n\
         - level_fit: pitched to a beginner/intermediate student (~800-1200)\n\
         - brevity: 2-4 sentences, one idea, no lecture\n\n\
         Reply with ONLY a JSON object:\n\
         {{\"groundedness\":n,\"tone\":n,\"level_fit\":n,\"brevity\":n,\"rationale\":\"one sentence\"}}"
    );

    let request = CompletionRequest {
        system: "You are a strict, terse evaluator. Output only JSON.".into(),
        messages: vec![coach_core::llm::Message::user(prompt)],
        tools: vec![],
    };
    let response = judge.complete(&request).await?;
    let text = response.text.unwrap_or_default();

    // Lenient extraction: first '{' to last '}'.
    let start = text.find('{').context("judge returned no JSON")?;
    let end = text.rfind('}').context("judge returned no JSON")?;
    let scores: JudgeScores =
        serde_json::from_str(&text[start..=end]).context("judge JSON did not match schema")?;
    Ok(scores)
}

// ---------- output ----------

fn print_summary(results: &[ScenarioResult]) {
    println!(
        "\n{:<18} {:<22} {:<12} {:>7} {:>6} {:>6} {:>7} {:>7}",
        "backend", "scenario", "judgment", "cp", "llm", "tools", "secs", "judge"
    );
    for r in results {
        let (judgment, cp) = r
            .verdict
            .as_ref()
            .map(|v| (format!("{:?}", v.judgment), v.cp_loss.to_string()))
            .unwrap_or_else(|| ("—".into(), "—".into()));
        let judge = r
            .judge
            .as_ref()
            .map(|j| format!("{:.2}", j.mean()))
            .unwrap_or_else(|| "—".into());
        let status = r.error.as_deref().map(|_| " ✗ ERROR").unwrap_or("");
        println!(
            "{:<18} {:<22} {:<12} {:>7} {:>6} {:>6} {:>7.1} {:>7}{}",
            r.backend,
            r.scenario,
            judgment,
            cp,
            r.stats.llm_calls,
            r.stats.tool_calls,
            r.duration_ms as f64 / 1000.0,
            judge,
            status
        );
    }
}

fn render_report(results: &[ScenarioResult]) -> String {
    let mut md = String::from("# Coach eval report\n\n## Summary\n\n");
    md.push_str("| backend | scenario | judgment | cp loss | llm calls | tool calls | tokens in/out | secs | judge mean |\n");
    md.push_str("|---|---|---|---|---|---|---|---|---|\n");
    for r in results {
        let (judgment, cp) = r
            .verdict
            .as_ref()
            .map(|v| (format!("{:?}", v.judgment), v.cp_loss.to_string()))
            .unwrap_or_else(|| ("error".into(), "—".into()));
        let judge = r
            .judge
            .as_ref()
            .map(|j| format!("{:.2}", j.mean()))
            .unwrap_or_else(|| "—".into());
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {}/{} | {:.1} | {} |\n",
            r.backend,
            r.scenario,
            judgment,
            cp,
            r.stats.llm_calls,
            r.stats.tool_calls,
            r.stats.input_tokens,
            r.stats.output_tokens,
            r.duration_ms as f64 / 1000.0,
            judge
        ));
    }

    md.push_str("\n## Transcripts\n");
    for r in results {
        md.push_str(&format!("\n### {} · {}\n\n", r.backend, r.scenario));
        if let Some(e) = &r.error {
            md.push_str(&format!("**ERROR:** {e}\n"));
            continue;
        }
        if let Some(v) = &r.verdict {
            md.push_str(&format!(
                "**Engine:** {} — {:?}, cp loss {}, best was `{}`\n\n",
                v.played_san, v.judgment, v.cp_loss, v.best_move_uci
            ));
        }
        if let Some(c) = &r.commentary {
            md.push_str(&format!("**Coach:** {c}\n\n"));
        }
        if let Some(c) = &r.chat_reply {
            md.push_str(&format!("**Follow-up reply:** {c}\n\n"));
        }
        if let Some(j) = &r.judge {
            md.push_str(&format!(
                "**Judge:** groundedness {}, tone {}, level-fit {}, brevity {} — {}\n",
                j.groundedness, j.tone, j.level_fit, j.brevity, j.rationale
            ));
        }
    }
    md
}
