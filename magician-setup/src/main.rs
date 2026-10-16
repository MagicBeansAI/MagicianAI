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

use magician_components::source_install::{SourceInstall, Stage};
use magician_components::{loader, Observed};

fn usage() {
    println!(
        "magician-setup — guided setup for the Magician stack\n\
         \n\
         (no args)   choose how to install, then what you want, then review the plan\n\
         --status    print what is present now and what it enables, then exit
--force-rebuild
            compatibility option; source setup always checks the incremental build
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
        runtime.block_on(engine::real::observe_components(&graph.components));

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
            let source_install = match source_install(&repo_root, &data_root) {
                Ok(plan) => plan,
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::FAILURE;
                },
            };
            // One acceptance covers the reviewed binary, runtime and capability plan.
            // Nothing is persisted by a rehearsal or an abandoned selection.
            println!("\nSource installation: debug runtime and Magican Desktop");
            println!("  Checkout: {}", source_install.checkout.display());
            println!("  Runtime data: {}", source_install.data_root.display());
            println!("  Installed software: {}", source_install.prefix.display());
            println!("  Backend: per-user ai.magicbeans.magican.backend LaunchAgent");
            println!("  Desktop: restart Magican with the new source build");
            println!("  Existing configuration, notes and scopes are preserved.");
            if !dry_run {
                if let Err(error) = run_source_stage(&source_install, Stage::Prepare, true) {
                    eprintln!("{error}");
                    return ExitCode::FAILURE;
                }
            }
            ui::print_plan(&selection);
            if !dry_run && !confirm_run(selection.plan().install.len()) {
                println!("Nothing was changed.");
                return ExitCode::SUCCESS;
            }
            if !dry_run {
                remember(&mut answers, &selection);
            }
            let prepared = match mode {
                install_mode::InstallMode::FromSource =>
                    run_source_stage(&source_install, Stage::Prepare, dry_run),
                install_mode::InstallMode::Prebuilt if dry_run => {
                    narrate_binaries(mode, &machine);
                    Ok(())
                },
                install_mode::InstallMode::Prebuilt =>
                    Err("Package-only terminal installation is not available yet; use Desktop setup or choose From source".into()),
                install_mode::InstallMode::Container =>
                    Err("The container path is not available yet.".into()),
            };
            if let Err(error) = prepared {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
            if let Err(error) = run_source_stage(&source_install, Stage::Start, dry_run) {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
            if let Err(error) = run_source_stage(&source_install, Stage::Verify, dry_run) {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
            if dry_run {
                // Only the rehearsal projects core startup. Real readiness is
                // always read again after services have actually started.
                for component in &selection.graph.components {
                    if component.core {
                        selection.observed.insert(
                            component.id.clone(),
                            Observed::Present("would be started by the core installation".into()),
                        );
                    }
                }
            } else {
                selection.observed = runtime.block_on(engine::real::observe_components(
                    &selection.graph.components,
                ));
            }
            let plan = selection.plan();
            ui::print_plan(&selection);
            let code = run_plan(
                &runtime,
                &selection,
                &plan.install,
                dry_run,
                &repo_root,
                &data_root,
            );
            // Apply credentials collected after the first service start. The
            // helper keeps an unchanged installation running on a re-run.
            if !dry_run {
                let status = std::process::Command::new("/bin/bash")
                    .args(source_install.arguments(Stage::Start, false, true))
                    .current_dir(&repo_root)
                    .status();
                if !status.is_ok_and(|status| status.success()) {
                    eprintln!("Could not apply runtime credentials to the backend service");
                    return ExitCode::FAILURE;
                }
            }
            // An empty capability plan still requires a health sweep.
            if let Err(error) = run_source_stage(&source_install, Stage::Verify, dry_run) {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
            if dry_run {
                println!("\nDry run complete. No choices, credentials, runtime files or services were changed.");
                return ExitCode::SUCCESS;
            }
            selection.observed = runtime.block_on(engine::real::observe_components(
                &selection.graph.components,
            ));
            let unavailable = unavailable_selected(&selection);
            if !unavailable.is_empty() {
                eprintln!("\nInstallation is incomplete. These selected capabilities or required components are not ready:");
                for item in unavailable {
                    eprintln!("  - {item}");
                }
                return ExitCode::FAILURE;
            }
            if code == ExitCode::SUCCESS {
                println!("\nInstallation verified. Re-run with --status to inspect readiness.");
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
    let compiled = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(std::path::Path::to_path_buf);
    let candidates = [
        std::env::var_os("MAGICIAN_SOURCE_DIR").map(std::path::PathBuf::from),
        std::env::current_dir().ok(),
        compiled,
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|root| magician_components::source_install::is_checkout(root))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

fn source_install(
    repo_root: &std::path::Path,
    data_root: &std::path::Path,
) -> Result<SourceInstall, String> {
    let prefix = std::env::var_os("MAGICIAN_PREFIX")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".magician"))
        })
        .ok_or("Set MAGICIAN_PREFIX or HOME for native installation")?;
    SourceInstall::new(repo_root, data_root, &prefix)
}

fn run_source_stage(plan: &SourceInstall, stage: Stage, dry_run: bool) -> Result<(), String> {
    let status = std::process::Command::new("/bin/bash")
        .args(plan.arguments(stage, dry_run, false))
        .current_dir(&plan.checkout)
        .status()
        .map_err(|error| {
            format!(
                "Could not execute source-install {}: {error}",
                stage.as_str()
            )
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Source-install {} failed; installation is not complete",
            stage.as_str()
        ))
    }
}

fn unavailable_selected(selection: &model::Selection) -> Vec<String> {
    let report = selection.report();
    let mut unavailable: Vec<String> = selection
        .graph
        .components
        .iter()
        .filter(|component| component.core || component.required)
        .filter(|component| {
            !report
                .component(&component.id)
                .is_some_and(|state| state.state.usable())
        })
        .map(|component| component.name.clone())
        .collect();
    for id in selection.wanted_ids() {
        if !report
            .feature(id)
            .is_some_and(|state| state.availability.is_available())
        {
            unavailable.push(
                selection
                    .features()
                    .iter()
                    .find(|feature| &feature.id == id)
                    .map(|feature| feature.name.clone())
                    .unwrap_or_else(|| id.clone()),
            );
        }
    }
    unavailable
}

fn confirm_run(count: usize) -> bool {
    use std::io::Write;
    print!(
        "\nInstall/start the native stack and set up {count} capability component(s) now? [Y/n] "
    );
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes")
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
    repo_root: &std::path::Path,
    data_root: &std::path::Path,
) -> ExitCode {
    let in_checkout = repo_root.join("Makefile").is_file();
    let mut real = engine::real::RealEffects::new(
        runtime,
        &selection.graph.components,
        repo_root.into(),
        data_root.into(),
    );
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
        .filter(|r| {
            matches!(
                r.outcome,
                engine::StepOutcome::NotVerified { .. }
                    | engine::StepOutcome::Blocked { .. }
                    | engine::StepOutcome::Unsupported { .. }
            )
        })
        .collect();
    if unfinished.is_empty() {
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
