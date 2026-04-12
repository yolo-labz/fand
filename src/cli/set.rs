//! `fand set` subcommand (FR-035 through FR-042, Phase 3 US1).
//!
//! Usage:
//!   fand set --fan <N> --rpm <V> (--dry-run | --commit) [--json]
//!
//! Exit codes (FR-039):
//!   0  - success
//!   1  - IOKit-level error
//!   2  - not root
//!   3  - no fans / unsupported SoC
//!   4  - userspace watchdog timeout
//!   5  - conflict detected (another fand instance holds the lock)
//!   64 - usage error
//!
//! Feature 005 Phase 3 status: **dry-run mode is complete**. Commit mode
//! is a skeleton that currently reports "not yet wired" and exits 1. The
//! commit path requires round-trip-verified `write_fan_mode_verified`,
//! `write_fan_target_verified`, and the GCD watchdog timer wiring —
//! tracked as T057-T062 in the follow-up session.

#![allow(clippy::print_stdout)] // FR-036 dry-run preview writes to stdout by design

use crate::cli::parse::{parse_fan_index, parse_rpm};
use crate::control::state::ClampedRpm;
use crate::smc::ffi::SmcError;
use crate::smc::write_session::WriteSession;

/// Parsed command-line arguments for `fand set`.
#[derive(Debug)]
struct CliSetArgs {
    fan_index: u8,
    raw_rpm: f32,
    mode: Mode,
    json_output: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    DryRun,
    Commit,
}

/// CLI entry point — matches `cli::keys::execute` signature.
pub fn execute(args: &[String]) {
    std::process::exit(run_with_code(args));
}

fn run_with_code(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fand set: {e}");
            eprintln!("usage: fand set --fan <N> --rpm <V> (--dry-run | --commit) [--json]");
            return 64;
        }
    };

    let mut session = match WriteSession::acquire() {
        Ok(s) => s,
        Err(e) => return code_from_smc_error(&e),
    };

    let fans = session.fans();
    if fans.is_empty() {
        eprintln!("fand set: no fans on this machine — nothing to set");
        return 3;
    }

    if usize::from(parsed.fan_index) >= fans.len() {
        eprintln!(
            "fand set: --fan {} is out of range (this machine has {} fan{})",
            parsed.fan_index,
            fans.len(),
            if fans.len() == 1 { "" } else { "s" }
        );
        return 64;
    }

    let fan = &fans[usize::from(parsed.fan_index)];
    let clamped = ClampedRpm::new(parsed.raw_rpm, fan.min_rpm, fan.max_rpm);
    let was_clamped = ClampedRpm::was_clamped(parsed.raw_rpm, fan.min_rpm, fan.max_rpm);

    match parsed.mode {
        Mode::DryRun => {
            if parsed.json_output {
                print_dry_run_json(&parsed, fan, clamped, was_clamped, &session);
            } else {
                print_dry_run_human(&parsed, fan, clamped, was_clamped);
            }
            drop(session);
            0
        }
        Mode::Commit => {
            if was_clamped {
                eprintln!(
                    "fand set: clamped {:.1} → {} RPM (fan {} envelope [{:.1}, {:.1}])",
                    parsed.raw_rpm,
                    clamped.value(),
                    parsed.fan_index,
                    fan.min_rpm,
                    fan.max_rpm
                );
            }
            eprintln!(
                "fand set: committing {} RPM on fan {} (session {})",
                clamped.value(),
                parsed.fan_index,
                session.session_id()
            );
            // Round-trip-verified commit sequence: Ftst=1 → F<i>Md=1 → F<i>Tg=<v>.
            if let Err(e) = session.commit_set_fan(parsed.fan_index, clamped) {
                eprintln!("fand set: commit failed: {e}");
                drop(session); // teardown runs in Drop
                return code_from_smc_error(&e);
            }
            eprintln!(
                "fand set: commit succeeded — holding override. Press Ctrl-C to release."
            );

            // Enter the hold loop. The dedicated signal thread (RD-03) is
            // blocked on `Signals::forever()` and will call `std::process::exit(0)`
            // on SIGINT/SIGTERM, which kills the whole process including this
            // main thread. We park the main thread on a condvar with a heartbeat
            // timeout so the `DiagnosticUnlockSession::watchdog_fired()` path
            // can still observe a stalled tick.
            hold_loop(&mut session);

            // Unreachable in practice — the signal thread takes over and exits.
            drop(session);
            0
        }
    }
}

