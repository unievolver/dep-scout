//! HTTP clients for every supported registry, each normalised into [`Package`].
//!
//! No API keys anywhere: crates.io, npm, PyPI and the official MCP registry all
//! expose keyless public read APIs.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use crate::model::{Ecosystem, Package};

const USER_AGENT: &str =
    "crate-scout/0.2 (Rust MCP tool; https://github.com/crate-scout/crate-scout)";

/// deps.dev's internal search endpoint rejects non-browser User-Agents, so we
/// present a browser UA only for that host.
const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .expect("failed to build reqwest client")
}

/// Dispatch a search to the right registry.
pub async fn search(
    http: &reqwest::Client,
    eco: Ecosystem,
    query: &str,
    limit: u32,
) -> anyhow::Result<Vec<Package>> {
    match eco {
        Ecosystem::Rust => crates_search(http, query, limit).await,
        Ecosystem::Npm => npm_search(http, query, limit).await,
        Ecosystem::Python => pypi_search(http, query, limit).await,
        Ecosystem::Go => go_search(http, query, limit).await,
        Ecosystem::Maven => maven_search(http, query, limit).await,
        Ecosystem::Nuget => nuget_search(http, query, limit).await,
    }
}

/// Dispatch a single-package inspection to the right registry.
pub async fn inspect(
    http: &reqwest::Client,
    eco: Ecosystem,
    name: &str,
) -> anyhow::Result<Package> {
    match eco {
        Ecosystem::Rust => crates_get(http, name).await,
        Ecosystem::Npm => npm_get(http, name).await,
        Ecosystem::Python => pypi_get(http, name).await,
        Ecosystem::Go => deps_dev_enrich(http, Ecosystem::Go, "go", name).await,
        Ecosystem::Maven => deps_dev_enrich(http, Ecosystem::Maven, "maven", name).await,
        Ecosystem::Nuget => nuget_get(http, name).await,
    }
}

/// Percent-encode a string for use as a single URL path segment.
fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn extract_license(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(o) => o.get("type").and_then(|t| t.as_str()).map(String::from),
        _ => None,
    }
}

// ===========================================================================
// crates.io (Rust)
// ===========================================================================

#[derive(Debug, Deserialize)]
struct CratesSearchResp {
    #[serde(default)]
    crates: Vec<RawCrate>,
}

#[derive(Debug, Deserialize)]
struct CrateResp {
    #[serde(rename = "crate")]
    krate: RawCrate,
    #[serde(default)]
    versions: Vec<RawCrateVersion>,
}

