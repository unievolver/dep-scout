//! Ecosystem-agnostic package model, reuse-quality scoring, and relevance.
//!
//! Every registry (crates.io, npm, PyPI, …) is normalised into a [`Package`] so
//! the scoring and ranking logic is written once and shared across languages.

use schemars::JsonSchema;
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Supported package ecosystems. Aliases let agents pass natural names
/// (e.g. "js", "frontend", "py") that map onto the canonical registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Ecosystem {
    /// Rust — crates.io. Aliases: rs, cargo, crates.
    #[serde(alias = "rs", alias = "cargo", alias = "crates", alias = "crates.io")]
    Rust,
    /// JavaScript / TypeScript / frontend — npm. Aliases: js, ts, javascript,
    /// typescript, node, nodejs, frontend, web.
    #[serde(
        alias = "js",
        alias = "ts",
        alias = "javascript",
        alias = "typescript",
        alias = "node",
        alias = "nodejs",
        alias = "frontend",
        alias = "web"
    )]
    Npm,
    /// Python — PyPI. Aliases: py, pip, pypi.
    #[serde(alias = "py", alias = "pip", alias = "pypi")]
    Python,
    /// Go — pkg.go.dev / Go module proxy. Aliases: golang.
    #[serde(alias = "golang")]
    Go,
    /// Java / Kotlin / JVM — Maven Central. Aliases: java, kotlin, gradle, jvm.
    #[serde(alias = "java", alias = "kotlin", alias = "gradle", alias = "jvm")]
    Maven,
    /// .NET / C# — NuGet. Aliases: dotnet, csharp, "c#", net.
    #[serde(alias = "dotnet", alias = "csharp", alias = "c#", alias = "net", alias = ".net")]
    Nuget,
}

impl Ecosystem {
    pub fn label(&self) -> &'static str {
        match self {
            Ecosystem::Rust => "Rust · crates.io",
            Ecosystem::Npm => "JS/TS · npm",
            Ecosystem::Python => "Python · PyPI",
            Ecosystem::Go => "Go · pkg.go.dev",
            Ecosystem::Maven => "Java/Kotlin · Maven",
            Ecosystem::Nuget => "·NET · NuGet",
        }
    }

    pub fn page_url(&self, name: &str) -> String {
        match self {
            Ecosystem::Rust => format!("https://crates.io/crates/{name}"),
            Ecosystem::Npm => format!("https://www.npmjs.com/package/{name}"),
            Ecosystem::Python => format!("https://pypi.org/project/{name}/"),
            Ecosystem::Go => format!("https://pkg.go.dev/{name}"),
            // Maven names are "group:artifact"; the site path uses a slash.
            Ecosystem::Maven => {
                format!("https://central.sonatype.com/artifact/{}", name.replacen(':', "/", 1))
            }
            Ecosystem::Nuget => format!("https://www.nuget.org/packages/{name}"),
        }
    }

    pub fn docs_url(&self, name: &str) -> Option<String> {
        match self {
            Ecosystem::Rust => Some(format!("https://docs.rs/{name}")),
            Ecosystem::Go => Some(format!("https://pkg.go.dev/{name}")),
            _ => None,
        }
    }

    pub fn install_hint(&self, name: &str) -> String {
        match self {
            Ecosystem::Rust => format!("cargo add {name}"),
            Ecosystem::Npm => format!("npm install {name}"),
            Ecosystem::Python => format!("pip install {name}"),
            Ecosystem::Go => format!("go get {name}"),
            Ecosystem::Maven => format!("加入 Maven/Gradle 依赖: {name}"),
            Ecosystem::Nuget => format!("dotnet add package {name}"),
        }
    }

    /// What a deprecated/withdrawn latest release is called in this ecosystem.
    fn deprecation_term(&self) -> &'static str {
        match self {
            Ecosystem::Rust => "yank（撤回）",
            Ecosystem::Npm => "deprecated（弃用）",
            Ecosystem::Python => "yank（撤回）",
            Ecosystem::Go => "deprecated（弃用）",
            Ecosystem::Maven => "deprecated（弃用）",
            Ecosystem::Nuget => "deprecated/unlisted（弃用）",
        }
    }
}

