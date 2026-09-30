//! Guided setup for the Magician stack.
//!
//!     magician-setup            choose what you want, see what it will install
//!     magician-setup --status   what is present right now, and what that enables
//!
//! The wizard asks about **capabilities** — things a person recognises — and
//! derives the components. Nobody wants a browser executor; they want the agent
//! to read their tabs. The graph and the resolver live in `magician-components`
//! so both this and the runtime answer from one declaration.

mod engine;
mod install_mode;
mod model;
mod simulate;
mod theme;
mod ui;

use std::collections::BTreeMap;
use std::process::ExitCode;

use magician_components::{loader, probe, Observed};

fn usage() {
    println!(
        "magician-setup — guided setup for the Magician stack\n\
         \n\
         (no args)   choose how to install, then what you want, then review the plan\n\
         --status    print what is present now and what it enables, then exit
--force-rebuild
            build from source again even when the binaries look current
--dry-run   walk the whole install and change nothing. Probes are real, so
            the plan is this machine's; every action is printed, not done
--simulate [file]
            play the whole flow against many described machines — an 8GB
            Intel Mac, a half-installed re-run, someone who declines every
            prompt — and check the invariants. With a YAML file, plays the
            machines it describes instead. Reads no probe and touches nothing,
            so it says nothing about this machine. Exits non-zero if any
            invariant broke\n\
         --help      this text\n\
         \n\
         Not a TTY, or MAGICIAN_WIZARD_NONINTERACTIVE=1, falls back to --status:\n\
         a wizard that blocks on a prompt in CI is a hung build."
    );
}

