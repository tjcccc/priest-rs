pub mod context_builder;
pub mod engine;
pub mod errors;
pub mod profile;
pub mod providers;
pub mod schema;
pub mod session;
pub mod tool_loop;

pub use engine::{PriestEngine, PriestStreamEvent, SPEC_VERSION};
pub use errors::PriestError;
pub use profile::{
    default_profile::built_in_default, filesystem_loader::FilesystemProfileLoader,
    loader::ProfileLoader, model::Profile,
};
pub use providers::{
    adapter::{AdapterCallOptions, AdapterResult, ProviderAdapter},
    anthropic::AnthropicProvider,
    ollama::OllamaProvider,
    openai_compat::OpenAICompatProvider,
};
pub use schema::{
    config::PriestConfig,
    request::{ImageInput, OutputSpec, PriestRequest, SessionRef},
    tools::{parse_tool_arguments, ToolCall, ToolChoice, ToolDefinition, ToolExchangeTurn},
    response::{ExecutionInfo, PriestErrorModel, PriestResponse, SessionInfo, UsageInfo},
};
pub use session::{
    in_memory::InMemorySessionStore,
    model::{Session, Turn},
    sqlite::SqliteSessionStore,
    store::SessionStore,
};
pub use tool_loop::{
    run_with_tools, ApprovalDecision, ToolExecutionResult, ToolExecutor, ToolLoopResult,
};
