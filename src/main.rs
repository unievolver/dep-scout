//! crate-scout — an MCP server that enforces "先查再造" (research before you build)
//! across ecosystems.
//!
//! Before an AI agent writes a feature from scratch, it should check: does a
//! mature, well-maintained package already do this? crate-scout searches
//! crates.io (Rust), npm (JS/TS/frontend) and PyPI (Python), scores candidates
//! on reuse quality, and can also discover existing MCP servers — so the agent
//! reuses proven solutions instead of reinventing wheels.

mod model;
mod sources;

use model::{Ecosystem, Package, Quality};
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
struct FindPackagesArgs {
    /// What you want to build, as keywords or a short phrase, e.g.
    /// "async http client", "date picker react", "parse yaml", "jwt auth".
    query: String,
    /// Which ecosystem to search. One of: rust, npm, python, go, maven, nuget.
    /// Aliases: js/ts/frontend -> npm; py -> python; golang -> go;
    /// java/kotlin/gradle -> maven; dotnet/csharp -> nuget; rs/cargo -> rust.
    /// Pass the language of the project you're working in.
    ecosystem: Ecosystem,
    /// Maximum number of candidate packages to return. Defaults to 8.
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct InspectPackageArgs {
    /// Exact package name as published in the registry, e.g. "tokio", "react",
    /// "requests".
    name: String,
    /// Which ecosystem the package belongs to: rust, npm, python, go, maven, or
    /// nuget (aliases ok). For Go pass the full module path; for Maven pass
    /// "group:artifact".
    ecosystem: Ecosystem,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FindMcpServersArgs {
    /// Capability or name to look for, e.g. "postgres", "filesystem", "github",
    /// "playwright". Note: the registry matches on server names, so prefer
    /// concrete nouns over long phrases.
    query: String,
    /// Maximum number of servers to return. Defaults to 8.
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Clone)]
struct CrateScout {
    http: reqwest::Client,
    #[allow(dead_code)] // used by the #[tool_handler] generated impl
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl CrateScout {
    fn new() -> Self {
        Self {
            http: sources::http_client(),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "先查再造: before writing code that is likely a solved problem, find existing \
            mature packages that already do it. Give a short description/keywords and the \
            ecosystem (rust | npm | python | go | maven | nuget; npm covers all JS/TS/frontend, \
            maven covers Java/Kotlin, nuget covers .NET). Returns ranked candidates with a 0-100 \
            reuse-quality score (popularity, maintenance, stability, metadata) so you reuse a \
            proven library instead of reinventing it."
    )]
    async fn find_packages(
        &self,
        Parameters(args): Parameters<FindPackagesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let limit = args.limit.unwrap_or(8).clamp(1, 25);
        let query = args.query.trim();
        if query.is_empty() {
            return Err(McpError::invalid_params("query must not be empty", None));
        }

        let candidates = sources::search(&self.http, args.ecosystem, query, limit)
            .await
            .map_err(|e| McpError::internal_error(format!("search failed: {e}"), None))?;

        if candidates.is_empty() {
            let extra = if args.ecosystem == Ecosystem::Python {
                "\n注意：PyPI 没有公开的关键词搜索 API，本工具按「包名」解析查询。\
                 若你已知道包名，请直接用 inspect_package；否则换成确切的包名再试。"
            } else {
                ""
            };
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "在 {} 上没有找到匹配 \"{query}\" 的包。\n\
                 建议：换更通用的英文关键词；确认 ecosystem 选对了；若确实没有现成方案，再自己实现。{extra}",
                args.ecosystem.label()
            ))]));
        }

        // Rank by a blend of relevance (text overlap + registry order) and
        // reuse quality. Exact-name matches are forced to the top.
        let pool_len = candidates.len().max(1) as f64;
        let mut scored: Vec<(Package, Quality, f64)> = candidates
            .into_iter()
            .enumerate()
            .map(|(idx, p)| {
                let q = model::score(&p);
                let position_rel = (pool_len - idx as f64) / pool_len;
                let overlap = model::term_overlap(query, &p);
                let exact = if model::name_is_exact(query, &p) { 1000.0 } else { 0.0 };
                let combined = q.score as f64 * 0.45
                    + overlap * 100.0 * 0.35
                    + position_rel * 100.0 * 0.20
                    + exact;
                (p, q, combined)
            })
            .collect();
        scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit as usize);

        let mut out = format!(
            "「先查再造」结果：在 {} 搜索 \"{query}\"，{} 个候选（按相关度 + 复用质量综合排序）。\n\
             优先复用得分高、且确实对口的；动手自己写之前，请确认没有更合适的现成包。\n\n",
            args.ecosystem.label(),
            scored.len()
        );
        for (i, (p, q, _)) in scored.iter().enumerate() {
            out.push_str(&render_package(i + 1, p, q));
            out.push('\n');
        }
        out.push_str("提示：用 `inspect_package` 可看某个包的许可证、弃用状态、运行时要求等细节。");

        Ok(CallToolResult::success(vec![Content::text(out)]))
    }

    #[tool(
        description = "Deeply inspect one package by exact name + ecosystem (rust | npm | python | \
            go | maven | nuget) to decide whether to depend on it. Returns a reuse-quality score \
            plus license, deprecation/yank status, runtime/version requirements, download stats or \
            GitHub stars, last-update recency, and links."
    )]
    async fn inspect_package(
        &self,
        Parameters(args): Parameters<InspectPackageArgs>,
    ) -> Result<CallToolResult, McpError> {
        let name = args.name.trim();
        if name.is_empty() {
            return Err(McpError::invalid_params("name must not be empty", None));
        }

        let p = sources::inspect(&self.http, args.ecosystem, name)
            .await
            .map_err(|e| {
                McpError::internal_error(format!("could not fetch '{name}': {e}"), None)
            })?;
        let q = model::score(&p);

        let mut out = format!("crate-scout 详细评估：{} ({})\n\n", p.name, p.ecosystem.label());
        out.push_str(&render_package(0, &p, &q));
        if let Some(lic) = &p.license {
            out.push_str(&format!("\n许可证: {lic}"));
        }
        if let Some(rt) = &p.min_runtime {
            out.push_str(&format!("\n运行时要求: {rt}"));
        }
        if let Some(total) = p.total_downloads {
            out.push_str(&format!("\n累计下载: {}", model::fmt_num(total)));
        }
        out.push_str(&format!("\n安装: {}", p.ecosystem.install_hint(&p.name)));

        Ok(CallToolResult::success(vec![Content::text(out)]))
    }

    #[tool(
        description = "先查再造 for tooling: search the official MCP registry for existing MCP \
            servers before building your own. Give a concrete capability/name (e.g. 'postgres', \
            'filesystem', 'github'). Returns servers with install info, repository and status."
    )]
    async fn find_mcp_servers(
        &self,
        Parameters(args): Parameters<FindMcpServersArgs>,
    ) -> Result<CallToolResult, McpError> {
        let limit = args.limit.unwrap_or(8).clamp(1, 25);
        let query = args.query.trim();
        if query.is_empty() {
            return Err(McpError::invalid_params("query must not be empty", None));
        }

        let servers = sources::mcp_search(&self.http, query, limit)
            .await
            .map_err(|e| McpError::internal_error(format!("MCP registry search failed: {e}"), None))?;

        if servers.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "官方 MCP 注册表里没有名字匹配 \"{query}\" 的 server。\n\
                 注意：注册表只按 server 名称做子串匹配，换更具体的名词（如 postgres、filesystem、github）再试。"
            ))]));
        }

        let mut out = format!(
            "MCP server 搜索结果：\"{query}\" 命中 {} 个（来自官方 MCP 注册表）。\n\
             自己写 MCP server 前，先看看有没有现成的可用。\n\n",
            servers.len()
        );
        for (i, s) in servers.iter().enumerate() {
            out.push_str(&format!("{}. {}", i + 1, s.name));
            if let Some(v) = &s.version {
                out.push_str(&format!("  v{v}"));
            }
            if let Some(st) = &s.status {
                out.push_str(&format!("  [{st}]"));
            }
            out.push('\n');
            if let Some(d) = &s.description {
                out.push_str(&format!("   {}\n", d.trim()));
            }
            if let Some(inst) = &s.install {
                out.push_str(&format!("   安装: {inst}\n"));
            }
            if let Some(repo) = &s.repository {
                out.push_str(&format!("   仓库: {repo}\n"));
            }
        }

        Ok(CallToolResult::success(vec![Content::text(out)]))
    }
}