/// Park the main thread while the override is held. Periodically rearms the
/// watchdog by calling `heartbeat()` on the unlock session (via a no-op
/// read-back). The signal thread owns the exit path — this loop runs until
/// SIGINT/SIGTERM kills the process from underneath us.
fn hold_loop(session: &mut WriteSession) {
    use std::thread::sleep;
    use std::time::Duration;
    loop {
        sleep(Duration::from_millis(500));
        // Heartbeat the watchdog so the 4-second FR-002 timer doesn't fire
        // while the override is legitimately held.
        session.heartbeat_unlock();
    }
}

fn parse_args(args: &[String]) -> Result<CliSetArgs, String> {
    let mut fan_index: Option<u8> = None;
    let mut raw_rpm: Option<f32> = None;
    let mut mode: Option<Mode> = None;
    let mut json_output = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--fan" => {
                i += 1;
                let v = args.get(i).ok_or_else(|| "--fan requires a value".to_string())?;
                let parsed = parse_fan_index(v).map_err(|e| format!("--fan: {e}"))?;
                fan_index = Some(parsed);
            }
            "--rpm" => {
                i += 1;
                let v = args.get(i).ok_or_else(|| "--rpm requires a value".to_string())?;
                let parsed = parse_rpm(v).map_err(|e| format!("--rpm: {e}"))?;
                raw_rpm = Some(parsed);
            }
            "--dry-run" => {
                if mode.is_some() {
                    return Err("exactly one of --dry-run or --commit is required".into());
                }
                mode = Some(Mode::DryRun);
            }
            "--commit" => {
                if mode.is_some() {
                    return Err("exactly one of --dry-run or --commit is required".into());
                }
                mode = Some(Mode::Commit);
            }
            "--json" => json_output = true,
            "--help" | "-h" => {
                return Err("see usage below".into());
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }

    let fan_index = fan_index.ok_or_else(|| "--fan is required".to_string())?;
    let raw_rpm = raw_rpm.ok_or_else(|| "--rpm is required".to_string())?;
    let mode = mode.ok_or_else(|| "one of --dry-run or --commit is required".to_string())?;

    Ok(CliSetArgs { fan_index, raw_rpm, mode, json_output })
}

fn print_dry_run_human(
    parsed: &CliSetArgs,
    fan: &crate::smc::enumerate::Fan,
    clamped: ClampedRpm,
    was_clamped: bool,
) {
    println!("fand set --fan {} --rpm {} --dry-run", parsed.fan_index, parsed.raw_rpm);
    println!("  fan:            {}", parsed.fan_index);
    println!("  envelope:       [{:.1}, {:.1}]", fan.min_rpm, fan.max_rpm);
    println!("  raw request:    {:.1}", parsed.raw_rpm);
    if was_clamped {
        println!(
            "  clamped target: {} RPM  (CLAMPED from {:.1})",
            clamped.value(),
            parsed.raw_rpm
        );
    } else {
        println!(
            "  clamped target: {} RPM  (within envelope, no clamp applied)",
            clamped.value()
        );
    }

    // Per RD-08 session 5 live findings on Mac17,2: Apple Silicon M-series
    // supports only F0md=1 (forced minimum). Determine which path the
    // commit would take and print the appropriate plan.
    let min_tolerance = (fan.max_rpm - fan.min_rpm) * 0.05;
    let target_rpm = clamped.as_f32();
    let is_minimum_request = (target_rpm - fan.min_rpm).abs() <= min_tolerance;

    println!("  planned writes (Apple Silicon M-series control surface):");
    if is_minimum_request {
        println!(
            "    1. F{}md = 1               (force minimum — fan will drop to ~{} RPM)",
            parsed.fan_index, fan.min_rpm as u32
        );
        println!("  round-trip reads (expected):");
        println!("    F{}md    = 1", parsed.fan_index);
        println!("  teardown on exit:");
        println!("    F{}md = 0  (return to system thermal manager)", parsed.fan_index);
    } else {
        println!(
            "    (none — Apple Silicon M-series exposes only F0md=0/auto and F0md=1/min;"
        );
        println!("     arbitrary RPM targets are not writable via the SMC interface");
        println!("     per research.md RD-08. --commit at this RPM would be REJECTED.)");
        println!();
        println!("  to engage forced minimum:  fand set --fan {} --rpm {} --commit", parsed.fan_index, fan.min_rpm as u32);
    }
}