fn main() -> ExitCode {
    // The runtime is built here rather than by a macro so the engine can block
    // on a probe. Inside `#[tokio::main]` that would panic; the engine is a
    // sequence and has nothing to overlap, so blocking is the honest shape.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("could not start a runtime: {e}");
            return ExitCode::FAILURE;
        },
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut status_only = args.iter().any(|a| a == "--status");
    let force_rebuild = args.iter().any(|a| a == "--force-rebuild");
    // Walk the whole install and change nothing. Probing stays real, so the
    // plan is this machine's rather than an invented one.
    let dry_run = args.iter().any(|a| a == "--dry-run");
    // Every flow against many machines, none of them this one. An argument
    // after the flag is a file of machines somebody else described.
    let simulate = args.iter().position(|a| a == "--simulate");
    let scenario_file = simulate
        .and_then(|at| args.get(at + 1))
        .filter(|a| !a.starts_with("--"));

    if args.iter().any(|a| a == "--help" || a == "-h") {
        usage();
        return ExitCode::SUCCESS;
    }
    if let Some(bad) = args.iter().find(|a| {
        !matches!(
            a.as_str(),
            "--status" | "--force-rebuild" | "--dry-run" | "--simulate"
        ) && Some(a) != scenario_file.as_ref()
    }) {
        eprintln!("unknown argument: {bad} (see --help)");
        return ExitCode::from(2);
    }

    let paths = loader::Paths::from_env();
    let graph = match loader::try_load(&paths) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("the component graph is malformed: {e}");
            return ExitCode::FAILURE;
        },
    };

    // Before anything reads this machine: a simulation is about other machines,
    // and probing here would both waste the time and invite the reader to
    // believe the results describe their laptop.
    if simulate.is_some() {
        let played = match scenario_file {
            Some(path) => match std::fs::read_to_string(path)
                .map_err(|e| format!("{path}: {e}"))
                .and_then(|yaml| simulate::read_scenarios(&yaml))
            {
                Ok(scenarios) => simulate::play(&graph, &scenarios),
                Err(e) => {
                    eprintln!("could not read those machines: {e}");
                    return ExitCode::from(2);
                },
            },
            None => simulate::sweep(&graph),
        };
        let (report, broken) = played;
        print!("{report}");
        return if broken == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    println!("Looking at what is already here…");
    let observed: BTreeMap<String, Observed> =
        runtime.block_on(probe::observe_all(&graph.components));

    // An interactive screen needs a terminal to draw on and a person to read it.
    // Without either, print the same information and exit rather than block.
    //
    // A dry run is the exception: it exists to be read, including in a pipe or
    // a review, so it walks the flow with the selection it would have started
    // from instead of collapsing to a status list.
    let headless = !ui::interactive_possible();
    if headless && !dry_run {
        status_only = true;
    }

    // A re-run starts from what the person chose last time, not only from what
    // happens to be working. Declining the tunnel leaves no trace on the machine,
    // so without this it would be offered again as though nobody had said no.
    let data_root = std::path::PathBuf::from(&paths.data_root);
    let mut answers = engine::state::Answers::load(&data_root);
    let selection_answers = answers_snapshot(&answers);
    let mut selection = model::Selection::with_remembered(graph, observed, move |id| {
        selection_answers.get(id).copied()
    });
    if status_only {
        ui::print_status(&selection);
        return ExitCode::SUCCESS;
    }

    // The repository root is where the wizard binary lives inside a checkout;
    // outside one there is no source, which is exactly what the offer rules
    // want to know.
    let repo_root = repo_root();
    let machine = install_mode::detect(&repo_root);
    let offers = install_mode::offers(&machine);

    // Headless dry run: no screen, but every stage after it still runs.
    let outcome = if headless && dry_run {
        let mode = offers
            .iter()
            .find(|o| o.state.is_ready())
            .map(|o| o.mode)
            .unwrap_or(install_mode::InstallMode::FromSource);
        println!("\nNo terminal, so nothing was asked. Walking the flow with the");
        println!("capabilities remembered from last time, and the first install path");
        println!("this machine can take.");
        Ok(ui::Outcome::Confirmed(mode))
    } else {
        ui::run(&mut selection, &offers)
    };

    match outcome {
        Ok(ui::Outcome::Confirmed(mode)) => {
            // Stage 1 — the binaries. Where they come from is the mode's
            // question; every mode ends with them at the prefix.
            if dry_run {
                narrate_binaries(mode, &machine);
            } else if let Err(message) =
                prepare_binaries(&runtime, mode, &machine, &repo_root, force_rebuild)
            {
                eprintln!("{message}");
                return ExitCode::FAILURE;
            }

            // Stage 2 — the runtime root and the stack around it. NOT WIRED:
            // `scripts/install.sh` still owns this, and someone has to run it
            // themselves. The stage exists here so the flow is whole and the
            // gap is visible rather than discovered.
            narrate_core_install(dry_run);

            // Stage 3 — the capability plan.
            ui::print_plan(&selection);
            remember(&mut answers, &selection);
            let plan = selection.plan();
            if plan.install.is_empty() && !dry_run {
                println!("\nNothing to install — everything you chose already works.");
                return ExitCode::SUCCESS;
            }
            if !dry_run && !confirm_run(plan.install.len()) {
                println!("Nothing was changed.");
                return ExitCode::SUCCESS;
            }
            let code = run_plan(&runtime, &selection, &plan.install, dry_run);

            // Stage 4 — the health sweep. NOT WIRED either; install-verify.sh
            // exists and nothing calls it from here yet.
            narrate_verify(dry_run);
            if dry_run {
                println!("\nNothing was changed. Drop --dry-run to do it for real.");
            }
            code
        },
        Ok(ui::Outcome::Cancelled) => {
            println!("Cancelled. Nothing was changed.");
            ExitCode::SUCCESS
        },
        Err(e) => {
            eprintln!("the screen could not start: {e}");
            ui::print_status(&selection);
            ExitCode::FAILURE
        },
    }
}

/// Where the checkout is, if this is running inside one. Derived from the
/// binary's own location rather than the working directory, so running the
/// wizard from elsewhere still finds the source it was built from.
fn repo_root() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            // …/target/release/magician-setup -> …/
            exe.parent()?
                .parent()?
                .parent()
                .map(std::path::Path::to_path_buf)
        })
        .filter(|root| root.join("magician-bin/Cargo.toml").is_file())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

