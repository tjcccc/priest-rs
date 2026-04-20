use crate::profile::model::Profile;
use crate::schema::request::PriestRequest;
use crate::session::model::Session;

pub const FORMAT_INSTRUCTION_JSON: &str =
    "Respond only with valid JSON. No prose, no markdown code fences.";
pub const FORMAT_INSTRUCTION_XML: &str =
    "Respond only with valid XML. No prose, no markdown code fences.";
pub const FORMAT_INSTRUCTION_CODE: &str =
    "Respond only with code. No prose, no markdown code fences around it.";

#[derive(Debug, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self { Self { role: "system".into(), content: content.into() } }
    pub fn user(content: impl Into<String>) -> Self   { Self { role: "user".into(),   content: content.into() } }
    pub fn assistant(content: impl Into<String>) -> Self { Self { role: "assistant".into(), content: content.into() } }
}

pub fn build_messages(
    request: &PriestRequest,
    profile: &Profile,
    session: Option<&Session>,
) -> Vec<Message> {
    let max_system_chars = request.config.max_system_chars;

    // Step 1 — normalize profile memories
    let profile_memories: Vec<String> = profile.memories
        .iter()
        .filter_map(|m| {
            let s = m.trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        })
        .collect();

    // Step 2 — deduplicate dynamic memory
    let mut seen: std::collections::HashSet<String> = profile_memories.iter().cloned().collect();
    let mut dynamic_memory: Vec<String> = vec![];
    for entry in &request.memory {
        let stripped = entry.trim().to_string();
        if stripped.is_empty() { continue; }
        if seen.contains(&stripped) { continue; }
        seen.insert(stripped.clone());
        dynamic_memory.push(stripped);
    }

    // Step 3 — trim to budget (only when max_system_chars is set)
    let (dynamic_memory, profile_memories) = if let Some(budget) = max_system_chars {
        let mut dyn_m = dynamic_memory;
        let mut prof_m = profile_memories;
        while assemble_system_content(request, profile, &prof_m, &dyn_m).len() > budget {
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

    let system_content = assemble_system_content(request, profile, &profile_memories, &dynamic_memory);

    // Step 5 — build message list
    let mut messages: Vec<Message> = vec![];

    if !system_content.is_empty() {
        messages.push(Message::system(system_content));
    }

    if let Some(sess) = session {
        for turn in &sess.turns {
            messages.push(Message { role: turn.role.clone(), content: turn.content.clone() });
        }
    }

    let mut user_parts = vec![request.prompt.clone()];
    for ctx in &request.user_context {
        if !ctx.is_empty() {
            user_parts.push(ctx.clone());
        }
    }
    messages.push(Message::user(user_parts.join("\n\n")));

    messages
}

fn assemble_system_content(
    request: &PriestRequest,
    profile: &Profile,
    profile_memories: &[String],
    dynamic_memory: &[String],
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
        parts.push(format!("## Loaded Memories\n\n{}", profile_memories.join("\n")));
    }

    if !dynamic_memory.is_empty() {
        parts.push(format!("## Memory\n\n{}", dynamic_memory.join("\n")));
    }

    if let Some(ref fmt) = request.output.prompt_format {
        let instruction = match fmt.as_str() {
            "json" => FORMAT_INSTRUCTION_JSON,
            "xml"  => FORMAT_INSTRUCTION_XML,
            "code" => FORMAT_INSTRUCTION_CODE,
            _      => "",
        };
        if !instruction.is_empty() {
            parts.push(instruction.to_string());
        }
    }

    parts.join("\n\n")
}
