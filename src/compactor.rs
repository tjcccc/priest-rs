//! Conversation compaction primitives (spec 2.5.0).
//!
//! Long sessions replay their full turn history on every call, so input cost
//! grows linearly per turn and quadratically over a session. Compaction folds
//! the older turns into a running summary and replays only a recent tail. It is
//! non-destructive: raw turns stay in the store; only the *replayed view*
//! shrinks. The summary lives in session metadata (see `session::model`).

use crate::context_builder::Message;
use crate::session::model::Turn;

/// Compact when the previous turn's input usage exceeds this fraction of the budget.
pub const COMPACTION_TRIGGER_RATIO: f64 = 0.8;
/// Most-recent turns kept verbatim; older turns fold into the summary.
pub const DEFAULT_COMPACTION_KEEP_TURNS: usize = 6;
/// Output cap for the summary-generation call (keeps the summary itself bounded).
pub const SUMMARY_MAX_OUTPUT_TOKENS: u32 = 1024;

/// A planned compaction round.
pub struct CompactionPlan {
    /// Turns to fold this round (after what's already summarized, before the tail).
    pub to_summarize: Vec<Turn>,
    /// Index into session.turns the new summary will cover up to.
    pub summarized_through: usize,
}

/// Whether the previous turn's input size warrants compaction. Off when no budget is set.
pub fn should_compact(last_input_tokens: Option<u32>, max_context_tokens: Option<u32>) -> bool {
    let Some(budget) = max_context_tokens else { return false };
    if budget == 0 {
        return false;
    }
    let Some(last) = last_input_tokens else { return false };
    f64::from(last) > f64::from(budget) * COMPACTION_TRIGGER_RATIO
}

/// Plan a compaction round: fold every turn after what's already summarized and
/// before the kept tail. Returns None when there is nothing new to fold.
pub fn plan_compaction(
    turns: &[Turn],
    already_summarized_through: usize,
    keep_turns: usize,
) -> Option<CompactionPlan> {
    let tail_start = turns.len().saturating_sub(keep_turns);
    if tail_start <= already_summarized_through {
        return None;
    }
    Some(CompactionPlan {
        to_summarize: turns[already_summarized_through..tail_start].to_vec(),
        summarized_through: tail_start,
    })
}

const SUMMARY_SYSTEM: &str = concat!(
    "You compress prior conversation into a compact running summary so the assistant can continue without the full transcript. ",
    "Preserve the user's goals and constraints, decisions made, facts established within the conversation, and open or unresolved threads. ",
    "Durable user facts are stored separately as memory — do not re-list them. Capture the conversation's trajectory and the context needed to continue it. ",
    "Write a tight synopsis, not a turn-by-turn log. When an earlier summary is provided, merge the new turns into it and return a single updated summary with no preamble.",
);

/// Build the messages for the summary-generation call (existing summary + new turns → one updated summary).
pub fn build_summary_messages(existing_summary: Option<&str>, to_summarize: &[Turn]) -> Vec<Message> {
    let transcript = to_summarize
        .iter()
        .map(|t| format!("{}: {}", t.role.to_uppercase(), t.content))
        .collect::<Vec<_>>()
        .join("\n\n");
    let user = match existing_summary.map(str::trim).filter(|s| !s.is_empty()) {
        Some(prior) => format!(
            "Existing summary so far:\n\n{prior}\n\n---\n\nNew conversation turns to fold in:\n\n{transcript}\n\n---\n\nReturn one updated summary."
        ),
        None => format!("Conversation turns to summarize:\n\n{transcript}\n\n---\n\nReturn the summary."),
    };
    vec![Message::system(SUMMARY_SYSTEM), Message::user(user)]
}
