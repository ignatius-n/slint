// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

//! Generate a third-party license listing.
//!
//! This is a self-contained replacement for `cargo about`. It fetches the
//! dependencies into the cargo cache (`cargo fetch`), resolves the dependency
//! graph across all target platforms via `cargo metadata`, restricting it to
//! the features actually enabled in the analyzed crate, and then harvests the
//! license texts directly from the crate sources in the cache.

// cSpell: ignore licence rsplit

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// SPDX license identifiers we accept for third-party dependencies. A crate is
/// rejected unless its license expression is satisfiable by this set.
const ACCEPTED: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "MPL-2.0",
    "Zlib",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "CC0-1.0",
    "BSL-1.0",
    "ISC",
    "Unicode-DFS-2016",
    "Unicode-3.0",
    "OpenSSL",
    "Unlicense",
    "WTFPL",
];

/// Skip dependencies reachable only through dev-dependency edges.
const IGNORE_DEV_DEPENDENCIES: bool = true;

#[derive(Debug, clap::Parser)]
pub struct LicenseCommand {
    /// Path to the `Cargo.toml` whose dependencies should be analyzed.
    /// Defaults to `Cargo.toml` in the current directory.
    #[arg(long)]
    manifest_path: Option<PathBuf>,
    /// Comma-separated features to enable on the analyzed crate (repeatable).
    #[arg(long, value_delimiter = ',')]
    features: Vec<String>,
    /// Do not enable the analyzed crate's default features.
    #[arg(long)]
    no_default_features: bool,
    /// Enable all features of the analyzed crate.
    #[arg(long)]
    all_features: bool,
    /// Where to write the result. Writes to stdout when omitted.
    #[arg(short = 'o', long)]
    output: Option<PathBuf>,
}

impl LicenseCommand {
    pub fn run(&self) -> anyhow::Result<()> {
        let manifest_path = match &self.manifest_path {
            Some(p) => std::env::current_dir()?.join(p),
            None => std::env::current_dir()?.join("Cargo.toml"),
        };
        generate(&GenerateArgs {
            manifest_path,
            features: Features {
                features: self.features.clone(),
                no_default_features: self.no_default_features,
                all_features: self.all_features,
            },
            output: self.output.clone(),
        })
    }
}

/// Which features to enable on the analyzed crate, mirroring cargo's flags.
#[derive(Debug, Default)]
pub struct Features {
    pub features: Vec<String>,
    pub no_default_features: bool,
    pub all_features: bool,
}

pub struct GenerateArgs {
    pub manifest_path: PathBuf,
    pub features: Features,
    pub output: Option<PathBuf>,
}

pub fn generate(args: &GenerateArgs) -> anyhow::Result<()> {
    let accepted: HashSet<&str> = ACCEPTED.iter().copied().collect();

    fetch_dependencies(&args.manifest_path)?;

    let packages = resolve_packages(&args.manifest_path, &args.features)?;

    // Harvest, per license id, the distinct license/notice texts (deduplicated)
    // so every required copyright notice is reproduced, and build one summary
    // row per crate for the dependency tables.
    let mut license_names: BTreeMap<String, String> = BTreeMap::new();
    let mut texts: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut rows: Vec<CrateRow> = Vec::new();

    for (pkg, usage) in &packages {
        let Some(expression) = pkg.license.clone() else {
            bail!("Crate {} {} has no license information", pkg.name, pkg.version);
        };

        // Lax parsing accepts the deprecated `/` OR-separator and imprecise
        // identifiers (e.g. `apache2`) still found in older crates.
        let expr =
            spdx::Expression::parse_mode(&expression, spdx::ParseMode::LAX).with_context(|| {
                format!("Cannot parse license `{expression}` of {} {}", pkg.name, pkg.version)
            })?;

        if !expr.evaluate(|req| accepted.contains(license_string(req).as_str())) {
            bail!(
                "License `{expression}` of crate {} {} is not in the accepted list",
                pkg.name,
                pkg.version
            );
        }

        let crate_dir = pkg.manifest_path.parent().expect("manifest has a parent");
        let files = license_files(crate_dir.as_std_path());

        // A crate contributes its text under every accepted license id mentioned
        // in its expression. For `A OR B` / `A AND B` it contributes to both.
        let ids: BTreeSet<(String, String)> = expr
            .requirements()
            .filter_map(|r| {
                let id = license_string(&r.req);
                accepted.contains(id.as_str()).then(|| (id, license_full_name(&r.req)))
            })
            .collect();

        for (id, full_name) in ids {
            license_names.entry(id.clone()).or_insert(full_name);
            // Use the crate's own license file when present, otherwise fall back
            // to a notice synthesized from its authors / repository so the
            // copyright attribution is not lost.
            let text = match find_license_text(&id, &files) {
                Some(text) => normalize(&text),
                None => fallback_text(&id, pkg),
            };
            texts.entry(id).or_default().insert(text);
        }

        rows.push(CrateRow {
            name: pkg.name.to_string(),
            version: pkg.version.to_string(),
            repository: pkg.repository.clone(),
            author: pkg.authors.join(", "),
            license: expression,
            usage: *usage,
        });
    }

    // One section per license id, holding its distinct texts.
    let mut sections: Vec<LicenseSection> = texts
        .into_iter()
        .map(|(id, texts)| LicenseSection {
            name: license_names.remove(&id).unwrap_or_else(|| id.clone()),
            id,
            texts: texts.into_iter().collect(),
        })
        .collect();
    sections.sort_by(|a, b| a.name.cmp(&b.name));

    let rendered = render_markdown(&rows, &sections);

    match &args.output {
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(path, rendered)
                .with_context(|| format!("Cannot write {}", path.display()))?;
        }
        None => print!("{rendered}"),
    }

    Ok(())
}

