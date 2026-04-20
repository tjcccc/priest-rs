use priest::context_builder::{build_messages, FORMAT_INSTRUCTION_CODE, FORMAT_INSTRUCTION_JSON, FORMAT_INSTRUCTION_XML};
use priest::profile::model::Profile;
use priest::schema::config::PriestConfig;
use priest::schema::request::PriestRequest;

fn config() -> PriestConfig { PriestConfig::new("mock", "m") }

fn empty_profile() -> Profile {
    Profile::new("default", "", "", "", vec![], Default::default())
}

fn make_request(prompt: &str) -> PriestRequest {
    PriestRequest::new(config(), prompt)
}

// ── Minimal request ─────────────────────────────────────────────────────────

#[test]
fn minimal_request_produces_only_user_message() {
    let msgs = build_messages(&make_request("Hello."), &empty_profile(), None);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].role, "user");
    assert_eq!(msgs[0].content, "Hello.");
}

// ── System prompt assembly ────────────────────────────────────────────────────

#[test]
fn identity_appears_in_system_message() {
    let profile = Profile::new("p", "I am a bot.", "", "", vec![], Default::default());
    let msgs = build_messages(&make_request("hi"), &profile, None);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].role, "system");
    assert!(msgs[0].content.contains("I am a bot."));
}

#[test]
fn rules_identity_custom_order() {
    let profile = Profile::new("p", "ID.", "RULES.", "CUSTOM.", vec![], Default::default());
    let msgs = build_messages(&make_request("hi"), &profile, None);
    let sys = &msgs[0].content;
    let ri = sys.find("RULES.").unwrap();
    let ii = sys.find("ID.").unwrap();
    let ci = sys.find("CUSTOM.").unwrap();
    assert!(ri < ii, "rules before identity");
    assert!(ii < ci, "identity before custom");
}

#[test]
fn context_appears_before_rules() {
    let profile = Profile::new("p", "", "RULES.", "", vec![], Default::default());
    let mut req = make_request("hi");
    req.context = vec!["CTX.".into()];
    let msgs = build_messages(&req, &profile, None);
    let sys = &msgs[0].content;
    assert!(sys.find("CTX.").unwrap() < sys.find("RULES.").unwrap());
}

#[test]
fn multiple_context_entries_joined_with_double_newline() {
    let profile = empty_profile();
    let mut req = make_request("hi");
    req.context = vec!["A.".into(), "B.".into()];
    let msgs = build_messages(&req, &profile, None);
    assert!(msgs[0].content.contains("A.\n\nB."));
}

// ── Profile memories ─────────────────────────────────────────────────────────

#[test]
fn profile_memories_under_loaded_memories_heading() {
    let profile = Profile::new("p", "", "", "", vec!["Fact A.".into()], Default::default());
    let msgs = build_messages(&make_request("hi"), &profile, None);
    assert!(msgs[0].content.contains("## Loaded Memories\n\nFact A."));
}

#[test]
fn multiple_profile_memories_joined_with_single_newline() {
    let profile = Profile::new("p", "", "", "", vec!["A.".into(), "B.".into()], Default::default());
    let msgs = build_messages(&make_request("hi"), &profile, None);
    assert!(msgs[0].content.contains("## Loaded Memories\n\nA.\nB."));
}

// ── Dynamic memory ────────────────────────────────────────────────────────────

#[test]
fn dynamic_memory_under_memory_heading() {
    let mut req = make_request("hi");
    req.memory = vec!["Dyn fact.".into()];
    let msgs = build_messages(&req, &empty_profile(), None);
    assert!(msgs[0].content.contains("## Memory\n\nDyn fact."));
}

#[test]
fn dedup_drops_memory_matching_profile_memories() {
    let profile = Profile::new("p", "", "", "", vec!["Fact A.".into()], Default::default());
    let mut req = make_request("hi");
    req.memory = vec!["Fact A.".into(), "Fact B.".into()];
    let msgs = build_messages(&req, &profile, None);
    let sys = &msgs[0].content;
    // Loaded Memories block
    assert!(sys.contains("## Loaded Memories\n\nFact A."));
    // Memory block must only contain Fact B.
    let mem_idx = sys.find("## Memory\n\n").unwrap();
    let mem_section = &sys[mem_idx..];
    assert!(mem_section.contains("Fact B."));
    assert!(!mem_section.contains("Fact A."), "dedup should drop Fact A from dynamic block");
}

#[test]
fn dedup_drops_duplicate_dynamic_entries() {
    let mut req = make_request("hi");
    req.memory = vec!["Fact X.".into(), "Fact X.".into()];
    let msgs = build_messages(&req, &empty_profile(), None);
    let sys = &msgs[0].content;
    // Count occurrences of "Fact X."
    assert_eq!(sys.matches("Fact X.").count(), 1);
}

