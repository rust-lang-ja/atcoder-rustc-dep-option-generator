use cargo::core::manifest::{EitherManifest, Manifest};
use cargo::core::{Dependency as CargoDependency, GitReference, SourceId};
use cargo::util::config::Config;
use cargo::util::toml::read_manifest;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn load_manifest(cargo_toml_path: &Path) -> Result<Manifest> {
    let abs_path = &to_absolute::to_absolute_from_current_dir(".")?;
    let source_id = SourceId::for_directory(abs_path)?;
    let abs_cargo_toml_path = &to_absolute::to_absolute_from_current_dir(cargo_toml_path)?;
    let config = Config::default()?;

    let (manifest, _) = read_manifest(abs_cargo_toml_path, source_id, &config)
        .map_err(|e| failure::Error::from(e).compat())?;

    match manifest {
        EitherManifest::Virtual(_) => Err("virtual manifest is not supported.".into()),
        EitherManifest::Real(m) => Ok(m),
    }
}

struct Dependency {
    crate_name: String,
    library_path: PathBuf,
}

impl Dependency {
    fn parse_git(package_name: String, git_ref: &GitReference) -> Result<Locator> {
        match git_ref {
            GitReference::Rev(revision) => Ok(Locator::Git {
                package_name,
                revision: revision.clone(),
            }),
            GitReference::Tag(_) => panic!("Tagged git source is not supported."),
            GitReference::Branch(_) => panic!("Branch git source is not supported."),
        }
    }

    fn parse_normal(package_name: String, version_req: String) -> Result<Locator> {
        if !version_req.starts_with('=') {
            return Err("use exact match version requirement: `= *.*.*`".into());
        }

        let version = version_req[1..].trim().to_string();

        Ok(Locator::Version {
            package_name,
            version,
        })
    }

    pub fn parse(deps_path: &Path, dep: &CargoDependency) -> Result<Dependency> {
        if !deps_path.exists() {
            return Err("dependencies path is not exist.".into());
        }

        let package_name = dep.package_name().to_string();

        let locator = match dep.source_id().git_reference() {
            Some(git_ref) => Dependency::parse_git(package_name.clone(), git_ref),
            None => Dependency::parse_normal(package_name.clone(), dep.version_req().to_string()),
        }?;

        let crate_name = locator.crate_name();
        let library_path = locator.find_library_path(deps_path)?;

        Ok(Dependency {
            crate_name,
            library_path,
        })
    }

    pub fn make_compile_option(&self) -> impl Iterator<Item = String> {
        vec![
            "--extern".to_string(),
            format!("{}={}", self.crate_name, self.library_path.display()),
        ]
        .into_iter()
    }
}

enum Locator {
    Version {
        package_name: String,
        version: String,
    },
    Git {
        package_name: String,
        revision: String,
    },
}

impl Locator {
    fn package_name(&self) -> &str {
        match self {
            Locator::Version { package_name, .. } => package_name,
            Locator::Git { package_name, .. } => package_name,
        }
    }

    fn crate_name(&self) -> String {
        self.package_name().replace("-", "_")
    }

    fn search_patterns(&self) -> Vec<String> {
        //
        // FIXME: maybe more better way
        //
        // This function uses `{crate_name}-{random}.d` file in
        // /target/release/deps dir.  This file seems to contain paths for files
        // included in that crate.  Usually dependency crates that cargo pulled from
        // crates.io is placed under
        // ~/.cargo/registory/github-{random}/{package_name}-{version}/, so by
        // looking at `*.d` file we can determine the correct version of library
        // file.
        //
        //
        match self {
            Locator::Version {
                package_name,
                version,
            } => vec![format!("/{}-{}/", package_name, version)],
            Locator::Git {
                package_name,
                revision,
            } => vec![
                format!("/{}", package_name),
                format!("/{}/", &revision[0..7]),
            ],
        }
    }

    fn matches(&self, content: String) -> bool {
        self.search_patterns()
            .iter()
            .all(|pat| content.contains(pat))
    }

    fn find_library_path(&self, deps_path: &Path) -> Result<PathBuf> {
        let crate_name = self.crate_name();
        for file in deps_path.read_dir()? {
            let file = file?;
            if file.file_type()?.is_dir() {
                continue;
            }

            let file_name = file.file_name();
            let file_name = file_name
                .to_str()
                .ok_or("file_name has invalid byte for UTF-8")?;
            if !file_name.starts_with(&crate_name) {
                continue;
            }

            let path = to_absolute::to_absolute_from_current_dir(file.path())?;
            let content = fs::read_to_string(&path)?;
            if self.matches(content) {
                let stem = path.with_file_name(format!("lib{}", file_name));

                // proc_macro
                let so = stem.with_extension("so");
                if so.exists() {
                    return Ok(so);
                }

                // any other normal crates
                let rlib = stem.with_extension("rlib");
                if rlib.exists() {
                    return Ok(rlib);
                }

                unreachable!("The compiled crate must be either '.rlib' or '.so'.");
            }
        }

        Err(format!(
            "failed to find appropriate path for {}",
            self.package_name()
        )
        .into())
    }
}

fn main() -> Result<()> {
    let cargo_toml_path = env::args().nth(1).ok_or("please specify cargo.toml path")?;
    let deps_path = env::args().nth(2).ok_or("please specify deps path")?;

    // read the manifest
    let cargo_toml_path = PathBuf::from(cargo_toml_path);
    let manifest = load_manifest(&cargo_toml_path)?;

    // path for `*/target/release/deps`
    let deps_path = to_absolute::to_absolute_from_current_dir(PathBuf::from(deps_path))?;

    let options = manifest
        .dependencies()
        .iter()
        .map(|dep| Dependency::parse(&deps_path, dep))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flat_map(|dep| dep.make_compile_option())
        .collect::<Vec<_>>()
        .join(" ");

    println!("{} -L dependency={}", options, deps_path.display());

    Ok(())
}