/// How a third-party crate is reached from the analyzed crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Usage {
    /// Linked into the program.
    Runtime,
    /// Used both at build time and linked into the program.
    Shared,
    /// Only used at build time (build scripts, proc-macros) and not shipped.
    Build,
}

/// One crate's row in a dependency table.
struct CrateRow {
    name: String,
    version: String,
    repository: Option<String>,
    author: String,
    license: String,
    usage: Usage,
}

/// All distinct license/notice texts gathered under one license id.
struct LicenseSection {
    id: String,
    name: String,
    texts: Vec<String>,
}

/// Run `cargo fetch` (for all targets) so that the relevant crate sources are
/// present in the cargo cache before we harvest from it.
fn fetch_dependencies(manifest_path: &Path) -> anyhow::Result<()> {
    let manifest = manifest_path.to_str().context("Non-UTF-8 manifest path")?;
    run_cargo(&["fetch", "--manifest-path", manifest])?;
    Ok(())
}

fn run_cargo(args: &[&str]) -> anyhow::Result<()> {
    let status = std::process::Command::new(std::env::var("CARGO").as_deref().unwrap_or("cargo"))
        .args(args)
        .status()
        .with_context(|| format!("Failed to run cargo {}", args.join(" ")))?;
    if !status.success() {
        bail!("cargo {} failed", args.join(" "));
    }
    Ok(())
}

