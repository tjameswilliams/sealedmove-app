//! Tool schemas exposed to the LLM. These are the coach's *only* source of
//! board facts — every schema description reinforces that contract.

use crate::llm::ToolDef;
use serde_json::json;

/// Full tool set for capable models.
pub fn full_toolset() -> Vec<ToolDef> {
    vec![
        evaluate_position(),
        classify_last_move(),
        lookup_opening(),
        detect_motifs(),
        get_student_profile(),
        update_student_profile(),
    ]
}

/// Reduced set for compact backends (on-device / small local models):
/// fewer tools, simpler decisions, smaller schemas in the context window.
pub fn compact_toolset() -> Vec<ToolDef> {
    vec![classify_last_move(), get_student_profile()]
}

fn evaluate_position() -> ToolDef {
    ToolDef {
        name: "evaluate_position".into(),
        description: "Run the analysis engine on the current position and return the top \
                      lines with evaluations. Call this before making any claim about what \
                      the best move is or how good the position is — never assess a position \
                      from your own reading of the board."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "multipv": {
                    "type": "integer",
                    "description": "How many top lines to return (1-5).",
                    "minimum": 1,
                    "maximum": 5
                },
                "depth": {
                    "type": "integer",
                    "description": "Search depth. Default 16; use up to 22 for critical positions.",
                    "minimum": 8,
                    "maximum": 22
                }
            },
            "required": []
        }),
    }
}

fn classify_last_move() -> ToolDef {
    ToolDef {
        name: "classify_last_move".into(),
        description: "Get the engine's verdict on the student's most recent move: centipawn \
                      loss, classification (best/excellent/good/inaccuracy/mistake/blunder), \
                      what the best move was, and the refutation line if the move was bad. \
                      Call this before reacting to a move."
            .into(),
        parameters: json!({ "type": "object", "properties": {}, "required": [] }),
    }
}

fn lookup_opening() -> ToolDef {
    ToolDef {
        name: "lookup_opening".into(),
        description: "Identify the opening being played from the game's move history. \
                      Returns the ECO code and name, or null if the game left known theory. \
                      Call this before naming any opening."
            .into(),
        parameters: json!({ "type": "object", "properties": {}, "required": [] }),
    }
}

fn detect_motifs() -> ToolDef {
    ToolDef {
        name: "detect_motifs".into(),
        description: "Scan the current position for tactical motifs (hanging pieces, forks, \
                      pins, skewers, back-rank weaknesses, available discovered attacks). \
                      Each result names the squares involved. Call this before pointing out \
                      any tactic — never claim a tactic exists from your own reading."
            .into(),
        parameters: json!({ "type": "object", "properties": {}, "required": [] }),
    }
}

fn get_student_profile() -> ToolDef {
    ToolDef {
        name: "get_student_profile".into(),
        description: "Read the student's learning profile: estimated rating, concepts \
                      already taught and their mastery level, and prior coach notes. Use it \
                      to pitch explanations at the right level and avoid re-teaching."
            .into(),
        parameters: json!({ "type": "object", "properties": {}, "required": [] }),
    }
}

fn update_student_profile() -> ToolDef {
    ToolDef {
        name: "update_student_profile".into(),
        description: "Record a teaching event or observation in the student's profile. Use \
                      status 'taught' when you explain a concept, 'demonstrated' when the \
                      student successfully uses one in play."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "concept": {
                    "type": "string",
                    "description": "Stable concept slug, e.g. 'fork', 'pin', 'back_rank_mate', 'opening:ruy_lopez'."
                },
                "status": {
                    "type": "string",
                    "enum": ["taught", "demonstrated", "mastered"]
                },
                "note": {
                    "type": "string",
                    "description": "Optional short observation about the student."
                }
            },
            "required": ["concept", "status"]
        }),
    }
}
