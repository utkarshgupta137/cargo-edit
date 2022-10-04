use std::path::Path;
use std::path::PathBuf;

use cargo_edit::{
    resolve_manifests, shell_status, shell_warn, upgrade_requirement, workspace_members,
    LocalManifest,
};
use clap::Args;

use crate::errors::*;
use crate::version::BumpLevel;
use crate::version::TargetVersion;

/// Change a package's version in the local manifest file (i.e. Cargo.toml).
#[derive(Debug, Args)]
#[command(version)]
#[command(group = clap::ArgGroup::new("ver").multiple(false))]
pub struct VersionArgs {
    /// Version to change manifests to
    #[arg(group = "ver")]
    target: Option<semver::Version>,

    /// Increment manifest version
    #[arg(long, group = "ver")]
    bump: Option<BumpLevel>,

    /// Specify the version metadata field (e.g. a wrapped libraries version)
    #[arg(short, long)]
    pub metadata: Option<String>,

    /// Path to the manifest to upgrade
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,

    /// Package id of the crate to change the version of.
    #[arg(
        long = "package",
        short = 'p',
        value_name = "PKGID",
        conflicts_with = "all",
        conflicts_with = "workspace"
    )]
    pkgid: Option<String>,

    /// Modify all packages in the workspace.
    #[arg(
        long,
        help = "[deprecated in favor of `--workspace`]",
        conflicts_with = "workspace",
        conflicts_with = "pkgid"
    )]
    all: bool,

    /// Modify all packages in the workspace.
    #[arg(long, conflicts_with = "all", conflicts_with = "pkgid")]
    workspace: bool,

    /// Print changes to be made without making them.
    #[arg(long)]
    dry_run: bool,

    /// Crates to exclude and not modify.
    #[arg(long)]
    exclude: Vec<String>,

    /// Unstable (nightly-only) flags
    #[arg(short = 'Z', value_name = "FLAG", global = true, value_enum)]
    unstable_features: Vec<UnstableOptions>,
}

impl VersionArgs {
    pub fn exec(self) -> CargoResult<()> {
        exec(self)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
enum UnstableOptions {}

/// Main processing function. Allows us to return a `Result` so that `main` can print pretty error
/// messages.
fn exec(args: VersionArgs) -> CargoResult<()> {
    let VersionArgs {
        target,
        bump,
        metadata,
        manifest_path,
        pkgid,
        all,
        dry_run,
        workspace,
        exclude,
        unstable_features: _,
    } = args;

    let target = match (target, bump) {
        (None, None) => TargetVersion::Relative(BumpLevel::Release),
        (None, Some(level)) => TargetVersion::Relative(level),
        (Some(version), None) => TargetVersion::Absolute(version),
        (Some(_), Some(_)) => unreachable!("clap groups should prevent this"),
    };

    if all {
        shell_warn("The flag `--all` has been deprecated in favor of `--workspace`")?;
    }
    let all = workspace || all;
    let manifests = resolve_manifests(
        manifest_path.as_deref(),
        all,
        pkgid.as_deref().into_iter().collect::<Vec<_>>(),
    )?;

    let workspace_members = workspace_members(manifest_path.as_deref())?;

    for package in manifests {
        if exclude.contains(&package.name) {
            continue;
        }
        let current = &package.version;
        let next = target.bump(current, metadata.as_deref())?;
        if let Some(next) = next {
            {
                let mut manifest = LocalManifest::try_new(Path::new(&package.manifest_path))?;
                manifest.set_package_version(&next);

                shell_status(
                    "Upgrading",
                    &format!("{} from {} to {}", package.name, current, next),
                )?;
                if !dry_run {
                    manifest.write()?;
                }
            }

            let crate_root =
                dunce::canonicalize(package.manifest_path.parent().expect("at least a parent"))?;
            for member in workspace_members.iter() {
                let mut dep_manifest = LocalManifest::try_new(member.manifest_path.as_std_path())?;
                let mut changed = false;
                let dep_crate_root = dep_manifest
                    .path
                    .parent()
                    .expect("at least a parent")
                    .to_owned();
                for dep in dep_manifest
                    .get_dependency_tables_mut()
                    .flat_map(|t| t.iter_mut().filter_map(|(_, d)| d.as_table_like_mut()))
                    .filter(|d| {
                        if !d.contains_key("version") {
                            return false;
                        }
                        match d.get("path").and_then(|i| i.as_str()).and_then(|relpath| {
                            dunce::canonicalize(dep_crate_root.join(relpath)).ok()
                        }) {
                            Some(dep_path) => dep_path == crate_root.as_path(),
                            None => false,
                        }
                    })
                {
                    let old_req = dep
                        .get("version")
                        .expect("filter ensures this")
                        .as_str()
                        .unwrap_or("*");
                    if let Some(new_req) = upgrade_requirement(old_req, &next)? {
                        shell_status(
                            "Updating",
                            &format!(
                                "{}'s dependency from {} to {}",
                                member.name, old_req, new_req
                            ),
                        )?;
                        dep.insert("version", toml_edit::value(new_req));
                        changed = true;
                    }
                }
                if changed && !dry_run {
                    dep_manifest.write()?;
                }
            }
        }
    }

    if args.dry_run {
        shell_warn("aborting set-version due to dry run")?;
    }

    Ok(())
}