/// Resolve the dependency packages across all target platforms, restricted to
/// the features enabled in the analyzed crate and honoring
/// `IGNORE_DEV_DEPENDENCIES`, classifying each into runtime / build / shared.
///
/// `cargo metadata` is run without `--filter-platform`, so its resolve graph
/// spans every target. Its graph keeps optional dependencies even when no
/// active feature enables them, so the walk follows only the edges that the
/// resolved feature set of each crate actually activates.
///
/// The walk carries a "build" flag: it starts in runtime mode at the root and
/// switches to build mode when it crosses a `[build-dependencies]` edge or
/// enters a proc-macro crate (neither is linked into the program). A crate
/// reachable in both modes is `Shared`.
fn resolve_packages(
    manifest_path: &Path,
    features: &Features,
) -> anyhow::Result<Vec<(cargo_metadata::Package, Usage)>> {
    let mut cmd = cargo_metadata::MetadataCommand::new();
    cmd.manifest_path(manifest_path);
    if !features.features.is_empty() {
        cmd.features(cargo_metadata::CargoOpt::SomeFeatures(features.features.clone()));
    }
    if features.no_default_features {
        cmd.features(cargo_metadata::CargoOpt::NoDefaultFeatures);
    }
    if features.all_features {
        cmd.features(cargo_metadata::CargoOpt::AllFeatures);
    }
    let metadata = cmd.exec()?;

    let packages: HashMap<cargo_metadata::PackageId, cargo_metadata::Package> =
        metadata.packages.iter().map(|p| (p.id.clone(), p.clone())).collect();

    let resolve = metadata.resolve.context("cargo metadata returned no resolve graph")?;
    let nodes: HashMap<_, _> = resolve.nodes.iter().map(|n| (n.id.clone(), n)).collect();

    // Seed the walk with the package the manifest points at. For a virtual
    // workspace manifest there is no single root, so fall back to all members.
    // Seeding from `resolve.root` (rather than every workspace member) ensures
    // we only consider the dependencies of the analyzed crate, not those of
    // unrelated examples elsewhere in the workspace.
    let seeds: Vec<_> = match resolve.root.clone() {
        Some(root) => vec![root],
        None => metadata.workspace_members.clone(),
    };
    // The workspace's own crates are first-party, not third-party: we traverse
    // through them to reach their dependencies but never list them.
    let workspace: HashSet<&cargo_metadata::PackageId> =
        metadata.workspace_members.iter().collect();

    // Third-party crates reached in runtime mode and in build mode (the same
    // crate can appear in both). `false` = runtime mode, `true` = build mode.
    let mut runtime: HashSet<cargo_metadata::PackageId> = HashSet::new();
    let mut build: HashSet<cargo_metadata::PackageId> = HashSet::new();
    let mut seen: HashSet<(cargo_metadata::PackageId, bool)> = HashSet::new();
    let mut stack: Vec<(cargo_metadata::PackageId, bool)> =
        seeds.into_iter().map(|id| (id, false)).collect();
    while let Some((id, build_mode)) = stack.pop() {
        if !seen.insert((id.clone(), build_mode)) {
            continue;
        }
        if !workspace.contains(&id) {
            if build_mode { &mut build } else { &mut runtime }.insert(id.clone());
        }
        let (Some(node), Some(parent)) = (nodes.get(&id), packages.get(&id)) else { continue };
        let active: HashSet<&str> = node.features.iter().map(String::as_str).collect();
        for dep in &node.deps {
            let non_dev: Vec<_> = dep
                .dep_kinds
                .iter()
                .filter(|k| k.kind != cargo_metadata::DependencyKind::Development)
                .collect();
            // Drop dev-only edges.
            if IGNORE_DEV_DEPENDENCIES && !dep.dep_kinds.is_empty() && non_dev.is_empty() {
                continue;
            }
            // The non-dev kinds that apply to a real target platform (ignoring
            // synthetic build cfgs such as `cfg(fuzzing)`/`cfg(test)`/`cfg(miri)`,
            // which are never active in a normal build).
            let effective: Vec<_> = non_dev
                .iter()
                .filter(|k| !k.target.as_ref().is_some_and(|t| is_synthetic_cfg(&t.to_string())))
                .collect();
            // Drop edges that only exist under such synthetic cfgs.
            if !non_dev.is_empty() && effective.is_empty() {
                continue;
            }
            let Some(dep_pkg) = packages.get(&dep.pkg) else { continue };
            if !dependency_enabled(parent, &active, dep_pkg.name.as_str()) {
                continue;
            }
            // The edge stays runtime-capable if it carries a normal kind (or the
            // resolver gave us no kinds at all); a build-only edge or a
            // proc-macro target moves into build mode.
            let edge_is_build = !effective.is_empty()
                && effective.iter().all(|k| k.kind == cargo_metadata::DependencyKind::Build);
            let next_build = build_mode || edge_is_build || is_proc_macro(dep_pkg);
            stack.push((dep.pkg.clone(), next_build));
        }
    }

    let mut packages = packages;
    let all: BTreeSet<_> = runtime.iter().chain(build.iter()).cloned().collect();
    Ok(all
        .into_iter()
        .filter_map(|id| {
            let usage = match (runtime.contains(&id), build.contains(&id)) {
                (true, true) => Usage::Shared,
                (true, false) => Usage::Runtime,
                _ => Usage::Build,
            };
            packages.remove(&id).map(|pkg| (pkg, usage))
        })
        .collect())
}

/// Whether a package is a proc-macro crate (compiled for the host and not
/// linked into the program, hence a build-time dependency).
fn is_proc_macro(pkg: &cargo_metadata::Package) -> bool {
    pkg.targets.iter().any(|t| t.is_proc_macro())
}

/// Whether a `cfg(...)` target predicate references a synthetic build flag
/// (`fuzzing`, `test`, `miri`, `clippy`) that is never set for a real target
/// platform in a normal build. Quoted values (e.g. `target_os = "..."`) are
/// ignored so only bare cfg flags are matched.
fn is_synthetic_cfg(target: &str) -> bool {
    let mut unquoted = String::with_capacity(target.len());
    let mut in_quote = false;
    for c in target.chars() {
        match c {
            '"' => in_quote = !in_quote,
            _ if !in_quote => unquoted.push(c),
            _ => {}
        }
    }
    unquoted
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|tok| matches!(tok, "fuzzing" | "test" | "miri" | "clippy"))
}

