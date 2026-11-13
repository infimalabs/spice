//! Package roots derived from the selected tree's own packaging metadata.
//!
//! A repository that already tells Python where its packages live should not
//! have to tell the gate a second time. When tracked policy declares no
//! `package_roots`, the roots are read from the captured `pyproject.toml`
//! instead, and only a repository whose metadata names nothing at all is
//! genuinely outside this arm's jurisdiction.
//!
//! Each backend is tried in one fixed precedence and the first one whose table
//! is present answers, including when its answer is "no roots". A backend that
//! is present but malformed refuses rather than falling through: falling
//! through would answer with a different backend's opinion, and a silent
//! fallthrough to nothing would turn a broken declaration into a skipped arm.

use toml::Value;

use super::super::CoreInputError;
use super::TreeIndex;
use crate::policy::PathPattern;

/// Package roots this tree's packaging metadata names, in backend precedence.
///
/// # Errors
///
/// Returns the first present-but-malformed backend table. An absent table is
/// not malformed and is skipped in silence.
pub(super) fn derived_package_roots(
    pyproject: &Value,
    tree: &TreeIndex<'_>,
) -> Result<Vec<String>, CoreInputError> {
    let tool = table(pyproject, "tool");
    for resolver in [
        setuptools_roots,
        poetry_roots,
        hatch_roots,
        flit_roots,
        pdm_roots,
    ] {
        if let Some(roots) = resolver(tool, tree)? {
            return Ok(roots);
        }
    }
    layout_fallback_roots(pyproject, tree)
}

fn setuptools_roots(
    tool: Option<&toml::Table>,
    tree: &TreeIndex<'_>,
) -> Result<Option<Vec<String>>, CoreInputError> {
    let Some(setuptools) = tool
        .and_then(|tool| tool.get("setuptools"))
        .and_then(Value::as_table)
    else {
        return Ok(None);
    };
    let Some(packages) = setuptools.get("packages") else {
        return Ok(None);
    };
    if let Some(entries) = packages.as_array() {
        let names = strings("[tool.setuptools].packages", entries)?;
        // A distribution names packages, not directories: only the first dotted
        // component is a root, and the rest live inside it.
        return Ok(Some(dedupe(names.iter().map(|name| {
            name.split_once('.')
                .map_or(name.as_str(), |(root, _)| root)
                .trim()
                .to_owned()
        }))));
    }
    if let Some(find) = packages
        .as_table()
        .and_then(|packages| packages.get("find"))
    {
        let find = find.as_table().ok_or_else(|| {
            CoreInputError::new("[tool.setuptools.packages].find must be a table")
        })?;
        return find_package_roots(find, tree).map(Some);
    }
    Err(CoreInputError::new(
        "[tool.setuptools].packages must be a list or a {find = {...}} table",
    ))
}

fn find_package_roots(
    find: &toml::Table,
    tree: &TreeIndex<'_>,
) -> Result<Vec<String>, CoreInputError> {
    let bases = optional_strings("[tool.setuptools.packages.find].where", find.get("where"))?;
    let bases = if bases.is_empty() {
        vec![String::from(".")]
    } else {
        bases
    };
    let includes = optional_strings(
        "[tool.setuptools.packages.find].include",
        find.get("include"),
    )?;
    let includes = if includes.is_empty() {
        vec![String::from("*")]
    } else {
        includes
    };
    let excludes = optional_strings(
        "[tool.setuptools.packages.find].exclude",
        find.get("exclude"),
    )?;
    let includes = name_patterns("[tool.setuptools.packages.find].include", &includes)?;
    let excludes = name_patterns("[tool.setuptools.packages.find].exclude", &excludes)?;

    let mut roots = Vec::new();
    for base in bases {
        let base = base.trim_start_matches("./").trim_matches('/');
        for child in tree.child_directories(base) {
            let name = child.rsplit('/').next().unwrap_or(child);
            let selected = !name.starts_with('.')
                && includes.iter().any(|pattern| pattern.matches(name))
                && !excludes.iter().any(|pattern| pattern.matches(name))
                && tree.holds_module(child);
            if selected && !roots.iter().any(|root| root == child) {
                roots.push(child.to_owned());
            }
        }
    }
    Ok(roots)
}

fn poetry_roots(
    tool: Option<&toml::Table>,
    tree: &TreeIndex<'_>,
) -> Result<Option<Vec<String>>, CoreInputError> {
    let Some(poetry) = tool
        .and_then(|tool| tool.get("poetry"))
        .and_then(Value::as_table)
    else {
        return Ok(None);
    };
    if let Some(packages) = poetry.get("packages") {
        let entries = packages
            .as_array()
            .ok_or_else(|| CoreInputError::new("[tool.poetry].packages must be a list"))?;
        let mut roots = Vec::new();
        for entry in entries {
            roots.push(poetry_package_root(entry)?);
        }
        return Ok(Some(dedupe(roots)));
    }
    match poetry.get("name").and_then(Value::as_str) {
        Some(name) if !name.trim().is_empty() => Ok(Some(name_directory_roots(name, tree))),
        _ => Ok(Some(Vec::new())),
    }
}

