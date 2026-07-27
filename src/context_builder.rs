use crate::profile::model::Profile;
use crate::schema::request::PriestRequest;
use crate::schema::tools::{ToolCall, ToolExchangeTurn};
use crate::schema::reasoning::ReasoningInfo;
use crate::session::model::Session;

pub const FORMAT_INSTRUCTION_JSON: &str =
    "Respond only with valid JSON. No prose, no markdown code fences.";
pub const FORMAT_INSTRUCTION_XML: &str =
    "Respond only with valid XML. No prose, no markdown code fences.";
pub const FORMAT_INSTRUCTION_CODE: &str =
    "Respond only with code. No prose, no markdown code fences around it.";

#[derive(Debug, Clone, Default)]
pub struct Message {
    pub role: String,
    pub content: String,
    /// Tool calls made by the model. Assistant role only (spec 2.4.0).
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Id of the tool call this message answers. Tool role only.
    pub tool_call_id: Option<String>,
    /// Tool name. Tool role only.
    pub name: Option<String>,
    /// Safe reasoning state on assistant tool-call turns (spec 2.8.0).
    pub reasoning: Option<ReasoningInfo>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
            ..Default::default()
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
            ..Default::default()
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
            ..Default::default()
        }
    }
}

pub fn build_messages(
    request: &PriestRequest,
    profile: &Profile,
    session: Option<&Session>,
) -> Vec<Message> {
    let max_system_chars = request.config.max_system_chars;

    // Compaction summary (spec 2.5.0): when the session has been compacted, the
    // summary stands in for the folded-away leading turns, which are skipped below.
    let compaction = session.map(|s| s.get_compaction());
    let conversation_summary = compaction.as_ref().and_then(|c| c.summary.clone());
    let summarized_through = compaction.as_ref().map(|c| c.summarized_through).unwrap_or(0);

    // Step 1 — normalize profile memories
    let profile_memories: Vec<String> = profile
        .memories
        .iter()
        .filter_map(|m| {
            let s = m.trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        })
        .collect();

    // Step 2 — deduplicate dynamic memory
    let mut seen: std::collections::HashSet<String> = profile_memories.iter().cloned().collect();
    let mut dynamic_memory: Vec<String> = vec![];
    for entry in &request.memory {
        let stripped = entry.trim().to_string();
        if stripped.is_empty() {
            continue;
        }
        if seen.contains(&stripped) {
            continue;
        }
        seen.insert(stripped.clone());
        dynamic_memory.push(stripped);
    }

    // Step 3 — trim to budget (only when max_system_chars is set)
    let (dynamic_memory, profile_memories) = if let Some(budget) = max_system_chars {
        let mut dyn_m = dynamic_memory;
        let mut prof_m = profile_memories;
        while assemble_system_content(request, profile, &prof_m, &dyn_m, conversation_summary.as_deref()).len() > budget {
            if !dyn_m.is_empty() {
                dyn_m.pop();
            } else if !prof_m.is_empty() {
                prof_m.pop();
            } else {
                break;
            }
        }
        (dyn_m, prof_m)
    } else {
        (dynamic_memory, profile_memories)
    };

    let system_content =
        assemble_system_content(request, profile, &profile_memories, &dynamic_memory, conversation_summary.as_deref());

    // Step 5 — build message list
    let mut messages: Vec<Message> = vec![];

    if !system_content.is_empty() {
        messages.push(Message::system(system_content));
    }

    if let Some(sess) = session {
        // Replay window (spec 2.5.0 + 2.6.0). Skip turns folded into the summary;
        // optionally cap to the last N turns (session_context_turns).
        let mut window_start = summarized_through;
        if let Some(n) = request.config.session_context_turns {
            window_start = summarized_through.max(sess.turns.len().saturating_sub(n));
            // Snap down to a user turn so an odd-sized window never opens the replay
            // on an orphan assistant reply. Floored by summarized_through.
            while window_start > summarized_through
                && window_start < sess.turns.len()
                && sess.turns[window_start].role != "user"
            {
                window_start -= 1;
            }
        }
        for turn in &sess.turns[window_start..] {
            messages.push(Message {
                role: turn.role.clone(),
                content: turn.content.clone(),
                ..Default::default()
            });
        }
    }

    let mut user_parts = vec![request.prompt.clone()];
    for ctx in &request.user_context {
        if !ctx.is_empty() {
            user_parts.push(ctx.clone());
        }
    }
    messages.push(Message::user(user_parts.join("\n\n")));

    // Tool loop history for the current turn (spec 2.4.0). Appended after the
    // user message, never persisted in sessions.
    for turn in &request.tool_exchange {
        match turn {
            ToolExchangeTurn::Assistant { text, tool_calls, reasoning } => {
                messages.push(Message {
                    role: "assistant".into(),
                    content: text.clone().unwrap_or_default(),
                    tool_calls: Some(tool_calls.clone()),
                    reasoning: reasoning.clone(),
                    ..Default::default()
                });
            }
            ToolExchangeTurn::ToolResult { tool_call_id, name, content, .. } => {
                messages.push(Message {
                    role: "tool".into(),
                    content: content.clone(),
                    tool_call_id: Some(tool_call_id.clone()),
                    name: Some(name.clone()),
                    ..Default::default()
                });
            }
        }
    }

    messages
}

fn assemble_system_content(
    request: &PriestRequest,
    profile: &Profile,
    profile_memories: &[String],
    dynamic_memory: &[String],
    conversation_summary: Option<&str>,
) -> String {
    let mut parts: Vec<String> = vec![];

    for ctx in &request.context {
        if !ctx.is_empty() {
            parts.push(ctx.clone());
        }
    }

    if !profile.rules.is_empty() {
        parts.push(profile.rules.clone());
    }

    if !profile.identity.is_empty() {
        parts.push(profile.identity.clone());
    }

    if !profile.custom.is_empty() {
        parts.push(profile.custom.clone());
    }

    if !profile_memories.is_empty() {
        parts.push(format!(
            "## Loaded Memories\n\n{}",
            profile_memories.join("\n")
        ));
    }

    if !dynamic_memory.is_empty() {
        parts.push(format!("## Memory\n\n{}", dynamic_memory.join("\n")));
    }

    // Compaction summary (spec 2.5.0): after memory, before the format instruction.
    if let Some(summary) = conversation_summary {
        let trimmed = summary.trim();
        if !trimmed.is_empty() {
            parts.push(format!("## Conversation so far (summary)\n\n{trimmed}"));
        }
    }

    if let Some(ref fmt) = request.output.prompt_format {
        let instruction = match fmt.as_str() {
            "json" => FORMAT_INSTRUCTION_JSON,
            "xml" => FORMAT_INSTRUCTION_XML,
            "code" => FORMAT_INSTRUCTION_CODE,
            _ => "",
        };
        if !instruction.is_empty() {
            parts.push(instruction.to_string());
        }
    }

    parts.join("\n\n")
}
