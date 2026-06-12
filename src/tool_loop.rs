//! Generic caller-executes tool loop (spec 2.4.0, behavior/tool-calling.md).

use async_trait::async_trait;

use crate::engine::PriestEngine;
use crate::errors::PriestError;
use crate::schema::request::PriestRequest;
use crate::schema::response::PriestResponse;
use crate::schema::tools::{ToolCall, ToolExchangeTurn};

const DEFAULT_MAX_ITERATIONS: usize = 10;

#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub struct ApprovalDecision {
    pub approved: bool,
    pub reason: Option<String>,
}

/// Executes tool calls for `run_with_tools`. `approve` is the optional gate
/// before each execution; the default approves everything. Errors should be
/// returned as content with `is_error`, not panicked.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &ToolCall) -> ToolExecutionResult;

    async fn approve(&self, _call: &ToolCall) -> ApprovalDecision {
        ApprovalDecision { approved: true, reason: None }
    }
}

#[derive(Debug)]
pub struct ToolLoopResult {
    /// The final response — the first one without tool calls, or the last
    /// iteration's response when the cap was hit or an error occurred.
    pub response: PriestResponse,
    /// Full tool exchange trace accumulated across iterations.
    pub exchange: Vec<ToolExchangeTurn>,
    /// True when the loop stopped because the iteration cap was reached.
    pub iteration_limit_reached: bool,
}

/// Run the request, execute tool calls through the caller-supplied executor,
/// replay results via `tool_exchange`, and repeat until the model answers
/// without tool calls or the iteration cap is hit. The library never chooses
/// or sandboxes tools — policy belongs to the caller.
pub async fn run_with_tools(
    engine: &PriestEngine,
    request: PriestRequest,
    executor: &dyn ToolExecutor,
    max_iterations: Option<usize>,
) -> Result<ToolLoopResult, PriestError> {
    let max_iterations = max_iterations.unwrap_or(DEFAULT_MAX_ITERATIONS).max(1);
    let mut exchange: Vec<ToolExchangeTurn> = request.tool_exchange.clone();

    let mut response: Option<PriestResponse> = None;
    for _ in 0..max_iterations {
        let mut iteration_request = request.clone();
        iteration_request.tool_exchange = exchange.clone();
        let current = engine.run(iteration_request).await?;

        let Some(calls) = current.tool_calls.clone().filter(|c| !c.is_empty() && current.error.is_none()) else {
            return Ok(ToolLoopResult {
                response: current,
                exchange,
                iteration_limit_reached: false,
            });
        };

        exchange.push(ToolExchangeTurn::Assistant {
            text: current.text.clone(),
            tool_calls: calls.clone(),
        });
        for call in &calls {
            let decision = executor.approve(call).await;
            if !decision.approved {
                let reason = decision
                    .reason
                    .map(|r| format!(": {r}"))
                    .unwrap_or_else(|| ".".to_string());
                exchange.push(ToolExchangeTurn::ToolResult {
                    tool_call_id: call.id.clone(),
                    name: call.name.clone(),
                    content: format!("Tool call denied by the caller{reason}"),
                    is_error: Some(true),
                });
                continue;
            }
            let result = executor.execute(call).await;
            exchange.push(ToolExchangeTurn::ToolResult {
                tool_call_id: call.id.clone(),
                name: call.name.clone(),
                content: result.content,
                is_error: if result.is_error { Some(true) } else { None },
            });
        }
        response = Some(current);
    }

    Ok(ToolLoopResult {
        // max_iterations is clamped to >= 1, so response is always set here.
        response: response.expect("loop ran at least once"),
        exchange,
        iteration_limit_reached: true,
    })
}
