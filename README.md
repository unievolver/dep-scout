# dep-scout

**先查再造 (research before you build)** — an MCP server that stops AI coding
agents from reinventing the wheel.

Before an agent writes a feature from scratch, dep-scout searches the right
registry — **crates.io** (Rust), **npm** (JS/TS/frontend), **PyPI** (Python),
**pkg.go.dev** (Go), **Maven Central** (Java/Kotlin) or **NuGet** (.NET) — for a
mature package that already solves the problem, and scores each candidate on
**reuse quality** (popularity, maintenance recency, version stability,
metadata) — and flags **known security advisories** ([OSV](https://osv.dev)) and
**license-compliance risks**. The agent then reuses a proven, safe solution
instead of hand-rolling buggy code. It can also search the official **MCP
registry** so the agent reuses an existing MCP server instead of building one.

Works with **Cursor** (and any MCP client) over stdio. **No API keys** — every
data source is a keyless public API.

## Why

AI now writes a lot of code — and its worst habit is reimplementing solved
problems, picking abandoned libraries, or hallucinating packages. dep-scout
turns the best practice *"don't build it if a maintained package already does
it"* into a tool the agent calls automatically, across languages.

## Tools

| Tool | What it does |
| --- | --- |
| `find_packages` | Given a feature description + `ecosystem` (`rust` \| `npm` \| `python` \| `go` \| `maven` \| `nuget`), returns ranked candidates with a 0–100 reuse score, signals, and warnings. |
| `inspect_package` | Deep-dives one package by exact name + ecosystem: license (+ compliance flag), known OSV vulnerabilities, deprecation/yank status, runtime/version requirements, downloads or GitHub stars, last-update recency. |
| `find_mcp_servers` | Searches the official MCP registry for existing MCP servers (name, install info, repo, status) — so you don't rebuild one. |

`ecosystem` accepts aliases: `js`/`ts`/`javascript`/`typescript`/`node`/`frontend` → **npm**,
`py`/`pip` → **python**, `golang` → **go**, `java`/`kotlin`/`gradle`/`jvm` → **maven**,
`dotnet`/`csharp`/`net` → **nuget**, `rs`/`cargo` → **rust**.

### Reuse score (0–100)

- **Popularity** (≤40) — downloads normalised to a ~90-day window; for Go/Maven
  (no public download stats) GitHub stars are used as a proxy.
- **Maintenance** (≤30) — how recently the package was updated.
- **Stability** (≤15) — stable `1.0+` vs `0.x`; penalty for pre-releases.
- **Metadata** (≤15) — repository, docs/homepage, license present.
- **Security penalty** — known [OSV](https://osv.dev) advisories on the resolved
  version sink the score and de-rank the candidate.
- **License flag** — copyleft / commercially-restricted licenses (GPL, AGPL,
  LGPL, MPL, SSPL, BUSL…) are surfaced as compliance warnings.
- **Penalty** — latest version yanked (crates/PyPI) or deprecated (npm).

Verdicts: `≥78` ✅ strongly recommend · `58–77` 🟡 usable · `38–57` 🟠 cautious ·
`<38` 🔴 find an alternative.

Ranking blends **textual relevance** (query-term overlap, exact-name match) with
the **reuse score** and the registry's own ordering, so results are both on-topic
and high quality.

## Data sources (all keyless)

| Ecosystem | Search | Details | Popularity |
| --- | --- | --- | --- |
| Rust | crates.io API (relevance + downloads merge) | crates.io API | crates.io downloads (90-day) |
| npm | `registry.npmjs.org/-/v1/search` | `registry.npmjs.org/{pkg}/latest` | `api.npmjs.org` (last-month) |
| Python | [deps.dev](https://deps.dev) name search (+ token fallback) | `pypi.org/pypi/{pkg}/json` | pypistats (last-month) |
| Go | deps.dev name search | deps.dev v3 (`GetPackage`/`GetVersion`/`GetProject`) | GitHub stars |
| Java/Kotlin | Maven Central solr search | deps.dev v3 | GitHub stars |
| .NET | NuGet search API | NuGet search API | NuGet total downloads |
| MCP servers | official `registry.modelcontextprotocol.io` | — | — |
| Security | [OSV.dev](https://osv.dev) `query` (detail) + `querybatch` (flag) across all ecosystems | | |

> **Python note:** PyPI has no public full-text search API (the legacy XML-RPC
> search is gone and the web search is bot-blocked), so Python search is
> name-oriented via deps.dev. It excels when the query is/contains the library
> name; for pure capability phrases, prefer `inspect_package` with a known name.

## Build

Requires a recent Rust toolchain (edition 2024).

```bash
cargo build --release
```

The binary is produced at `target/release/dep-scout` (`.exe` on Windows).

## Use in Cursor

1. Build the release binary (above).
2. Register the MCP server. A ready-made project config lives at
   [`.cursor/mcp.json`](.cursor/mcp.json). For global use, add the same entry to
   `~/.cursor/mcp.json`:

   ```json
   {
     "mcpServers": {
       "dep-scout": {
         "command": "/absolute/path/to/dep-scout/target/release/dep-scout"
       }
     }
   }
   ```

   On Windows use the full `...\\target\\release\\dep-scout.exe` path.
3. Restart Cursor; you should see `dep-scout` with its tools enabled.
4. The [`.cursor/rules/research-before-build.mdc`](.cursor/rules/research-before-build.mdc)
   rule makes the agent search before building. A portable, client-agnostic
   version lives at [`skill/SKILL.md`](skill/SKILL.md).

## How it talks

Standard MCP over stdio (JSON-RPC 2.0), built on the official
[`rmcp`](https://crates.io/crates/rmcp) SDK. Logs go to stderr so they never
corrupt the protocol stream on stdout.

## Roadmap

- More ecosystems: Ruby (RubyGems), PHP (Packagist), Dart/Flutter (pub.dev).
- Per-ecosystem popularity calibration (npm/PyPI volumes dwarf crates.io).
- Last-update recency for NuGet; richer Maven search ranking.
- Deeper repo health signals (open issues, release cadence, maintainer count).
- A curated trust/quality dataset — the real moat over a plain registry mirror.

## Project layout

```
src/
  main.rs      # MCP server + tools (find_packages / inspect_package / find_mcp_servers)
  model.rs     # Ecosystem enum, normalised Package, reuse scoring, relevance
  sources.rs   # keyless registry clients (crates.io, npm, PyPI, deps.dev, MCP registry)
.cursor/
  rules/research-before-build.mdc   # always-on Cursor rule
  mcp.json                          # local server registration
skill/SKILL.md                      # portable skill for any MCP client
```

## MCP Registry

Published to the [official MCP Registry](https://registry.modelcontextprotocol.io) as
`io.github.unievolver/dep-scout`. Download the `.mcpb` bundle from
[GitHub Releases](https://github.com/unievolver/dep-scout/releases) or build from
source with `cargo build --release`.

- MCP Registry name: `mcp-name: io.github.unievolver/dep-scout`

## License

MIT