/// Make sure the four runtime binaries exist before the capability plan starts
/// installing things around them. Only the from-source path has anything to do
/// here; prebuilt will download, and container is not offered yet.
fn prepare_binaries(
    runtime: &tokio::runtime::Runtime,
    mode: install_mode::InstallMode,
    machine: &install_mode::Machine,
    repo_root: &std::path::Path,
    force_rebuild: bool,
) -> Result<(), String> {
    use install_mode::InstallMode;
    let _ = runtime;
    match mode {
        InstallMode::Container => Err("The container path is not available yet.".to_string()),
        InstallMode::Prebuilt => {
            let Some(source) = machine.prebuilt_source.clone() else {
                return Err(
                    "There is no package to install from. Choose 'From source', \
                            or set MAGICIAN_PACKAGE to a package you already have."
                        .to_string(),
                );
            };
            install_package(&source)
        },
        InstallMode::FromSource => {
            if machine.artifacts_fresh && !force_rebuild {
                println!("\nAlready built here — nothing to compile.");
                println!("Pass --force-rebuild to build it again anyway.");
            } else {
                // Asked, not assumed. A build is minutes of someone's time and
                // the one step of this wizard that cannot be undone by walking
                // away from it.
                if !confirm_build(machine.artifacts_fresh) {
                    return Err("Nothing was built, so nothing was changed.".to_string());
                }
                println!("\nInstalling build prerequisites…");
                run_make_in(repo_root, "setup-prerequisites")?;
                println!("Building. This takes a while on a first run.");
                run_make_in(repo_root, "build-all-release")?;
            }

            // Both routes end in the same place. A build that left the binaries
            // in the checkout would give a developer a different layout from
            // everyone else's — two things to support, and an uninstaller that
            // only knows one of them.
            println!("\nPackaging what was built…");
            let package = package_locally(repo_root)?;
            install_package(&package)
        },
    }
}

/// Hand a package to its own installer. The package carries the installer it
/// was built with, so the checksum check, the layout and the replace prompt are
/// that release's, not this binary's idea of them.
fn install_package(source: &str) -> Result<(), String> {
    let path = std::path::Path::new(source);
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        // A tarball is unpacked beside itself rather than into a temp dir the
        // user cannot inspect after a failure.
        let parent = path.parent().unwrap_or(std::path::Path::new("."));
        let status = std::process::Command::new("tar")
            .args(["-xzf", source, "-C"])
            .arg(parent)
            .status()
            .map_err(|e| format!("could not unpack {source}: {e}"))?;
        if !status.success() {
            return Err(format!("{source} could not be unpacked."));
        }
        let stem = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.trim_end_matches(".tar.gz").to_string())
            .ok_or_else(|| format!("cannot tell what {source} unpacks to"))?;
        parent.join(stem)
    };

    let installer = dir.join("install.sh");
    if !installer.is_file() {
        return Err(format!(
            "{} has no install.sh — it is not a Magician package.",
            dir.display()
        ));
    }
    let status = std::process::Command::new("bash")
        .arg(&installer)
        .arg("--yes")
        .status()
        .map_err(|e| format!("could not run the package installer: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("the package installer refused. The output above says why.".to_string())
    }
}

/// Package the local build the same way a release is packaged, so the thing a
/// developer installs went through the identical path as the thing a stranger
/// downloads. Ad-hoc signed: a locally built binary is never quarantined, so
/// distribution signing would buy nothing here.
fn package_locally(repo_root: &std::path::Path) -> Result<String, String> {
    let path_file = std::env::temp_dir().join(format!("magician-package-{}", std::process::id()));
    let status = std::process::Command::new("bash")
        .arg(repo_root.join("scripts/package-release.sh"))
        .current_dir(repo_root)
        .env("MAGICIAN_SIGN_ADHOC", "1")
        .env("MAGICIAN_PACKAGE_PATH_FILE", &path_file)
        .status()
        .map_err(|e| format!("could not run the packager: {e}"))?;
    if !status.success() {
        return Err("packaging failed. The output above says why.".to_string());
    }
    let package = std::fs::read_to_string(&path_file)
        .map_err(|e| format!("the packager did not report where it put the package: {e}"))?
        .trim()
        .to_string();
    let _ = std::fs::remove_file(&path_file);
    if package.is_empty() {
        return Err("the packager reported an empty path".to_string());
    }
    Ok(package)
}

fn confirm_build(already_built: bool) -> bool {
    if already_built {
        print!("\nRebuild from source? This can take several minutes. [Y/n] ");
    } else {
        print!("\nBuild from source now? This can take several minutes. [Y/n] ");
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    !matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no")
}

fn run_make_in(repo_root: &std::path::Path, target: &str) -> Result<(), String> {
    let status = std::process::Command::new("make")
        .arg(target)
        .current_dir(repo_root)
        .status()
        .map_err(|e| format!("could not run make {target}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "`make {target}` failed. The output above says why."
        ))
    }
}

