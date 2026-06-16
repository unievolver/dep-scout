---
name: research-before-build
description: Before implementing a feature in Rust, JavaScript/TypeScript, Python, Go, Java/Kotlin, or .NET, find and reuse a mature existing package instead of writing it from scratch. Use when adding dependencies, building a capability that is likely already solved (HTTP, parsing, auth, retries, dates, state management, UI components, validation, CLI), choosing between libraries, or before building an MCP server.
---

# 先查再造 / Research before you build

A portable skill that pairs with the **crate-scout** MCP server. Its job: stop
AI agents from reinventing wheels by always checking the right registry —
crates.io (Rust), npm (JS/TS/frontend), PyPI (Python), pkg.go.dev (Go), Maven
Central (Java/Kotlin) or NuGet (.NET) — for a mature, maintained package first.

## How to apply

When you are about to build a non-trivial piece of functionality:

1. **Search first.** Call `find_packages` with keywords describing the
   capability plus the project's `ecosystem` (`rust` | `npm` | `python` | `go` |
   `maven` | `nuget`; `js`/`ts`/`frontend` → npm, `py` → python, `golang` → go,
   `java`/`kotlin` → maven, `dotnet`/`csharp` → nuget). Examples:
   `{query:"async http client", ecosystem:"rust"}`,
   `{query:"react date picker", ecosystem:"frontend"}`,
   `{query:"json", ecosystem:"dotnet"}`.
2. **Read the scores.** crate-scout ranks candidates 0–100 on popularity,
   maintenance recency, version stability, and metadata completeness:
   - `≥ 78` ✅ strongly recommended — reuse it.
   - `58–77` 🟡 usable — resolve the listed warnings first.
   - `< 58` 🟠/🔴 — compare alternatives or justify a from-scratch build.
3. **Vet the finalist.** Call `inspect_package` with the exact name + ecosystem
   to confirm license, deprecation/yank status, and runtime/version requirements
   before depending on it.
4. **Reuse tooling too.** Before building an MCP server, call `find_mcp_servers`
   to check the official registry for an existing one.
5. **Decide explicitly.** Reuse the best fit, or state in one line why nothing
   suitable exists and you are implementing it yourself.

## Notes

- Pass the ecosystem of the project you're editing. Frontend frameworks (React,
  Vue, Svelte, Angular…) all live on `npm`.
- PyPI has no public full-text search API, so Python search is name-oriented:
  prefer concrete library names, or use `inspect_package` directly.

## Why

Most "new" features are solved problems. A maintained package carries fewer
bugs, better security, and lower long-term cost than a hand-rolled
reimplementation. The default should be reuse; building from scratch is the
exception that needs a reason.