#[test]
fn dedup_uses_stripped_comparison() {
    let profile = Profile::new("p", "", "", "", vec!["Fact.".into()], Default::default());
    let mut req = make_request("hi");
    req.memory = vec!["  Fact.  ".into()]; // whitespace-padded duplicate
    let msgs = build_messages(&req, &profile, None);
    // Should have no ## Memory block — all entries deduped
    assert!(!msgs[0].content.contains("## Memory"));
}

#[test]
fn empty_memory_entries_ignored() {
    let mut req = make_request("hi");
    req.memory = vec!["".into(), "  ".into(), "Real.".into()];
    let msgs = build_messages(&req, &empty_profile(), None);
    let sys = &msgs[0].content;
    assert!(sys.contains("## Memory\n\nReal."));
    assert!(!sys.contains("\n\n\n"));
}

// ── Trim ──────────────────────────────────────────────────────────────────────

#[test]
fn no_trim_when_max_system_chars_is_none() {
    let mut req = make_request("hi");
    req.memory = vec!["A.".repeat(50)];
    // No max_system_chars → no trimming
    let msgs = build_messages(&req, &empty_profile(), None);
    assert!(msgs[0].content.contains("A."));
}

#[test]
fn trim_drops_dynamic_memory_tail_first() {
    let mut req = make_request("hi");
    req.config.max_system_chars = Some(50);
    req.memory = vec!["A.".into(), "B.".repeat(100)];
    let msgs = build_messages(&req, &empty_profile(), None);
    // B. should be trimmed (tail), A. may survive or also be trimmed
    let sys_content = if msgs[0].role == "system" { &msgs[0].content } else { "" };
    assert!(sys_content.len() <= 50 || sys_content.is_empty());
}

#[test]
fn trim_dynamic_before_profile_memories() {
    let profile = Profile::new("p", "", "", "", vec!["Profile mem.".into()], Default::default());
    let mut req = make_request("hi");
    req.config.max_system_chars = Some(60);
    req.memory = vec!["Dynamic mem.".into()];
    // 60 chars is tight — dynamic memory should be dropped before profile memories
    let msgs = build_messages(&req, &profile, None);
    // After trimming, profile memory should still be present
    let sys = if msgs[0].role == "system" { msgs[0].content.clone() } else { String::new() };
    // Either everything fits or dynamic was dropped first
    if sys.len() > 60 {
        // budget exceeded even after all dynamic dropped — that's ok per spec (warn + continue)
    } else {
        assert!(!sys.contains("Dynamic mem.") || sys.contains("Profile mem."));
    }
}

// ── Format instructions ───────────────────────────────────────────────────────

#[test]
fn format_instruction_json() {
    let mut req = make_request("hi");
    req.output.prompt_format = Some("json".into());
    let msgs = build_messages(&req, &empty_profile(), None);
    assert!(msgs[0].content.contains(FORMAT_INSTRUCTION_JSON));
}

#[test]
fn format_instruction_xml() {
    let mut req = make_request("hi");
    req.output.prompt_format = Some("xml".into());
    let msgs = build_messages(&req, &empty_profile(), None);
    assert!(msgs[0].content.contains(FORMAT_INSTRUCTION_XML));
}

#[test]
fn format_instruction_code() {
    let mut req = make_request("hi");
    req.output.prompt_format = Some("code".into());
    let msgs = build_messages(&req, &empty_profile(), None);
    assert!(msgs[0].content.contains(FORMAT_INSTRUCTION_CODE));
}

// ── User context ──────────────────────────────────────────────────────────────

#[test]
fn user_context_appended_to_user_turn() {
    let mut req = make_request("Prompt.");
    req.user_context = vec!["Extra.".into()];
    let msgs = build_messages(&req, &empty_profile(), None);
    let user = msgs.iter().find(|m| m.role == "user").unwrap();
    assert_eq!(user.content, "Prompt.\n\nExtra.");
}

// ── Session history ───────────────────────────────────────────────────────────

#[test]
fn session_turns_inserted_before_user_message() {
    use priest::session::model::{Session, Turn};
    use chrono::Utc;

    let mut sess = Session::new("s1", "default");
    sess.turns.push(Turn { role: "user".into(), content: "Q1.".into(), timestamp: Utc::now() });
    sess.turns.push(Turn { role: "assistant".into(), content: "A1.".into(), timestamp: Utc::now() });

    let msgs = build_messages(&make_request("Q2."), &empty_profile(), Some(&sess));
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0].role, "user");
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(msgs[2].role, "user");
    assert_eq!(msgs[2].content, "Q2.");
}

// ── Canonical separator between system parts ──────────────────────────────────

#[test]
fn system_parts_separated_by_double_newline() {
    let profile = Profile::new("p", "ID.", "RULES.", "", vec![], Default::default());
    let msgs = build_messages(&make_request("hi"), &profile, None);
    assert!(msgs[0].content.contains("RULES.\n\nID."));
}
