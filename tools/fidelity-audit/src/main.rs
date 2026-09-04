//! `fidelity-audit` CLI.
//!
//! Report mode (default): print the per-component divergence ledger and always
//! exit 0. Gate mode (`--gate`): exit 1 if any unaccepted divergence remains.

use std::path::PathBuf;
use std::process::ExitCode;

use fidelity_audit::{Config, OutputFormat, run};

const USAGE: &str = "\
fidelity-audit — per-component mockup-vs-render fidelity ledger

USAGE:
    fidelity-audit [--renders-dir <dir>] [--shell-bin <path>] [options]

INPUTS (one of):
    --renders-dir <dir>   Directory of pre-produced <slug>.json + <slug>.png
                          (from `pf-shell --offscreen --out <dir>`).
    --shell-bin <path>    Render the slugs first by invoking this pf-shell binary
                          into --renders-dir, then audit.

OPTIONS:
    --route <id>          Audit only this route (repeatable). Default: all mapped.
    --out <dir>           Artifact + ledger output dir. Default: target/fidelity-audit.
    --repo-root <dir>     Launcher repo root (for golden renders). Default: crate/../..
    --crate-dir <dir>     Dir holding design-facts/, mapping/, baseline/. Default: this crate.
    --format table|json   Ledger output format on stdout. Default: table.
    --gate                Exit non-zero on any unaccepted divergence (default: report only).
    -h, --help            Show this help.

EXIT: 0 report ok / gate passed; 1 gate failed; 2 usage or input error.
";

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("fidelity-audit: {e}");
            ExitCode::from(2)
        }
    }
}

fn real_main() -> Result<ExitCode, String> {
    let crate_dir = Config::default_crate_dir();
    let mut renders_dir: Option<PathBuf> = None;
    let mut shell_bin: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut repo_root: Option<PathBuf> = None;
    let mut crate_dir_override: Option<PathBuf> = None;
    let mut routes: Vec<String> = Vec::new();
    let mut gate = false;
    let mut format = OutputFormat::Table;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            "--renders-dir" => renders_dir = Some(next(&mut args, &arg)?.into()),
            "--shell-bin" => shell_bin = Some(next(&mut args, &arg)?.into()),
            "--out" => out_dir = Some(next(&mut args, &arg)?.into()),
            "--repo-root" => repo_root = Some(next(&mut args, &arg)?.into()),
            "--crate-dir" => crate_dir_override = Some(next(&mut args, &arg)?.into()),
            "--route" => routes.push(next(&mut args, &arg)?),
            "--gate" => gate = true,
            "--format" => {
                format = match next(&mut args, &arg)?.as_str() {
                    "table" => OutputFormat::Table,
                    "json" => OutputFormat::Json,
                    other => return Err(format!("unknown --format {other} (want table|json)")),
                }
            }
            other => return Err(format!("unknown argument {other} (try --help)")),
        }
    }

    let crate_dir = crate_dir_override.unwrap_or(crate_dir);
    let repo_root = repo_root.unwrap_or_else(|| crate_dir.join("../.."));
    let renders_dir =
        renders_dir.unwrap_or_else(|| repo_root.join("target/fidelity-audit/renders"));
    let out_dir = out_dir.unwrap_or_else(|| repo_root.join("target/fidelity-audit"));

    if shell_bin.is_none() && !renders_dir.join("boot-home.semantic.txt").exists() {
        eprintln!(
            "fidelity-audit: note — --renders-dir {} has no renders and no --shell-bin given; \
             pass --shell-bin <pf-shell> to render, or point --renders-dir at an offscreen output.",
            renders_dir.display()
        );
    }

    let config = Config {
        crate_dir,
        repo_root,
        renders_dir,
        out_dir,
        shell_bin,
        routes: if routes.is_empty() {
            None
        } else {
            Some(routes)
        },
        gate,
        format,
    };

    let result = run(&config)?;
    let ledger = &result.ledger;

    match format {
        OutputFormat::Table => print!("{}", ledger.to_table()),
        OutputFormat::Json => println!("{}", ledger.to_json()?),
    }

    eprintln!(
        "fidelity-audit: {} findings ({} divergences, {} gating); ledger -> {}",
        ledger.findings.len(),
        ledger.divergences(),
        ledger.gating(),
        result.ledger_path.display()
    );

    if gate && ledger.gating() > 0 {
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

fn next(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}