fn confirm_run(count: usize) -> bool {
    use std::io::Write;
    print!("\nSet up {count} component(s) now? [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes")
}

/// Work the plan, then say plainly what is and is not there afterwards. The
/// exit code reflects the outcome: a step that ran and still cannot be verified
/// is a failure, while one that was declined is a choice.
/// The stages that exist in the flow but are not yet wired to anything. They
/// are printed rather than omitted: a flow with a silent hole in it reads as
/// finished, and whoever runs it finds the hole afterwards.
fn narrate_core_install(dry_run: bool) {
    println!("\n── The stack itself ──");
    if dry_run {
        println!("  would run: scripts/install.sh (data root, seed, supervisor, health)");
    } else {
        println!("  Not yet run from here. Until the wizard delegates to it, this is");
        println!("  a separate step:  make install");
    }
}

fn narrate_verify(dry_run: bool) {
    println!("\n── Checking it came up ──");
    if dry_run {
        println!("  would run: scripts/install-verify.sh");
    } else {
        println!("  Not yet run from here:  bash scripts/install-verify.sh");
    }
}

fn narrate_binaries(mode: install_mode::InstallMode, machine: &install_mode::Machine) {
    use install_mode::InstallMode;
    println!("\n── The binaries ──");
    match mode {
        InstallMode::FromSource if machine.artifacts_fresh => {
            println!("  already built here — would package and install them to the prefix")
        },
        InstallMode::FromSource => {
            println!("  would install build prerequisites, build, package, install to the prefix")
        },
        InstallMode::Prebuilt => match &machine.prebuilt_source {
            Some(source) => println!("  would verify and install the package at {source}"),
            None => println!("  no package available — nothing to install"),
        },
        InstallMode::Container => println!("  the container path is not available yet"),
    }
}

fn run_plan(
    runtime: &tokio::runtime::Runtime,
    selection: &model::Selection,
    order: &[String],
    dry_run: bool,
) -> ExitCode {
    let repo_root = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let in_checkout = repo_root.join("Makefile").is_file();
    let data_root =
        std::path::PathBuf::from(magician_components::loader::Paths::from_env().data_root);
    let mut real =
        engine::real::RealEffects::new(runtime, &selection.graph.components, repo_root, data_root);
    let mut dry =
        engine::dry::DryRunEffects::new(runtime, &selection.graph.components, in_checkout);
    let usable = selection
        .report()
        .components
        .iter()
        .map(|c| (c.id.clone(), c.state.usable()))
        .collect();

    println!("\n── What you chose ──");
    let reports = if dry_run {
        engine::Engine::new(&selection.graph, &mut dry, usable).run(order)
    } else {
        engine::Engine::new(&selection.graph, &mut real, usable).run(order)
    };
    if dry_run {
        return ExitCode::SUCCESS;
    }

    let unfinished: Vec<&engine::StepReport> = reports
        .iter()
        .filter(|r| matches!(r.outcome, engine::StepOutcome::NotVerified { .. }))
        .collect();
    if unfinished.is_empty() {
        println!("\nDone. Re-run with --status at any time to see where things stand.");
        ExitCode::SUCCESS
    } else {
        println!(
            "\n{} step(s) ran but could not be verified:",
            unfinished.len()
        );
        for report in unfinished {
            println!("  - {} ({})", report.name, report.component_id);
        }
        println!("Re-running is safe — anything already there is left alone.");
        ExitCode::FAILURE
    }
}

/// Snapshot the remembered answers so the closure that reads them owns its data
/// and the file handle is not held across the whole selection.
fn answers_snapshot(answers: &engine::state::Answers) -> std::collections::BTreeMap<String, bool> {
    answers.all()
}

/// Record what was chosen, once. Written after acceptance rather than on every
/// keystroke, so an abandoned run leaves last time's answers intact.
fn remember(answers: &mut engine::state::Answers, selection: &model::Selection) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let chosen: std::collections::BTreeSet<&String> = selection.wanted_ids().collect();
    for feature in selection.features() {
        answers.record(&feature.id, chosen.contains(&feature.id), now);
    }
}