/// Whether `parent` actually pulls in the dependency crate named `dep_name`
/// given its resolved set of `active` features. A non-optional (non-dev)
/// dependency is always pulled in; an optional one only if some active feature
/// activates it (`name` / `dep:name` / `name/feature`, but not the weak
/// `name?/feature`).
fn dependency_enabled(
    parent: &cargo_metadata::Package,
    active: &HashSet<&str>,
    dep_name: &str,
) -> bool {
    let mut matched = false;
    for dep in &parent.dependencies {
        if dep.kind == cargo_metadata::DependencyKind::Development || dep.name != dep_name {
            continue;
        }
        matched = true;
        if !dep.optional {
            return true;
        }
        // The feature gate uses the dependency's local name (its rename if any).
        let key = dep.rename.as_deref().unwrap_or(&dep.name);
        if active.contains(key) {
            return true;
        }
        let enables =
            |value: &str| value == format!("dep:{key}") || value.starts_with(&format!("{key}/"));
        if active
            .iter()
            .any(|f| parent.features.get(*f).is_some_and(|v| v.iter().any(|x| enables(x))))
        {
            return true;
        }
    }
    // If no matching manifest entry was found (unusual), keep the dependency.
    !matched
}

/// The canonical SPDX string for a requirement, e.g. `MIT` or
/// `LicenseRef-Slint-Software-3.0`.
fn license_string(req: &spdx::LicenseReq) -> String {
    match &req.license {
        spdx::LicenseItem::Spdx { id, .. } => id.name.to_string(),
        spdx::LicenseItem::Other { doc_ref, lic_ref } => match doc_ref {
            Some(doc_ref) => format!("DocumentRef-{doc_ref}:LicenseRef-{lic_ref}"),
            None => format!("LicenseRef-{lic_ref}"),
        },
    }
}

fn license_full_name(req: &spdx::LicenseReq) -> String {
    match &req.license {
        spdx::LicenseItem::Spdx { id, .. } => id.full_name.to_string(),
        spdx::LicenseItem::Other { .. } => license_string(req),
    }
}

/// Collect candidate license files from a crate directory, both the
/// conventional top-level `LICENSE*`/`COPYING*` files and any REUSE-style
/// `LICENSES/<id>.*` files.
fn license_files(dir: &Path) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let mut collect = |path: &Path| {
        if let (Ok(text), Some(name)) =
            (std::fs::read_to_string(path), path.file_name().and_then(|n| n.to_str()))
        {
            result.push((name.to_string(), text));
        }
    };

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let name = entry.file_name().to_string_lossy().to_ascii_uppercase();
                if name.starts_with("LICENSE")
                    || name.starts_with("LICENCE")
                    || name.starts_with("COPYING")
                    || name.starts_with("COPYRIGHT")
                    || name.starts_with("UNLICENSE")
                    || name.starts_with("NOTICE")
                {
                    collect(&path);
                }
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("LICENSES")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                collect(&path);
            }
        }
    }
    result
}

/// Suffix found on a license file name (after `LICENSE-`/`LICENSE.`) mapped to
/// the SPDX id it most likely refers to.
fn suffix_to_id(suffix: &str) -> Option<&'static str> {
    Some(match suffix.to_ascii_uppercase().as_str() {
        "APACHE" | "APACHE2" | "APACHE-2.0" | "APACHE2.0" => "Apache-2.0",
        "MIT" => "MIT",
        "ZLIB" => "Zlib",
        "MPL" | "MPL-2.0" | "MPL2" => "MPL-2.0",
        "ISC" => "ISC",
        "BSL" | "BOOST" => "BSL-1.0",
        "UNICODE" => "Unicode-3.0",
        _ => return None,
    })
}

/// Find the license text for a given license id among the crate's license
/// files. Matches REUSE files by stem, conventional files by suffix, and falls
/// back to a single generic license file when the crate has only one license.
fn find_license_text(id: &str, files: &[(String, String)]) -> Option<String> {
    // Exact REUSE match: `LICENSES/<id>.txt` or `<id>.md`.
    for (name, text) in files {
        let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
        if stem.eq_ignore_ascii_case(id) {
            return Some(text.clone());
        }
    }
    // Conventional `LICENSE-<SUFFIX>` / `LICENSE.<SUFFIX>` files.
    for (name, text) in files {
        let upper = name.to_ascii_uppercase();
        let suffix = upper
            .strip_prefix("LICENSE-")
            .or_else(|| upper.strip_prefix("LICENSE."))
            .or_else(|| upper.strip_prefix("LICENCE-"))
            .or_else(|| upper.strip_prefix("LICENCE."));
        if let Some(suffix) = suffix {
            let suffix = suffix.rsplit_once('.').map(|(s, _)| s).unwrap_or(suffix);
            if suffix_to_id(suffix) == Some(id) {
                return Some(text.clone());
            }
        }
    }
    None
}