fn render_package(rank: usize, p: &Package, q: &Quality) -> String {
    let mut s = String::new();
    let header = if rank > 0 {
        format!("{}. {}", rank, p.name)
    } else {
        p.name.clone()
    };
    let ver = p.display_version();
    let ver = ver.strip_prefix('v').unwrap_or(ver);
    s.push_str(&format!(
        "{header}  v{ver}  | 复用评分 {}/100  {}\n",
        q.score, q.verdict
    ));
    if let Some(desc) = &p.description {
        s.push_str(&format!("   {}\n", desc.trim()));
    }
    if let Some(repo) = &p.repository {
        s.push_str(&format!("   仓库: {repo}\n"));
    }
    if let Some(docs) = p.ecosystem.docs_url(&p.name) {
        s.push_str(&format!("   文档: {docs}\n"));
    }
    s.push_str(&format!("   主页: {}\n", p.ecosystem.page_url(&p.name)));
    if !q.signals.is_empty() {
        s.push_str(&format!("   优点: {}\n", q.signals.join("；")));
    }
    if !q.warnings.is_empty() {
        s.push_str(&format!("   注意: {}\n", q.warnings.join("；")));
    }
    s
}

#[tool_handler]
impl ServerHandler for CrateScout {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "crate-scout 帮你在多语言开发中践行「先查再造」：写新功能前，先用 find_packages \
             搜索 crates.io(Rust)/npm(JS·TS·前端)/PyPI(Python)/Go/Maven(Java·Kotlin)/NuGet(.NET) \
             上已有的成熟方案，用 inspect_package 评估具体候选；写 MCP server 前先用 \
             find_mcp_servers 找现成的。避免重复造轮子。"
                .to_string(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // MCP speaks JSON-RPC over stdout, so all logs MUST go to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "crate_scout=info,rmcp=warn".into()),
        )
        .init();

    tracing::info!("starting crate-scout MCP server on stdio");

    let service = CrateScout::new().serve(stdio()).await.inspect_err(|e| {
        tracing::error!("failed to start server: {e}");
    })?;
    service.waiting().await?;
    Ok(())
}