#[derive(Debug, Default, Deserialize)]
struct RawCrate {
    name: String,
    description: Option<String>,
    #[serde(default)]
    downloads: u64,
    recent_downloads: Option<u64>,
    newest_version: Option<String>,
    max_stable_version: Option<String>,
    updated_at: Option<String>,
    repository: Option<String>,
    homepage: Option<String>,
    // crates.io's search endpoint sends `keywords: null`, so this must tolerate
    // an explicit null (which `#[serde(default)]` alone does not).
    #[serde(default)]
    keywords: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawCrateVersion {
    num: String,
    #[serde(default)]
    yanked: bool,
    license: Option<String>,
    rust_version: Option<String>,
}

impl RawCrate {
    fn into_package(self) -> Package {
        let mut p = Package::new(Ecosystem::Rust, self.name);
        p.description = self.description;
        p.keywords = self.keywords.unwrap_or_default();
        p.recent_downloads_90d = self.recent_downloads;
        p.total_downloads = Some(self.downloads);
        p.updated_at = self.updated_at;
        p.latest_version = self.newest_version;
        p.stable_version = self.max_stable_version;
        p.repository = self.repository;
        p.homepage = self.homepage;
        p
    }
}

async fn crates_search_sorted(
    http: &reqwest::Client,
    query: &str,
    per_page: u32,
    sort: &str,
) -> anyhow::Result<Vec<Package>> {
    let resp = http
        .get("https://crates.io/api/v1/crates")
        .query(&[
            ("q", query),
            ("per_page", &per_page.to_string()),
            ("sort", sort),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<CratesSearchResp>()
        .await?;
    Ok(resp.crates.into_iter().map(RawCrate::into_package).collect())
}

/// Merge relevance-sorted and downloads-sorted results so popular head crates
/// (e.g. reqwest for "http client") aren't missed by raw relevance alone.
/// A real failure of *both* sorts is surfaced rather than masked as "no results".
async fn crates_search(
    http: &reqwest::Client,
    query: &str,
    limit: u32,
) -> anyhow::Result<Vec<Package>> {
    let per_page = (limit * 4).clamp(15, 40);
    let by_relevance = crates_search_sorted(http, query, per_page, "relevance").await;
    let by_downloads = crates_search_sorted(http, query, per_page, "downloads").await;

    let (primary, secondary) = match (by_relevance, by_downloads) {
        (Ok(a), Ok(b)) => (a, b),
        (Ok(a), Err(_)) => (a, Vec::new()),
        (Err(_), Ok(b)) => (b, Vec::new()),
        (Err(e), Err(_)) => return Err(e),
    };

    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    for p in primary.into_iter().chain(secondary.into_iter()) {
        if seen.insert(p.name.clone()) {
            merged.push(p);
        }
    }
    Ok(merged)
}

async fn crates_get(http: &reqwest::Client, name: &str) -> anyhow::Result<Package> {
    let resp = http
        .get(format!("https://crates.io/api/v1/crates/{name}"))
        .send()
        .await?
        .error_for_status()?
        .json::<CrateResp>()
        .await?;

    let mut p = resp.krate.into_package();
    if let Some(latest) = resp.versions.first() {
        p.deprecated = Some(latest.yanked);
        p.license = latest.license.clone();
        p.min_runtime = latest.rust_version.clone();
        if p.latest_version.is_none() {
            p.latest_version = Some(latest.num.clone());
        }
    }
    Ok(p)
}

// ===========================================================================
// npm (JavaScript / TypeScript / frontend)
// ===========================================================================

#[derive(Debug, Deserialize)]
struct NpmSearchResp {
    #[serde(default)]
    objects: Vec<NpmObject>,
}

#[derive(Debug, Deserialize)]
struct NpmObject {
    package: NpmSearchPkg,
    downloads: Option<NpmDownloads>,
}

#[derive(Debug, Deserialize)]
struct NpmDownloads {
    monthly: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct NpmSearchPkg {
    name: String,
    version: Option<String>,
    description: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    date: Option<String>,
    license: Option<String>,
    links: Option<NpmLinks>,
}

#[derive(Debug, Deserialize)]
struct NpmLinks {
    homepage: Option<String>,
    repository: Option<String>,
}

async fn npm_search(
    http: &reqwest::Client,
    query: &str,
    limit: u32,
) -> anyhow::Result<Vec<Package>> {
    let size = (limit * 3).clamp(10, 40);
    let resp = http
        .get("https://registry.npmjs.org/-/v1/search")
        .query(&[("text", query), ("size", &size.to_string())])
        .send()
        .await?
        .error_for_status()?
        .json::<NpmSearchResp>()
        .await?;

    Ok(resp
        .objects
        .into_iter()
        .map(|o| {
            let mut p = Package::new(Ecosystem::Npm, o.package.name);
            p.description = o.package.description;
            p.keywords = o.package.keywords;
            p.recent_downloads_90d = o.downloads.and_then(|d| d.monthly).map(|m| m.saturating_mul(3));
            p.updated_at = o.package.date;
            p.latest_version = o.package.version.clone();
            p.stable_version = o.package.version;
            p.license = o.package.license;
            if let Some(links) = o.package.links {
                p.homepage = links.homepage;
                p.repository = links.repository;
            }
            p
        })
        .collect())
}

#[derive(Debug, Deserialize)]
struct NpmRepo {
    url: Option<String>,
}

/// The `/{pkg}/latest` manifest — small and complete, even for packages with
/// thousands of versions (fetching the full package document can be tens of MB).
#[derive(Debug, Deserialize)]
struct NpmManifest {
    version: Option<String>,
    description: Option<String>,
    license: Option<Value>,
    homepage: Option<String>,
    repository: Option<NpmRepo>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    engines: HashMap<String, String>,
    deprecated: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct NpmDownloadPoint {
    downloads: Option<u64>,
}

async fn npm_get(http: &reqwest::Client, name: &str) -> anyhow::Result<Package> {
    // Fetch the (small) latest manifest, the search hit (for last-publish date +
    // downloads), and the downloads point — all concurrently.
    let manifest_fut = async {
        http.get(format!("https://registry.npmjs.org/{name}/latest"))
            .send()
            .await?
            .error_for_status()?
            .json::<NpmManifest>()
            .await
    };
    let point_fut = async {
        http.get(format!(
            "https://api.npmjs.org/downloads/point/last-month/{name}"
        ))
        .send()
        .await
        .ok()?
        .json::<NpmDownloadPoint>()
        .await
        .ok()?
        .downloads
    };
    let (manifest, from_search, point) = tokio::join!(
        manifest_fut,
        npm_search(http, name, 5),
        point_fut
    );

    // Start from the search hit (carries the last-publish date), then let the
    // authoritative manifest fill in / override the rest.
    let mut p = from_search
        .ok()
        .and_then(|v| v.into_iter().find(|p| p.name.eq_ignore_ascii_case(name)))
        .unwrap_or_else(|| Package::new(Ecosystem::Npm, name));

    if let Ok(m) = manifest {
        p.latest_version = m.version.clone();
        p.stable_version = m.version;
        p.deprecated = Some(matches!(&m.deprecated, Some(v) if !v.is_null()));
        p.min_runtime = m.engines.get("node").map(|n| format!("node {n}"));
        if let Some(d) = m.description {
            p.description = Some(d);
        }
        if !m.keywords.is_empty() {
            p.keywords = m.keywords;
        }
        if let Some(lic) = m.license.as_ref().and_then(extract_license) {
            p.license = Some(lic);
        }
        if let Some(h) = m.homepage {
            p.homepage = Some(h);
        }
        if let Some(url) = m.repository.and_then(|r| r.url) {
            p.repository = Some(url);
        }
    }

    if let Some(m) = point {
        p.recent_downloads_90d = Some(m.saturating_mul(3));
    }

    Ok(p)
}

// ===========================================================================
// PyPI (Python)
// ===========================================================================

#[derive(Debug, Deserialize)]
struct PyPiResp {
    info: PyPiInfo,
    /// Distribution files for the latest release (used for last-update time).
    #[serde(default)]
    urls: Vec<PyPiUrl>,
}

#[derive(Debug, Deserialize)]
struct PyPiUrl {
    upload_time_iso_8601: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PyPiInfo {
    summary: Option<String>,
    version: Option<String>,
    license: Option<String>,
    license_expression: Option<String>,
    home_page: Option<String>,
    #[serde(default)]
    project_urls: HashMap<String, String>,
    requires_python: Option<String>,
    keywords: Option<String>,
    #[serde(default)]
    yanked: bool,
}

#[derive(Debug, Deserialize)]
struct PyPiStats {
    data: Option<PyPiStatsData>,
}

#[derive(Debug, Deserialize)]
struct PyPiStatsData {
    last_month: Option<u64>,
}

fn pypi_repo_url(info: &PyPiInfo) -> Option<String> {
    for (k, v) in &info.project_urls {
        let kl = k.to_lowercase();
        if kl.contains("source") || kl.contains("repository") || kl.contains("github") || kl.contains("code") {
            return Some(v.clone());
        }
    }
    info.home_page.clone()
}

async fn pypi_get(http: &reqwest::Client, name: &str) -> anyhow::Result<Package> {
    // Fetch package metadata and download stats concurrently to cut latency.
    let meta_fut = async {
        http.get(format!("https://pypi.org/pypi/{name}/json"))
            .send()
            .await?
            .error_for_status()?
            .json::<PyPiResp>()
            .await
    };
    let (resp, downloads) = tokio::join!(meta_fut, pypi_downloads(http, name));
    let resp = resp?;
    let info = resp.info;

    let mut p = Package::new(Ecosystem::Python, name);
    p.description = info.summary.clone();
    p.keywords = info
        .keywords
        .as_deref()
        .map(|k| {
            k.split([',', ' '])
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    p.latest_version = info.version.clone();
    p.stable_version = info.version.clone();
    p.repository = pypi_repo_url(&info);
    p.homepage = info.home_page.clone();
    p.min_runtime = info.requires_python.clone().map(|r| format!("Python {r}"));
    p.deprecated = Some(info.yanked);
    p.license = info
        .license_expression
        .filter(|s| !s.is_empty())
        .or_else(|| info.license.filter(|s| !s.is_empty() && s.len() < 80));
    // Last update = newest distribution file's upload time (same response).
    p.updated_at = resp.urls.into_iter().find_map(|u| u.upload_time_iso_8601);
    // Popularity via pypistats (last 30 days → ~90d estimate).
    p.recent_downloads_90d = downloads.map(|m| m.saturating_mul(3));

    Ok(p)
}

async fn pypi_downloads(http: &reqwest::Client, name: &str) -> Option<u64> {
    let stats = http
        .get(format!("https://pypistats.org/api/packages/{name}/recent"))
        .send()
        .await
        .ok()?
        .json::<PyPiStats>()
        .await
        .ok()?;
    stats.data.and_then(|d| d.last_month)
}

/// PyPI has no public keyword-search API (the legacy XML-RPC search is gone and
/// the web search page is bot-blocked). We use deps.dev (Google's Open Source
/// Insights) to rank candidate names, then enrich each via PyPI's stable JSON
/// API. If deps.dev yields nothing we fall back to resolving the query as a
/// package name directly — so the tool degrades gracefully and never scrapes.
async fn pypi_search(
    http: &reqwest::Client,
    query: &str,
    limit: u32,
) -> anyhow::Result<Vec<Package>> {
    let mut names = deps_dev_names(http, query, "PYPI").await.unwrap_or_default();
    if names.is_empty() {
        names = pypi_name_candidates(query);
    }
    names.truncate((limit + 3) as usize);

    let mut out: Vec<Package> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for name in names {
        if out.len() >= limit as usize {
            break;
        }
        if let Ok(p) = pypi_get(http, &name).await {
            if seen.insert(p.name.to_lowercase()) {
                out.push(p);
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct DepsDevSearch {
    #[serde(default)]
    results: Vec<DepsDevResult>,
}

#[derive(Debug, Deserialize)]
struct DepsDevResult {
    name: String,
    system: Option<String>,
}

/// Query deps.dev's package search and return matching names for one ecosystem
/// (e.g. "PYPI", "NPM", "CARGO"), preserving relevance order and de-duplicating.
///
/// deps.dev matches on package *names* (not full-text), so a multi-word phrase
/// like "http requests" returns nothing. We therefore fall back to searching
/// each significant token, which surfaces canonical packages (e.g. "requests").
async fn deps_dev_names(
    http: &reqwest::Client,
    query: &str,
    system: &str,
) -> anyhow::Result<Vec<String>> {
    let mut seen = std::collections::HashSet::new();
    let mut names = Vec::new();

    let direct = deps_dev_search_once(http, query, system).await?;
    for n in direct {
        if seen.insert(n.to_lowercase()) {
            names.push(n);
        }
    }

    if names.is_empty() {
        for token in query
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() >= 3)
        {
            if let Ok(more) = deps_dev_search_once(http, token, system).await {
                for n in more {
                    if seen.insert(n.to_lowercase()) {
                        names.push(n);
                    }
                }
            }
        }
    }
    Ok(names)
}

async fn deps_dev_search_once(
    http: &reqwest::Client,
    query: &str,
    system: &str,
) -> anyhow::Result<Vec<String>> {
    let resp = http
        .get("https://deps.dev/_/search")
        .header(reqwest::header::USER_AGENT, BROWSER_UA)
        .query(&[("q", query)])
        .send()
        .await?
        .error_for_status()?
        .json::<DepsDevSearch>()
        .await?;

    Ok(resp
        .results
        .into_iter()
        .filter(|r| r.system.as_deref() == Some(system))
        .map(|r| r.name)
        .collect())
}

/// Generate likely PyPI package names from a free-text query (normalised
/// separators, joined forms, and individual significant tokens).
fn pypi_name_candidates(query: &str) -> Vec<String> {
    let q = query.trim().to_lowercase();
    let tokens: Vec<&str> = q.split_whitespace().collect();

    let mut v = Vec::new();
    let push = |s: String, v: &mut Vec<String>| {
        if !s.is_empty() && !v.contains(&s) {
            v.push(s);
        }
    };
    push(q.replace(' ', "-"), &mut v);
    push(q.replace(' ', "_"), &mut v);
    push(q.replace(' ', ""), &mut v);
    push(q.clone(), &mut v);
    // Individual tokens (>=3 chars) — the query word is often the package name.
    for t in tokens.iter().filter(|t| t.len() >= 3) {
        push((*t).to_string(), &mut v);
    }
    v.truncate(7);
    v
}

// ===========================================================================
// deps.dev v3 enrichment (shared by Go and Maven, which lack download stats)
// ===========================================================================

#[derive(Debug, Deserialize)]
struct DdPackage {
    #[serde(default)]
    versions: Vec<DdPackageVersion>,
}

#[derive(Debug, Deserialize)]
struct DdPackageVersion {
    #[serde(rename = "versionKey")]
    version_key: DdVersionKey,
    #[serde(rename = "isDefault", default)]
    is_default: bool,
}

#[derive(Debug, Deserialize)]
struct DdVersionKey {
    version: String,
}

#[derive(Debug, Deserialize)]
struct DdVersion {
    #[serde(rename = "publishedAt")]
    published_at: Option<String>,
    #[serde(rename = "isDeprecated", default)]
    is_deprecated: bool,
    #[serde(default)]
    licenses: Vec<String>,
    #[serde(default)]
    links: Vec<DdLink>,
    #[serde(rename = "relatedProjects", default)]
    related_projects: Vec<DdRelatedProject>,
}

#[derive(Debug, Deserialize)]
struct DdLink {
    label: Option<String>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DdRelatedProject {
    #[serde(rename = "projectKey")]
    project_key: Option<DdProjectKey>,
}

#[derive(Debug, Deserialize)]
struct DdProjectKey {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DdProject {
    #[serde(rename = "starsCount")]
    stars_count: Option<u64>,
    description: Option<String>,
}

/// Build a [`Package`] for a Go module or Maven coordinate via deps.dev v3:
/// default version + publish date + license + source repo, plus GitHub stars
/// (a popularity proxy) and description from the linked project.
async fn deps_dev_enrich(
    http: &reqwest::Client,
    eco: Ecosystem,
    system: &str,
    name: &str,
) -> anyhow::Result<Package> {
    let enc_name = pct_encode(name);
    let pkg = http
        .get(format!(
            "https://api.deps.dev/v3/systems/{system}/packages/{enc_name}"
        ))
        .send()
        .await?
        .error_for_status()?
        .json::<DdPackage>()
        .await?;

    let version = pkg
        .versions
        .iter()
        .find(|v| v.is_default)
        .or_else(|| pkg.versions.last())
        .map(|v| v.version_key.version.clone())
        .ok_or_else(|| anyhow::anyhow!("no versions found for {name}"))?;

    let mut p = Package::new(eco, name);
    p.latest_version = Some(version.clone());
    p.stable_version = Some(version.clone());

    let ver = http
        .get(format!(
            "https://api.deps.dev/v3/systems/{system}/packages/{enc_name}/versions/{}",
            pct_encode(&version)
        ))
        .send()
        .await?
        .error_for_status()?
        .json::<DdVersion>()
        .await?;

    p.updated_at = ver.published_at;
    p.deprecated = Some(ver.is_deprecated);
    p.license = ver.licenses.first().cloned();
    p.repository = ver
        .links
        .iter()
        .find(|l| l.label.as_deref() == Some("SOURCE_REPO"))
        .and_then(|l| l.url.clone());

    let project_id = ver
        .related_projects
        .iter()
        .find_map(|rp| rp.project_key.as_ref().and_then(|k| k.id.clone()))
        .or_else(|| {
            p.repository
                .as_deref()
                .and_then(|u| u.strip_prefix("https://"))
                .map(|s| s.trim_end_matches('/').to_string())
        });

    if let Some(id) = project_id {
        if let Ok(proj) = http
            .get(format!("https://api.deps.dev/v3/projects/{}", pct_encode(&id)))
            .send()
            .await
        {
            if let Ok(proj) = proj.json::<DdProject>().await {
                p.stars = proj.stars_count;
                if p.description.is_none() {
                    p.description = proj.description;
                }
            }
        }
    }

    Ok(p)
}

// ===========================================================================
// Go (pkg.go.dev) — search via deps.dev, details via deps.dev v3
// ===========================================================================

async fn go_search(
    http: &reqwest::Client,
    query: &str,
    limit: u32,
) -> anyhow::Result<Vec<Package>> {
    let names = deps_dev_names(http, query, "GO").await.unwrap_or_default();
    enrich_deps_dev_names(http, Ecosystem::Go, "go", names, limit).await
}

// ===========================================================================
// Maven Central (Java/Kotlin) — search via Maven Central, details via deps.dev
// ===========================================================================

#[derive(Debug, Deserialize)]
struct MavenSearchResp {
    response: MavenResponse,
}

#[derive(Debug, Deserialize)]
struct MavenResponse {
    #[serde(default)]
    docs: Vec<MavenDoc>,
}

#[derive(Debug, Deserialize)]
struct MavenDoc {
    id: String,
}

async fn maven_search(
    http: &reqwest::Client,
    query: &str,
    limit: u32,
) -> anyhow::Result<Vec<Package>> {
    let rows = (limit * 3).clamp(10, 30);
    let resp = http
        .get("https://search.maven.org/solrsearch/select")
        .query(&[
            ("q", query),
            ("rows", &rows.to_string()),
            ("wt", "json"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<MavenSearchResp>()
        .await?;

    let names: Vec<String> = resp.response.docs.into_iter().map(|d| d.id).collect();
    enrich_deps_dev_names(http, Ecosystem::Maven, "maven", names, limit).await
}

/// Enrich a list of deps.dev package names into full [`Package`]s, capped to a
/// small multiple of `limit` to bound the number of HTTP round-trips.
async fn enrich_deps_dev_names(
    http: &reqwest::Client,
    eco: Ecosystem,
    system: &str,
    mut names: Vec<String>,
    limit: u32,
) -> anyhow::Result<Vec<Package>> {
    names.truncate((limit + 3) as usize);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for name in names {
        if out.len() >= limit as usize {
            break;
        }
        if let Ok(p) = deps_dev_enrich(http, eco, system, &name).await {
            if seen.insert(p.name.to_lowercase()) {
                out.push(p);
            }
        }
    }
    Ok(out)
}

// ===========================================================================
// NuGet (.NET) — native search API (carries total downloads)
// ===========================================================================

#[derive(Debug, Deserialize)]
struct NugetSearchResp {
    #[serde(default)]
    data: Vec<NugetData>,
}

#[derive(Debug, Deserialize)]
struct NugetData {
    id: String,
    version: Option<String>,
    description: Option<String>,
    #[serde(rename = "totalDownloads")]
    total_downloads: Option<u64>,
    #[serde(rename = "licenseExpression")]
    license_expression: Option<String>,
    #[serde(rename = "projectUrl")]
    project_url: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    deprecation: Option<serde_json::Value>,
}

impl NugetData {
    fn into_package(self) -> Package {
        let mut p = Package::new(Ecosystem::Nuget, self.id);
        p.description = self.description;
        p.keywords = self.tags;
        p.total_downloads = self.total_downloads;
        p.latest_version = self.version.clone();
        p.stable_version = self.version;
        p.license = self.license_expression.filter(|s| !s.is_empty());
        p.repository = self.project_url;
        p.deprecated = Some(self.deprecation.is_some());
        p
    }
}

async fn nuget_query(
    http: &reqwest::Client,
    q: &str,
    take: u32,
) -> anyhow::Result<Vec<NugetData>> {
    let resp = http
        .get("https://azuresearch-usnc.nuget.org/query")
        .query(&[
            ("q", q),
            ("take", &take.to_string()),
            ("prerelease", "false"),
            ("semVerLevel", "2.0.0"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<NugetSearchResp>()
        .await?;
    Ok(resp.data)
}

async fn nuget_search(
    http: &reqwest::Client,
    query: &str,
    limit: u32,
) -> anyhow::Result<Vec<Package>> {
    let take = (limit * 3).clamp(10, 30);
    let data = nuget_query(http, query, take).await?;
    Ok(data.into_iter().map(NugetData::into_package).collect())
}

async fn nuget_get(http: &reqwest::Client, name: &str) -> anyhow::Result<Package> {
    let data = nuget_query(http, &format!("packageid:{name}"), 1).await?;
    data.into_iter()
        .next()
        .map(NugetData::into_package)
        .ok_or_else(|| anyhow::anyhow!("NuGet package '{name}' not found"))
}

// ===========================================================================
// OSV.dev — known security advisories (keyless)
// ===========================================================================

use crate::model::Vuln;

#[derive(Debug, Deserialize)]
struct OsvResp {
    #[serde(default)]
    vulns: Vec<OsvVuln>,
}

#[derive(Debug, Deserialize)]
struct OsvVuln {
    id: String,
    summary: Option<String>,
    #[serde(default)]
    database_specific: OsvDbSpecific,
}

#[derive(Debug, Default, Deserialize)]
struct OsvDbSpecific {
    severity: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OsvBatchResp {
    #[serde(default)]
    results: Vec<OsvBatchResult>,
}

#[derive(Debug, Deserialize)]
struct OsvBatchResult {
    #[serde(default)]
    vulns: Vec<OsvBatchVuln>,
}

#[derive(Debug, Deserialize)]
struct OsvBatchVuln {
    id: String,
}

/// OSV uses plain semver; Go module versions carry a leading "v".
fn osv_version(version: &str) -> &str {
    version.trim().trim_start_matches('v')
}

/// Detailed advisory lookup for a single package version.
pub async fn osv_query(
    http: &reqwest::Client,
    eco: Ecosystem,
    name: &str,
    version: &str,
) -> anyhow::Result<Vec<Vuln>> {
    let body = serde_json::json!({
        "version": osv_version(version),
        "package": { "name": name, "ecosystem": eco.osv_name() }
    });
    let resp = http
        .post("https://api.osv.dev/v1/query")
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json::<OsvResp>()
        .await?;
    Ok(resp
        .vulns
        .into_iter()
        .map(|v| Vuln {
            id: v.id,
            summary: v.summary,
            severity: v.database_specific.severity,
        })
        .collect())
}

/// Batched advisory check: returns the vulnerability id list for each input,
/// in the same order. Cheap way to flag many candidates with one request.
pub async fn osv_querybatch(
    http: &reqwest::Client,
    queries: &[(Ecosystem, String, String)],
) -> Vec<Vec<String>> {
    let empty = vec![Vec::new(); queries.len()];
    if queries.is_empty() {
        return empty;
    }
    let body = serde_json::json!({
        "queries": queries
            .iter()
            .map(|(eco, name, version)| serde_json::json!({
                "version": osv_version(version),
                "package": { "name": name, "ecosystem": eco.osv_name() }
            }))
            .collect::<Vec<_>>()
    });
    let parsed = async {
        http.post("https://api.osv.dev/v1/querybatch")
            .json(&body)
            .send()
            .await
            .ok()?
            .json::<OsvBatchResp>()
            .await
            .ok()
    }
    .await;

    match parsed {
        Some(resp) if resp.results.len() == queries.len() => resp
            .results
            .into_iter()
            .map(|r| r.vulns.into_iter().map(|v| v.id).collect())
            .collect(),
        _ => empty,
    }
}

// ===========================================================================
// Official MCP registry (for find_mcp_servers)
// ===========================================================================

#[derive(Debug, Clone)]
pub struct McpServer {
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub repository: Option<String>,
    pub install: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpServersResp {
    #[serde(default)]
    servers: Vec<McpServerEntry>,
}

#[derive(Debug, Deserialize)]
struct McpServerEntry {
    server: McpServerObj,
    #[serde(rename = "_meta")]
    meta: Option<McpMeta>,
}

#[derive(Debug, Deserialize)]
struct McpServerObj {
    name: String,
    description: Option<String>,
    version: Option<String>,
    repository: Option<McpRepo>,
    #[serde(default)]
    packages: Vec<McpPackage>,
}

#[derive(Debug, Deserialize)]
struct McpRepo {
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpPackage {
    #[serde(rename = "registryType")]
    registry_type: Option<String>,
    identifier: Option<String>,
    #[serde(rename = "runtimeHint")]
    runtime_hint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpMeta {
    #[serde(rename = "io.modelcontextprotocol.registry/official")]
    official: Option<McpOfficial>,
}

#[derive(Debug, Deserialize)]
struct McpOfficial {
    status: Option<String>,
}

/// Search the official MCP registry. Note: the registry's `search` matches on
/// server *names* (reverse-DNS) only, so broad capability words may miss; try
/// concrete terms (e.g. "postgres", "filesystem", "github").
pub async fn mcp_search(
    http: &reqwest::Client,
    query: &str,
    limit: u32,
) -> anyhow::Result<Vec<McpServer>> {
    let resp = http
        .get("https://registry.modelcontextprotocol.io/v0/servers")
        .query(&[
            ("search", query),
            ("limit", &limit.clamp(1, 30).to_string()),
            ("version", "latest"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<McpServersResp>()
        .await?;

    Ok(resp
        .servers
        .into_iter()
        .map(|e| {
            let install = e.server.packages.first().map(|pkg| {
                let id = pkg.identifier.as_deref().unwrap_or("?");
                let rt = pkg.registry_type.as_deref().unwrap_or("?");
                match pkg.runtime_hint.as_deref() {
                    Some(hint) => format!("{rt}: {id} (runtime: {hint})"),
                    None => format!("{rt}: {id}"),
                }
            });
            McpServer {
                name: e.server.name,
                description: e.server.description,
                version: e.server.version,
                repository: e.server.repository.and_then(|r| r.url),
                install,
                status: e.meta.and_then(|m| m.official).and_then(|o| o.status),
            }
        })
        .collect())
}