fn poetry_package_root(entry: &Value) -> Result<String, CoreInputError> {
    if let Some(value) = entry.as_str() {
        return Ok(value.trim_matches('/').to_owned());
    }
    let include = entry
        .as_table()
        .and_then(|entry| entry.get("include"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CoreInputError::new(
                "[tool.poetry].packages entries must be a string or a table with 'include'",
            )
        })?;
    let base = entry
        .as_table()
        .and_then(|entry| entry.get("from"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let joined = if base.is_empty() {
        include.to_owned()
    } else {
        format!("{base}/{include}")
    };
    Ok(joined.trim_matches('/').to_owned())
}

fn hatch_roots(
    tool: Option<&toml::Table>,
    tree: &TreeIndex<'_>,
) -> Result<Option<Vec<String>>, CoreInputError> {
    let Some(build) = tool
        .and_then(|tool| tool.get("hatch"))
        .and_then(Value::as_table)
        .and_then(|hatch| hatch.get("build"))
        .and_then(Value::as_table)
    else {
        return Ok(None);
    };
    let packages = build.get("packages").or_else(|| {
        build
            .get("targets")
            .and_then(Value::as_table)
            .and_then(|targets| targets.get("wheel"))
            .and_then(Value::as_table)
            .and_then(|wheel| wheel.get("packages"))
    });
    if let Some(packages) = packages {
        let entries = packages
            .as_array()
            .ok_or_else(|| CoreInputError::new("[tool.hatch.build...].packages must be a list"))?;
        let names = strings("[tool.hatch.build...].packages", entries)?;
        return Ok(Some(dedupe(
            names.iter().map(|name| name.trim_matches('/').to_owned()),
        )));
    }
    let Some(include) = build.get("include") else {
        return Ok(None);
    };
    let entries = include
        .as_array()
        .ok_or_else(|| CoreInputError::new("[tool.hatch.build].include must be a list"))?;
    let names = strings("[tool.hatch.build].include", entries)?;
    Ok(Some(dedupe(
        names
            .iter()
            .map(|name| name.trim_matches('/').to_owned())
            .filter(|name| tree.holds_module(name)),
    )))
}

fn flit_roots(
    tool: Option<&toml::Table>,
    tree: &TreeIndex<'_>,
) -> Result<Option<Vec<String>>, CoreInputError> {
    let Some(flit) = tool
        .and_then(|tool| tool.get("flit"))
        .and_then(Value::as_table)
    else {
        return Ok(None);
    };
    let name = flit
        .get("module")
        .and_then(Value::as_table)
        .and_then(|module| module.get("name"))
        .or_else(|| {
            flit.get("metadata")
                .and_then(Value::as_table)
                .and_then(|metadata| metadata.get("module"))
        });
    let Some(name) = name else {
        return Ok(None);
    };
    let name = name
        .as_str()
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| CoreInputError::new("[tool.flit.module].name must be a non-empty string"))?;
    Ok(Some(name_directory_roots(name, tree)))
}

fn pdm_roots(
    tool: Option<&toml::Table>,
    _tree: &TreeIndex<'_>,
) -> Result<Option<Vec<String>>, CoreInputError> {
    let Some(build) = tool
        .and_then(|tool| tool.get("pdm"))
        .and_then(Value::as_table)
        .and_then(|pdm| pdm.get("build"))
        .and_then(Value::as_table)
    else {
        return Ok(None);
    };
    let Some(includes) = build.get("includes") else {
        return Ok(None);
    };
    let entries = includes
        .as_array()
        .ok_or_else(|| CoreInputError::new("[tool.pdm.build].includes must be a list"))?;
    let names = strings("[tool.pdm.build].includes", entries)?;
    // An include list also carries single files and globs. Neither is a package
    // root, and neither is guessed at here.
    Ok(Some(dedupe(
        names
            .iter()
            .map(|name| name.trim_end_matches('/').to_owned())
            .filter(|name| !name.contains('*') && !name.ends_with(".py")),
    )))
}

fn layout_fallback_roots(
    pyproject: &Value,
    tree: &TreeIndex<'_>,
) -> Result<Vec<String>, CoreInputError> {
    let source_layout: Vec<String> = tree
        .child_directories("src")
        .into_iter()
        .filter(|child| {
            !child.rsplit('/').next().unwrap_or(child).starts_with('.') && tree.holds_module(child)
        })
        .map(str::to_owned)
        .collect();
    if !source_layout.is_empty() {
        return Ok(source_layout);
    }
    let name = table(pyproject, "project")
        .and_then(|project| project.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty());
    Ok(name.map_or_else(Vec::new, |name| name_directory_roots(name, tree)))
}

/// The one directory a distribution name can name, flat layout or `src/`.
fn name_directory_roots(name: &str, tree: &TreeIndex<'_>) -> Vec<String> {
    let normalized = name.trim().replace('-', "_");
    [normalized.clone(), format!("src/{normalized}")]
        .into_iter()
        .find(|candidate| tree.holds_module(candidate))
        .map_or_else(Vec::new, |candidate| vec![candidate])
}

fn table<'a>(value: &'a Value, key: &str) -> Option<&'a toml::Table> {
    value.get(key).and_then(Value::as_table)
}

fn strings(field: &str, entries: &[Value]) -> Result<Vec<String>, CoreInputError> {
    entries
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(|value| value.trim().to_owned())
                .ok_or_else(|| CoreInputError::new(format!("{field} entries must be strings")))
        })
        .collect()
}

fn optional_strings(field: &str, value: Option<&Value>) -> Result<Vec<String>, CoreInputError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let entries = value
        .as_array()
        .ok_or_else(|| CoreInputError::new(format!("{field} must be a list")))?;
    strings(field, entries)
}

fn name_patterns(field: &str, values: &[String]) -> Result<Vec<PathPattern>, CoreInputError> {
    values
        .iter()
        .map(|value| {
            PathPattern::tracked(value)
                .map_err(|reason| CoreInputError::new(format!("{field} {reason}")))
        })
        .collect()
}

fn dedupe(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    for name in names {
        if !name.is_empty() && !roots.contains(&name) {
            roots.push(name);
        }
    }
    roots
}