fn print_dry_run_json(
    parsed: &CliSetArgs,
    fan: &crate::smc::enumerate::Fan,
    clamped: ClampedRpm,
    was_clamped: bool,
    session: &WriteSession,
) {
    // Hand-rolled JSON — consistent with the `fand keys --json` style.
    // FR-038: must carry $schema and $id; FR-100: must carry the session
    // correlation ID.
    let session_id = session.session_id();
    let fand_version = env!("CARGO_PKG_VERSION");
    print!("{{");
    print!(r#""$schema":"https://pedrohbalbino.github.io/fand/schemas/dry-run-v1.json","#);
    print!(r#""$id":"urn:fand:session:{session_id}","#);
    print!(r#""schema_version":1,"#);
    print!(r#""subcommand":"set","#);
    print!(r#""mode":"dry-run","#);
    print!(r#""fand_version":"{fand_version}","#);
    print!(r#""session_id":"{session_id}","#);
    print!(r#""fan":{{"#);
    print!(r#""index":{},"#, parsed.fan_index);
    print!(r#""min_rpm":{:.1},"#, fan.min_rpm);
    print!(r#""max_rpm":{:.1},"#, fan.max_rpm);
    print!(
        r#""mode_key":"{}""#,
        fourcc_display(fan.mode_key)
    );
    print!(r#"}},"#);
    print!(r#""request":{{"#);
    print!(r#""raw_rpm":{:.1},"#, parsed.raw_rpm);
    print!(r#""clamped_rpm":{},"#, clamped.value());
    print!(r#""was_clamped":{}"#, was_clamped);
    print!(r#"}},"#);
    print!(r#""planned_writes":["#);
    print!(r#"{{"step":1,"key":"Ftst","value":1}},"#);
    print!(
        r#"{{"step":2,"key":"F{}Md","value":1}},"#,
        parsed.fan_index
    );
    print!(
        r#"{{"step":3,"key":"F{}Tg","value":{}}}"#,
        parsed.fan_index,
        clamped.value()
    );
    print!(r#"],"#);
    print!(r#""planned_teardown":["#);
    print!(
        r#"{{"step":1,"key":"F{}Md","value":0}},"#,
        parsed.fan_index
    );
    print!(r#"{{"step":2,"key":"Ftst","value":0}}"#);
    print!(r#"]"#);
    println!("}}");
}

fn fourcc_display(fourcc: u32) -> String {
    let bytes = fourcc.to_be_bytes();
    bytes
        .iter()
        .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
        .collect()
}

fn code_from_smc_error(e: &SmcError) -> i32 {
    match e {
        SmcError::ConflictDetected { .. } => {
            eprintln!("fand set: {e}");
            5
        }
        SmcError::OpenFailed(_) | SmcError::ServiceNotFound => {
            eprintln!("fand set: {e}");
            eprintln!("  hint: run as root (sudo fand set ...)");
            2
        }
        SmcError::KeyNotFound(_) => {
            eprintln!("fand set: {e}");
            eprintln!("  hint: this fan may be unsupported on this SoC");
            3
        }
        SmcError::WatchdogFired { .. } => {
            eprintln!("fand set: {e}");
            4
        }
        _ => {
            eprintln!("fand set: {e}");
            1
        }
    }
}
