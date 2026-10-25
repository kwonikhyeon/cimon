//! `cimon recipe …` — local checks and admin access to the recipe repository.

use anyhow::{bail, Context, Result};
use cimon_core::Config;
use cimon_recipe::tree::{MemTree, Tree};
use cimon_recipe::{check_tree, resolve_project};
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum RecipeCmd {
    /// Static checks on a recipe folder (no server needed). Intake checks are reported
    /// as skipped.
    Check { dir: PathBuf },
    /// Print a project's resolved recipe (presets and includes layered).
    Show {
        project: String,
        #[arg(long)]
        resolved: bool,
        /// Read from a folder instead of the server's recipe repository.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Recipe ref when reading the repository (default main).
        #[arg(long = "ref")]
        ref_: Option<String>,
    },
    /// Import a folder: --init creates main once; --branch commits to a recipe/* branch.
    Import {
        dir: PathBuf,
        #[arg(long)]
        init: bool,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        author: String,
        #[arg(long)]
        message: Option<String>,
    },
    /// Write a ref of the recipe repository to a folder.
    Export {
        dir: PathBuf,
        #[arg(long = "ref")]
        ref_: Option<String>,
    },
    /// Rebuild the recipe repository from a bundle (the store path must not exist).
    Restore { bundle: PathBuf },
}

pub fn run(cmd: RecipeCmd, config: &Option<PathBuf>) -> Result<i32> {
    match cmd {
        RecipeCmd::Check { dir } => {
            let tree = MemTree::from_dir(&dir).with_context(|| format!("reading {}", dir.display()))?;
            let report = check_tree(&tree);
            for d in &report.errors {
                println!("error: {d}");
            }
            for d in &report.warnings {
                println!("warning: {d}");
            }
            println!(
                "skipped: intake checks (version_file, globs matching tracked files, base toolchains) — they need the project repository and the server; a trial run covers them"
            );
            println!(
                "{} project(s), {} error(s), {} warning(s) — cimon {}",
                tree.project_ids().len(),
                report.errors.len(),
                report.warnings.len(),
                env!("CARGO_PKG_VERSION")
            );
            Ok(if report.ok() { 0 } else { 1 })
        }
        RecipeCmd::Show { project, resolved, dir, ref_ } => {
            let tree: std::sync::Arc<MemTree> = match dir {
                Some(d) => std::sync::Arc::new(MemTree::from_dir(&d)?),
                None => {
                    let app = open(config)?;
                    let commit = app.recipes.resolve(ref_.as_deref().unwrap_or("main")).map_err(|e| anyhow::anyhow!("{e}"))?;
                    app.recipes.tree(&commit).map_err(|e| anyhow::anyhow!("{e}"))?
                }
            };
            if resolved {
                match resolve_project(&*tree, &project) {
                    Ok(r) => println!("{}", serde_json::to_string_pretty(&r)?),
                    Err(diags) => {
                        for d in diags {
                            println!("error: {d}");
                        }
                        return Ok(1);
                    }
                }
            } else {
                for f in tree.files().into_iter().filter(|f| f.starts_with(&format!("projects/{project}/"))) {
                    println!("── {f}\n{}", tree.read_str(f).unwrap_or("(binary)"));
                }
            }
            Ok(0)
        }
        RecipeCmd::Import { dir, init, branch, author, message } => {
            let app = open(config)?;
            let tree = MemTree::from_dir(&dir).with_context(|| format!("reading {}", dir.display()))?;
            if init == branch.is_some() {
                bail!("give exactly one of --init and --branch recipe/<설명>");
            }
            if init {
                let msg = message.unwrap_or_else(|| "레시피 저장소 처음 내용".into());
                match app.recipes.import_init(&tree, &author, &msg) {
                    Ok(c) => println!("main = {c}"),
                    Err(e) => {
                        eprintln!("{e}");
                        return Ok(1);
                    }
                }
                match cimon_engine::backup::backup_recipes(&app, "import --init") {
                    Ok(p) => println!("bundle: {}", p.display()),
                    Err(e) => eprintln!("WARNING: {e}"),
                }
            } else {
                let branch = branch.unwrap();
                let Some(msg) = message else { bail!("--message is required for --branch") };
                match app.recipes.import_branch(&app.store, &author, &branch, &tree, &msg) {
                    Ok(r) => {
                        println!("{branch} = {} (proposal {})", r.commit, cimon_core::proposal_label(r.proposal_id));
                        for w in r.warnings {
                            println!("warning: {w}");
                        }
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        return Ok(1);
                    }
                }
            }
            for w in cimon_engine::backup::warnings(&app) {
                eprintln!("WARNING: {w}");
            }
            Ok(0)
        }
        RecipeCmd::Export { dir, ref_ } => {
            let app = open(config)?;
            let commit = app.recipes.resolve(ref_.as_deref().unwrap_or("main")).map_err(|e| anyhow::anyhow!("{e}"))?;
            let tree = app.recipes.tree(&commit).map_err(|e| anyhow::anyhow!("{e}"))?;
            if dir.exists() && std::fs::read_dir(&dir)?.next().is_some() {
                bail!("{} is not empty", dir.display());
            }
            tree.write_to(&dir)?;
            println!("{} file(s) from {commit} written to {}", tree.files.len(), dir.display());
            Ok(0)
        }
        RecipeCmd::Restore { bundle } => {
            let cfg = crate::load_config(config)?;
            let path = cfg.recipes_store();
            cimon_recipestore::RecipeStore::restore(&bundle, &path).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("restored {} from {}", path.display(), bundle.display());
            let app = cimon_engine::App::open(cfg)?;
            for n in app.recipes.reconcile(&app.store).map_err(|e| anyhow::anyhow!("{e}"))? {
                println!("{n}");
            }
            Ok(0)
        }
    }
}

fn open(config: &Option<PathBuf>) -> Result<std::sync::Arc<cimon_engine::App>> {
    let cfg: Config = crate::load_config(config)?;
    cimon_engine::App::open(cfg)
}