/// Normalised facts about a single package, across any ecosystem.
#[derive(Debug, Clone)]
pub struct Package {
    pub ecosystem: Ecosystem,
    pub name: String,
    pub description: Option<String>,
    pub keywords: Vec<String>,
    /// Downloads normalised to a ~90-day window for cross-registry comparison.
    pub recent_downloads_90d: Option<u64>,
    pub total_downloads: Option<u64>,
    /// GitHub stars — a popularity proxy for ecosystems without download stats
    /// (Go, Maven).
    pub stars: Option<u64>,
    /// Last-update timestamp, RFC3339 if available.
    pub updated_at: Option<String>,
    pub latest_version: Option<String>,
    pub stable_version: Option<String>,
    pub repository: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub deprecated: Option<bool>,
    /// MSRV (Rust), engines.node (npm), or requires_python (PyPI).
    pub min_runtime: Option<String>,
}

impl Package {
    pub fn new(ecosystem: Ecosystem, name: impl Into<String>) -> Self {
        Package {
            ecosystem,
            name: name.into(),
            description: None,
            keywords: Vec::new(),
            recent_downloads_90d: None,
            total_downloads: None,
            stars: None,
            updated_at: None,
            latest_version: None,
            stable_version: None,
            repository: None,
            homepage: None,
            license: None,
            deprecated: None,
            min_runtime: None,
        }
    }

    pub fn display_version(&self) -> &str {
        self.stable_version
            .as_deref()
            .or(self.latest_version.as_deref())
            .unwrap_or("?")
    }
}

/// The result of scoring a package for reuse suitability.
#[derive(Debug, Clone)]
pub struct Quality {
    pub score: u32,
    pub verdict: &'static str,
    pub signals: Vec<String>,
    pub warnings: Vec<String>,
}

fn months_since(date: &str) -> Option<f64> {
    let parsed = OffsetDateTime::parse(date, &Rfc3339).ok()?;
    let now = OffsetDateTime::now_utc();
    let days = (now - parsed).whole_days();
    Some(days as f64 / 30.4)
}

fn major_of(version: &str) -> Option<u64> {
    let v = version.trim().trim_start_matches(['v', 'V']);
    v.split('.').next()?.parse::<u64>().ok()
}

