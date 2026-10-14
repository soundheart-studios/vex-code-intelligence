//! `vex mcp`: a thin MCP stdio adapter over vex-app.
//!
//! Advertises Cortex's nine code tools under their names and with their
//! exact input schemas (descriptions adapted to vex; `tools.json`). Every
//! call builds the same vex-app request the CLI builds and returns the same
//! reply text as `vex --json`, so the two can never disagree. A reply that
//! is an error document reads `VEX ERROR (code): message`, after Cortex's
//! `CORTEX ERROR (code): message`; arguments that do not fit the schema
//! are a tool error naming the argument.

use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
    Implementation, JsonObject, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use vex_app::tools::{self, Reply};

pub mod transport;

/// The advertised tools: name, description and input schema.
const TOOLS: &str = include_str!("tools.json");

/// Every tool vex advertises, in Cortex's order.
pub fn tools() -> Vec<Tool> {
    let table: Vec<Value> = serde_json::from_str(TOOLS).expect("tools.json parses");
    table
        .into_iter()
        .map(|t| {
            let name = t["name"].as_str().expect("tool name").to_string();
            let description = t["description"].as_str().expect("description").to_string();
            let schema: JsonObject = match &t["inputSchema"] {
                Value::Object(o) => o.clone(),
                _ => panic!("{name}: inputSchema is an object"),
            };
            Tool::new(name, description, Arc::new(schema))
        })
        .collect()
}

/// A tool call's result: the text shown to the caller, and whether the
/// call itself was malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Called {
    pub text: String,
    pub is_error: bool,
}

impl Called {
    fn reply(reply: &Reply) -> Called {
        let text = match &reply.error {
            Some((code, message)) => format!("VEX ERROR ({code}): {message}"),
            None => reply.text.clone(),
        };
        Called {
            text,
            is_error: false,
        }
    }

    fn invalid(message: String) -> Called {
        Called {
            text: format!("VEX ERROR (invalid_request): {message}"),
            is_error: true,
        }
    }
}

/// Arguments checked against the tool's schema, as pydantic would.
struct Args<'a> {
    map: &'a JsonObject,
}

type Checked<T> = Result<T, String>;

impl Args<'_> {
    fn get(&self, key: &str) -> Option<&Value> {
        self.map.get(key).filter(|v| !v.is_null())
    }

    fn required(&self, key: &str) -> Checked<&Value> {
        self.map
            .get(key)
            .ok_or_else(|| format!("missing required argument '{key}'"))
    }

    fn string(&self, key: &str, default: &str) -> Checked<String> {
        match self.get(key) {
            None => Ok(default.to_string()),
            Some(Value::String(s)) => Ok(s.clone()),
            Some(_) => Err(format!("'{key}' must be a string")),
        }
    }

    fn required_string(&self, key: &str) -> Checked<String> {
        match self.required(key)? {
            Value::String(s) => Ok(s.clone()),
            _ => Err(format!("'{key}' must be a string")),
        }
    }

    /// `str | list[str]`: the strings and whether a list was given.
    fn strings(&self, key: &str) -> Checked<(Vec<String>, bool)> {
        match self.required(key)? {
            Value::String(s) => Ok((vec![s.clone()], false)),
            Value::Array(items) => items
                .iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .map(|v| (v, true))
                .ok_or_else(|| format!("'{key}' must be a string or a list of strings")),
            _ => Err(format!("'{key}' must be a string or a list of strings")),
        }
    }

    fn int(&self, key: &str, default: i64) -> Checked<i64> {
        match self.get(key) {
            None => Ok(default),
            Some(Value::Number(n)) => n
                .as_i64()
                .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
                .ok_or_else(|| format!("'{key}' must be an integer")),
            Some(_) => Err(format!("'{key}' must be an integer")),
        }
    }

    fn number(&self, key: &str, default: f64) -> Checked<f64> {
        match self.get(key) {
            None => Ok(default),
            Some(Value::Number(n)) => n
                .as_f64()
                .ok_or_else(|| format!("'{key}' must be a number")),
            Some(_) => Err(format!("'{key}' must be a number")),
        }
    }

    fn boolean(&self, key: &str, default: bool) -> Checked<bool> {
        match self.get(key) {
            None => Ok(default),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => Err(format!("'{key}' must be a boolean")),
        }
    }

    /// `list[str] | None`.
    fn optional_strings(&self, key: &str) -> Checked<Vec<String>> {
        match self.get(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| format!("'{key}' must be a list of strings")),
            Some(_) => Err(format!("'{key}' must be a list of strings")),
        }
    }
}

/// Run one tool. `home` is VEX_HOME (None when unset), `cwd` the directory
/// an empty `project` means.
pub fn call(home: Option<&Path>, cwd: &Path, name: &str, arguments: &JsonObject) -> Called {
    match dispatch(home, cwd, name, &Args { map: arguments }) {
        Ok(reply) => Called::reply(&reply),
        Err(message) => Called::invalid(message),
    }
}

