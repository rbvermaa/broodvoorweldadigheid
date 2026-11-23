use std::path::Path;

use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, TargetKind};

use crate::model::{Crate, Dep, DepKind, RootFile};

pub fn load(manifest_path: Option<&Path>) -> Result<Metadata, String> {
    let mut command = MetadataCommand::new();
    command.no_deps();
    if let Some(path) = manifest_path {
        command.manifest_path(path);
    }
    command
        .exec()
        .map_err(|error| format!("cargo metadata failed: {error}"))
}

pub fn crates(metadata: &Metadata) -> Vec<Crate> {
    let members = metadata.workspace_packages();
    let names: Vec<&str> = members
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    members
        .iter()
        .map(|package| Crate {
            name: package.name.to_string(),
            deps: package
                .dependencies
                .iter()
                .map(|dependency| Dep {
                    name: dependency.name.clone(),
                    kind: match dependency.kind {
                        DependencyKind::Build => DepKind::Build,
                        DependencyKind::Development => DepKind::Dev,
                        _ => DepKind::Normal,
                    },
                    optional: dependency.optional,
                    workspace: dependency.path.is_some()
                        && names.contains(&dependency.name.as_str()),
                })
                .collect(),
        })
        .collect()
}

pub fn root_files(metadata: &Metadata) -> Result<Vec<RootFile>, String> {
    let mut roots = Vec::new();
    for package in metadata.workspace_packages() {
        for target in &package.targets {
            let is_root = target.kind.iter().any(|kind| {
                matches!(
                    kind,
                    TargetKind::Lib
                        | TargetKind::Bin
                        | TargetKind::ProcMacro
                        | TargetKind::RLib
                        | TargetKind::DyLib
                        | TargetKind::CDyLib
                        | TargetKind::StaticLib
                )
            });
            if !is_root {
                continue;
            }
            let contents = std::fs::read_to_string(&target.src_path)
                .map_err(|error| format!("cannot read {}: {error}", target.src_path))?;
            roots.push(RootFile {
                crate_name: package.name.to_string(),
                path: target.src_path.to_string(),
                contents,
            });
        }
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_workspace_passes_all_checks() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml");
        let metadata = load(Some(&manifest)).expect("cargo metadata");
        let crates = crates(&metadata);
        assert!(crates.iter().any(|krate| krate.name == "server"));
        assert_eq!(crate::layers::check(&crates), Vec::<String>::new());
        let roots = root_files(&metadata).expect("root files");
        assert!(roots.iter().any(|root| root.crate_name == "domain"));
        assert_eq!(crate::unsafe_forbid::check(&roots), Vec::<String>::new());
    }
}