/// Score a package 0–100 on popularity, maintenance, stability and metadata.
pub fn score(p: &Package) -> Quality {
    let mut score: i64 = 0;
    let mut signals = Vec::new();
    let mut warnings = Vec::new();

    // --- Popularity (max 40). Prefer downloads; fall back to GitHub stars for
    // ecosystems without download stats (Go, Maven); else a neutral baseline. ---
    let download_basis = p
        .recent_downloads_90d
        .or_else(|| p.total_downloads.map(|t| t / 8));
    if let Some(basis) = download_basis {
        let popularity = match basis {
            d if d >= 5_000_000 => 40,
            d if d >= 500_000 => 36,
            d if d >= 100_000 => 30,
            d if d >= 20_000 => 24,
            d if d >= 5_000 => 18,
            d if d >= 1_000 => 12,
            d if d >= 100 => 6,
            _ => 2,
        };
        score += popularity;
        if basis >= 100_000 {
            signals.push(format!("人气高（近90天约 {} 次下载）", fmt_num(basis)));
        } else if basis < 1_000 {
            warnings.push(format!("下载量偏低（近90天约 {} 次）", fmt_num(basis)));
        }
    } else if let Some(stars) = p.stars {
        let popularity = match stars {
            s if s >= 20_000 => 40,
            s if s >= 5_000 => 34,
            s if s >= 1_000 => 28,
            s if s >= 200 => 20,
            s if s >= 50 => 12,
            s if s >= 10 => 6,
            _ => 2,
        };
        score += popularity;
        if stars >= 1_000 {
            signals.push(format!("人气高（GitHub {} stars）", fmt_num(stars)));
        } else if stars < 50 {
            warnings.push(format!("关注度偏低（GitHub {} stars）", fmt_num(stars)));
        }
    } else {
        // No popularity data available for this ecosystem — stay neutral rather
        // than capping the verdict artificially low.
        score += 18;
        signals.push("（该来源无公开人气数据，人气分按中性计）".to_string());
    }

    // --- Maintenance (max 30), from last-update recency. ---
    match p.updated_at.as_deref().and_then(months_since) {
        Some(m) if m <= 3.0 => {
            score += 30;
            signals.push("维护活跃（近 3 个月内有更新）".to_string());
        }
        Some(m) if m <= 6.0 => score += 24,
        Some(m) if m <= 12.0 => score += 16,
        Some(m) if m <= 18.0 => {
            score += 10;
            warnings.push(format!("已约 {:.0} 个月未更新", m));
        }
        Some(m) if m <= 24.0 => {
            score += 5;
            warnings.push(format!("已约 {:.0} 个月未更新，维护存疑", m));
        }
        Some(m) => warnings.push(format!("已约 {:.0} 个月未更新，疑似停止维护", m)),
        None => score += 8,
    }

    // --- Stability (max 15). ---
    match p.stable_version.as_deref().and_then(major_of) {
        Some(major) if major >= 1 => {
            score += 15;
            signals.push(format!("已发布稳定版 v{}.x", major));
        }
        _ => {
            score += 9;
            warnings.push("仍是 0.x 版本，API 可能有破坏性变更".to_string());
        }
    }
    if let Some(v) = p.latest_version.as_deref() {
        if v.contains('-') {
            warnings.push(format!("最新版 {v} 是预发布版（alpha/beta/rc）"));
        }
    }

    // --- Metadata completeness (max 15). ---
    let mut meta = 0;
    if p.repository.is_some() {
        meta += 7;
    } else {
        warnings.push("未提供源码仓库链接".to_string());
    }
    if p.homepage.is_some() || p.ecosystem.docs_url(&p.name).is_some() {
        meta += 4;
    }
    if p.license.is_some() {
        meta += 4;
    }
    score += meta.min(15);

    // --- Hard penalty. ---
    if p.deprecated == Some(true) {
        score -= 40;
        warnings.push(format!(
            "⚠️ 最新版本已被 {}，不要直接依赖",
            p.ecosystem.deprecation_term()
        ));
    }

    let score = score.clamp(0, 100) as u32;
    let verdict = match score {
        78..=100 => "✅ 强烈推荐复用",
        58..=77 => "🟡 可用，但留意下方提示",
        38..=57 => "🟠 谨慎，建议对比替代方案",
        _ => "🔴 不建议，优先找更成熟的替代",
    };

    Quality {
        score,
        verdict,
        signals,
        warnings,
    }
}

/// Tokenise a query into lowercase alphanumeric terms of length >= 2.
fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_string())
        .collect()
}

/// Fraction (0..1) of query terms that appear in the package's name,
/// description or keywords — a cheap textual relevance signal.
pub fn term_overlap(query: &str, p: &Package) -> f64 {
    let terms = tokenize(query);
    if terms.is_empty() {
        return 0.0;
    }
    let mut hay = p.name.to_lowercase();
    if let Some(d) = &p.description {
        hay.push(' ');
        hay.push_str(&d.to_lowercase());
    }
    for k in &p.keywords {
        hay.push(' ');
        hay.push_str(&k.to_lowercase());
    }
    let hits = terms.iter().filter(|t| hay.contains(t.as_str())).count();
    hits as f64 / terms.len() as f64
}

/// True if the package name exactly equals one of the query terms — a strong
/// signal the user is looking for this exact package.
pub fn name_is_exact(query: &str, p: &Package) -> bool {
    let name = p.name.to_lowercase();
    tokenize(query).iter().any(|t| *t == name) || query.trim().to_lowercase() == name
}

pub fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}