/// Synthesize a notice for a crate that ships no license file: the copyright
/// holders taken from the crate's `authors` (falling back to its repository or
/// name) above the canonical SPDX license body, so the attribution required by
/// permissive licenses is still reproduced.
fn fallback_text(id: &str, pkg: &cargo_metadata::Package) -> String {
    let holder = if !pkg.authors.is_empty() {
        pkg.authors.join(", ")
    } else if let Some(repository) = &pkg.repository {
        repository.clone()
    } else {
        pkg.name.to_string()
    };
    let body = spdx::license_id(id)
        .map(|l| l.text().to_string())
        .unwrap_or_else(|| format!("No license text available for {id}."));
    normalize(&format!("Copyright (c) {holder}\n\n{body}"))
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").trim().to_string()
}

fn crate_url(name: &str, repository: &Option<String>) -> String {
    match repository {
        Some(repo) => repo.clone(),
        None => format!("https://crates.io/crates/{name}"),
    }
}

/// Escape a value for use inside a Markdown table cell: `|` would otherwise end
/// the cell and newlines would break the row.
fn table_cell(value: &str) -> String {
    value.replace('\\', "\\\\").replace('|', "\\|").replace(['\n', '\r'], " ")
}

/// Render the dependency tables followed by the license texts as Markdown. The
/// output carries no front matter so it can be shipped verbatim in the binary
/// packages; the docs site generators (`docs/common/src/utils/thirdparty.ts`)
/// prepend the Starlight front matter they need.
fn render_markdown(rows: &[CrateRow], sections: &[LicenseSection]) -> String {
    let mut out = String::new();
    let ids: HashSet<&str> = sections.iter().map(|s| s.id.as_str()).collect();
    render_table(&mut out, "Runtime dependencies", rows, Usage::Runtime, &ids);
    render_table(&mut out, "Shared dependencies", rows, Usage::Shared, &ids);
    render_table(&mut out, "Build dependencies", rows, Usage::Build, &ids);

    out.push_str("## License texts\n\n");
    for s in sections {
        out.push_str(&format!("### <a id=\"{id}\"></a> {name}\n\n", id = s.id, name = s.name));
        for text in &s.texts {
            out.push_str(&format!("```\n{}\n```\n\n", text.trim_end()));
        }
    }
    out
}

/// Render one dependency table (Name / Author / License) for the crates with
/// the given usage, sorted by name and version. License ids found in `ids` are
/// linked to their text section below.
fn render_table(
    out: &mut String,
    title: &str,
    rows: &[CrateRow],
    usage: Usage,
    ids: &HashSet<&str>,
) {
    out.push_str(&format!("## {title}\n\n"));
    let mut selected: Vec<&CrateRow> = rows.iter().filter(|r| r.usage == usage).collect();
    selected.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));
    if selected.is_empty() {
        out.push_str("_None._\n\n");
        return;
    }
    out.push_str("| Name | Author / Copyright | License |\n| --- | --- | --- |\n");
    for r in selected {
        let author = if r.author.is_empty() { "—" } else { r.author.as_str() };
        out.push_str(&format!(
            "| [{name} {version}]({url}) | {author} | {license} |\n",
            name = table_cell(&r.name),
            version = table_cell(&r.version),
            url = crate_url(&r.name, &r.repository),
            author = table_cell(author),
            license = linkify_license(&r.license, ids),
        ));
    }
    out.push('\n');
}

/// Turn each known SPDX license id in `expression` into a link to its text
/// section, leaving operators (`OR`/`AND`/`WITH`), parentheses and unknown
/// tokens untouched. SPDX expressions contain no `|`, so no cell escaping is
/// needed.
fn linkify_license(expression: &str, ids: &HashSet<&str>) -> String {
    let mut out = String::new();
    let mut token = String::new();
    for c in expression.chars() {
        if c.is_alphanumeric() || matches!(c, '.' | '-' | '+') {
            token.push(c);
        } else {
            push_license_token(&mut out, &token, ids);
            token.clear();
            out.push(c);
        }
    }
    push_license_token(&mut out, &token, ids);
    out
}

fn push_license_token(out: &mut String, token: &str, ids: &HashSet<&str>) {
    if token.is_empty() {
        // nothing to flush
    } else if ids.contains(token) {
        out.push_str(&format!("[{token}](#{token})"));
    } else {
        out.push_str(token);
    }
}
