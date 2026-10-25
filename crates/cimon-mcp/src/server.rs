//! rmcp `ServerHandler`: discovery, tool list, tool calls, `tasks/get`, `tasks/cancel`.

use crate::tools::{self, ToolError, ToolOutcome};
use cimon_engine::App;
use rmcp::{
    model::*,
    service::{RequestContext, RoleServer},
    ErrorData as McpError, ServerHandler,
};
use std::borrow::Cow;
use std::sync::Arc;

#[derive(Clone)]
pub struct CimonMcp {
    pub(crate) app: Arc<App>,
}

impl CimonMcp {
    pub fn new(app: Arc<App>) -> Self {
        CimonMcp { app }
    }
}

pub(crate) struct CallCtx {
    pub user: cimon_core::AuthUser,
    pub tailnet_login: Option<String>,
    pub supports_tasks: bool,
}

impl ServerHandler for CimonMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_tasks().build())
            .with_server_info(Implementation::new("cimon", env!("CARGO_PKG_VERSION")).with_title("cimon — remote build & test"))
            .with_instructions(
                "cimon builds and tests registered projects on a Windows build machine. build/test/run return a task; \
                 poll it, then read the verdict, failure_class and artifact links. Recipes are edited with recipe_* tools \
                 on recipe/* branches and promoted by an admin.",
            )
    }

    /// Only the 2026-07-28 revision (design §7.1). Older clients get -32022 from rmcp.
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(&[ProtocolVersion::V_2026_07_28])
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tools::definitions(&self.app)))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools::definitions(&self.app).into_iter().find(|t| t.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let parts = context.extensions.get::<http::request::Parts>();
        let Some(user) = crate::auth_user(parts) else {
            return Err(McpError::invalid_request("unauthenticated", None));
        };
        let ctx = CallCtx {
            user,
            tailnet_login: crate::tailnet_login(parts),
            supports_tasks: context.client_capabilities().is_some_and(|c| c.supports_tasks()),
        };
        let args = serde_json::Value::Object(request.arguments.clone().unwrap_or_default());
        let app = self.app.clone();
        let name = request.name.to_string();
        let outcome = tools::call(&app, &ctx, &name, args).await;
        let _ = app.store.audit(cimon_store::AuditEntry {
            user: &ctx.user.name,
            action: &format!("call:{name}"),
            tailnet_login: ctx.tailnet_login.as_deref(),
            detail: match &outcome {
                Err(e) => Some(format!("rejected: {}", e.code)),
                Ok(_) => None,
            },
            ..Default::default()
        });
        Ok(match outcome {
            Ok(ToolOutcome::Result(r)) => CallToolResponse::Complete(r),
            Ok(ToolOutcome::Task(t)) => CallToolResponse::Task(CreateTaskResult::new(t)),
            Err(e) => CallToolResponse::Complete(e.into_result()),
        })
    }

    async fn get_task(&self, request: GetTaskParams, context: RequestContext<RoleServer>) -> Result<GetTaskResult, McpError> {
        let parts = context.extensions.get::<http::request::Parts>();
        if crate::auth_user(parts).is_none() {
            return Err(McpError::invalid_request("unauthenticated", None));
        }
        match tools::get_task(&self.app, &request.task_id) {
            Ok(t) => Ok(GetTaskResult::new(t)),
            Err(ToolError { message, .. }) => Err(McpError::invalid_params(message, None)),
        }
    }

    async fn cancel_task(&self, request: CancelTaskParams, context: RequestContext<RoleServer>) -> Result<(), McpError> {
        let parts = context.extensions.get::<http::request::Parts>();
        let Some(user) = crate::auth_user(parts) else {
            return Err(McpError::invalid_request("unauthenticated", None));
        };
        tools::cancel_task(&self.app, &user, &request.task_id).map_err(|e| McpError::invalid_params(e.message, None))
    }
}