fn dispatch(home: Option<&Path>, cwd: &Path, name: &str, a: &Args) -> Checked<Reply> {
    use vex_app::{changes, expand, graph, grep, map, project, search};
    // Validate the arguments before anything touches a project.
    enum Call {
        Projects,
        Search(search::Request),
        Context(String, String, f64),
        Impact(graph::ImpactRequest),
        Map(i64, Vec<String>),
        Expand(expand::Request),
        Grep(grep::Request),
        Changes(changes::Request),
        Reindex(bool),
    }
    let call = match name {
        "list_projects" => Call::Projects,
        "search" => {
            let (queries, list) = a.strings("query")?;
            Call::Search(search::Request {
                queries,
                list,
                top_k: a.int("top_k", 5)?,
                max_bytes: a.int("max_bytes", 0)?,
                path: a.string("path", "")?,
                select: a.string("select", "")?,
                force_fresh: a.boolean("force_fresh", false)?,
            })
        }
        "context" => Call::Context(
            a.required_string("symbol")?,
            a.string("detail", "full")?,
            a.number("confidence_min", 0.7)?,
        ),
        "impact" => {
            let (roots, list) = a.strings("symbol")?;
            Call::Impact(graph::ImpactRequest {
                roots,
                list,
                direction: a.string("direction", "upstream")?,
                depth: a.int("depth", 3)?,
                confidence_min: a.number("confidence_min", 0.7)?,
            })
        }
        "map" => Call::Map(
            a.int("budget_tokens", map::DEFAULT_MAP_TOKEN_BUDGET)?,
            a.optional_strings("focus")?,
        ),
        "expand" => {
            let (targets, list) = a.strings("target")?;
            Call::Expand(expand::Request {
                targets,
                list,
                detail: a.string("detail", "full")?,
                max_bytes: a.int("max_bytes", expand::DEFAULT_MAX_BYTES)?,
            })
        }
        "grep" => Call::Grep(grep::Request {
            pattern: a.required_string("pattern")?,
            path: a.string("path", "")?,
            literal: a.boolean("literal", false)?,
            max_results: a.int("max_results", grep::DEFAULT_MAX_RESULTS)?,
            context_lines: a.int("context_lines", 0)?,
        }),
        "detect_changes" => Call::Changes(changes::Request {
            base: a.string("base", "")?,
            staged: false,
            max_depth: a.int("max_depth", changes::DEFAULT_MAX_DEPTH)?,
        }),
        "reindex" => Call::Reindex(a.boolean("force", false)?),
        other => return Err(format!("unknown tool '{other}'")),
    };
    let project = a.string("project", "")?;
    let Some(home) = home else {
        return Ok(Reply::no_home());
    };
    if let Call::Projects = call {
        return Ok(tools::reply_serde(&tools::projects(home)));
    }
    let start = match tools::resolve_project(home, cwd, &project) {
        Ok(start) => start,
        Err(reply) => return Ok(reply),
    };
    Ok(match call {
        Call::Projects => unreachable!("answered above"),
        Call::Search(req) => tools::reply(&search::run(home, &start, &req)),
        Call::Context(symbol, detail, floor) => {
            tools::reply(&graph::run_context(home, &start, &symbol, &detail, floor))
        }
        Call::Impact(req) => tools::reply(&graph::run_impact(home, &start, &req)),
        Call::Map(budget, focus) => tools::reply(&map::run(home, &start, budget, &focus)),
        Call::Expand(req) => tools::reply(&expand::run(home, &start, &req)),
        Call::Grep(req) => tools::reply(&grep::run(home, &start, &req)),
        Call::Changes(req) => tools::reply(&changes::run(home, &start, &req)),
        Call::Reindex(force) => tools::reply_serde(&project::index(
            home,
            &start,
            force,
            &vex_app::embed::Plan::from_env(home, false),
        )),
    })
}

/// The MCP server: the tool table plus where calls resolve projects.
pub struct Server {
    home: Option<PathBuf>,
    cwd: PathBuf,
    tools: Vec<Tool>,
    route: Option<transport::Route>,
}

impl Server {
    pub fn new(home: Option<PathBuf>, cwd: PathBuf) -> Server {
        Server {
            home,
            cwd,
            tools: tools(),
            route: None,
        }
    }

    /// Try `route` (the daemon) before answering in this process.
    pub fn with_route(mut self, route: transport::Route) -> Server {
        self.route = Some(route);
        self
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("vex", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let listed = ListToolsResult::with_all_items(self.tools.clone());
        // From 2026-07-28 a list result must say how long it stays fresh and
        // who may cache it; clients reject one without. Older sessions keep
        // the shape they know. Not cached: a daemon upgrade can change it.
        let caching = context
            .protocol_version()
            .is_some_and(|v| v.as_str() >= ProtocolVersion::V_2026_07_28.as_str());
        Ok(if caching {
            listed.with_ttl_ms(0).with_cache_scope(CacheScope::Private)
        } else {
            listed
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if !self.tools.iter().any(|t| t.name == request.name) {
            return Err(ErrorData::invalid_params(
                format!("unknown tool '{}'", request.name),
                None,
            ));
        }
        let arguments = request.arguments.unwrap_or_default();
        let routed = self
            .route
            .as_ref()
            .and_then(|r| r(&request.name, &arguments));
        let called = match routed {
            Some(c) => c,
            None => call(self.home.as_deref(), &self.cwd, &request.name, &arguments),
        };
        let content = vec![ContentBlock::text(called.text)];
        Ok(if called.is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        }
        .into())
    }
}

/// Serve MCP over stdin and stdout until the client disconnects.
pub fn serve_stdio(home: Option<PathBuf>, cwd: PathBuf) -> std::io::Result<()> {
    serve_stdio_routed(home, cwd, None)
}

/// `serve_stdio`, trying `route` for every call first.
pub fn serve_stdio_routed(
    home: Option<PathBuf>,
    cwd: PathBuf,
    route: Option<transport::Route>,
) -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let mut server = Server::new(home, cwd);
        if let Some(r) = route {
            server = server.with_route(r);
        }
        let service = server
            .serve(rmcp::transport::stdio())
            .await
            .map_err(std::io::Error::other)?;
        service.waiting().await.map_err(std::io::Error::other)?;
        Ok(())
    })
}
