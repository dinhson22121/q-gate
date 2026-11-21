use std::fmt::Write as _;
use std::io;
use std::sync::Arc;

use rmcp::ErrorData;
use rmcp::RoleServer;
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::{Map, Value};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::llm::{ToolOutput, ToolSpec};

pub const SERVER_NAME: &str = "qgate";
const SECRET_BYTES: usize = 24;

pub struct ToolCall {
    pub name: String,
    pub input: Value,
    pub reply: oneshot::Sender<ToolOutput>,
}

#[derive(Clone)]
pub struct ToolBridge {
    tools: Arc<Vec<Tool>>,
    calls: mpsc::Sender<ToolCall>,
}

impl ToolBridge {
    pub fn new(specs: &[ToolSpec], calls: mpsc::Sender<ToolCall>) -> Self {
        let tools = specs
            .iter()
            .map(|spec| {
                let schema: Map<String, Value> = spec.input_schema.as_object().cloned().unwrap_or_default();
                Tool::new(spec.name.clone(), spec.description.clone(), Arc::new(schema))
            })
            .collect();
        Self { tools: Arc::new(tools), calls }
    }

    async fn forward(&self, name: &str, input: Value) -> ToolOutput {
        if !self.tools.iter().any(|t| t.name == name) {
            return ToolOutput::error(format!("unknown tool '{name}'"));
        }
        let (reply, answer) = oneshot::channel();
        let call = ToolCall { name: name.to_string(), input, reply };
        if self.calls.send(call).await.is_err() {
            return ToolOutput::error("the review session has ended");
        }
        answer.await.unwrap_or_else(|_| ToolOutput::error("the review session has ended"))
    }
}

impl ServerHandler for ToolBridge {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.to_vec()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let input = Value::Object(request.arguments.unwrap_or_default());
        let output = self.forward(&request.name, input).await;
        let content = vec![ContentBlock::text(output.content)];
        let result = if output.is_error { CallToolResult::error(content) } else { CallToolResult::success(content) };
        Ok(result.into())
    }
}

pub struct RunningServer {
    url: String,
    task: JoinHandle<()>,
}

impl RunningServer {
    pub async fn start(bridge: ToolBridge) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let path = format!("/mcp/{}", random_secret()?);
        let service = StreamableHttpService::new(
            move || Ok(bridge.clone()),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default(),
        );
        let app = axum::Router::new().nest_service(&path, service);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { url: format!("http://127.0.0.1:{port}{path}"), task })
    }

    pub fn mcp_config(&self) -> Value {
        serde_json::json!({ "mcpServers": { SERVER_NAME: { "type": "http", "url": self.url } } })
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn random_secret() -> io::Result<String> {
    let mut bytes = [0u8; SECRET_BYTES];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    }))
}

#[cfg(test)]
#[path = "mcp_bridge_tests.rs"]
mod tests;
