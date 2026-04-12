//! `fand selftest` subcommand (FR-043 through FR-049, Phase 4 US2).
//!
//! Usage:
//!   fand selftest [--fan <N>] [--iterations <N>] [--json]
//!
//! Exit codes (FR-039 + FR-047):
//!   0  - PASS (zero round-trip mismatches AND delta ≥ 500 RPM)
//!   1  - FAIL (≥1 round-trip mismatch)
//!   2  - not root
//!   3  - INCONCLUSIVE (delta < 500 RPM, system was too cool to differentiate)
//!   4  - watchdog timeout
//!   5  - conflict (another fand instance holds the lock)
//!   64 - usage error
//!
//! See `src/smc/selftest.rs` for the F0md=0/1 oscillation design rationale
//! adapted for the Apple Silicon control surface (RD-08).

#![allow(clippy::print_stdout)] // CLI subcommand legitimately writes to stdout

use crate::cli::parse::parse_fan_index;
use crate::smc::ffi::SmcError;
use crate::smc::selftest::{
    DEFAULT_ITERATIONS, DELTA_THRESHOLD_RPM, SAMPLES_PER_HOLD, SelftestFanReport,
    SelftestReport, SelftestResult,
};
use crate::smc::write_session::WriteSession;

#[derive(Debug)]
struct CliSelftestArgs {
    target_fan: Option<u8>,
    iterations: u8,
    json_output: bool,
}

pub fn execute(args: &[String]) {
    std::process::exit(run_with_code(args));
}

fn run_with_code(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fand selftest: {e}");
            eprintln!("usage: fand selftest [--fan N] [--iterations N (1..=100)] [--json]");
            return 64;
        }
    };

    let mut session = match WriteSession::acquire() {
        Ok(s) => s,
        Err(e) => return code_from_smc_error(&e),
    };

    let fans_count = session.fans().len();
    if fans_count == 0 {
        // FR-066: vacuous pass on fanless chassis
        if parsed.json_output {
            print_vacuous_pass_json(&session);
        } else {
            println!("fand selftest: no fans on this machine — vacuous pass");
        }
        drop(session);
        return 0;
    }

    if let Some(idx) = parsed.target_fan {
        if usize::from(idx) >= fans_count {
            eprintln!(
                "fand selftest: --fan {} is out of range (this machine has {} fan{})",
                idx,
                fans_count,
                if fans_count == 1 { "" } else { "s" }
            );
            return 64;
        }
    }

    eprintln!(
        "fand selftest: starting {} iterations × {} fan{} (session {})",
        parsed.iterations,
        fans_count,
        if fans_count == 1 { "" } else { "s" },
        session.session_id()
    );
    eprintln!(
        "fand selftest: budget ~{} s per fan ({} hold windows × {} samples × 200 ms)",
        ((parsed.iterations as u32) * 2 * SAMPLES_PER_HOLD as u32 * 200) / 1000,
        parsed.iterations as u32 * 2,
        SAMPLES_PER_HOLD
    );

    // Run the per-fan loop. The selftest internally iterates every enumerated
    // fan; the --fan filter is enforced by the report layer.
    let report = match session.run_selftest(parsed.iterations) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("fand selftest: aborted: {e}");
            // Drop the session — teardown writes F0md=0 for all fans.
            drop(session);
            return code_from_smc_error(&e);
        }
    };

    if parsed.json_output {
        print_json(&session, &report);
    } else {
        print_human(&report);
    }

    // Drop the session — teardown writes F0md=0 + Ftst=0 + releases the flock.
    drop(session);

    report.overall_result.exit_code()
}

fn parse_args(args: &[String]) -> Result<CliSelftestArgs, String> {
    let mut target_fan: Option<u8> = None;
    let mut iterations: u8 = DEFAULT_ITERATIONS;
    let mut json_output = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--fan" => {
                i += 1;
                let v = args.get(i).ok_or_else(|| "--fan requires a value".to_string())?;
                target_fan = Some(parse_fan_index(v).map_err(|e| format!("--fan: {e}"))?);
            }
            "--iterations" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--iterations requires a value".to_string())?;
                let n: u8 = v
                    .parse()
                    .map_err(|_| "--iterations must be an integer 1..=100".to_string())?;
                if !(1..=100).contains(&n) {
                    return Err("--iterations must be in range 1..=100".into());
                }
                iterations = n;
            }
            "--json" => json_output = true,
            "--help" | "-h" => {
                return Err("see usage below".into());
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }

    Ok(CliSelftestArgs { target_fan, iterations, json_output })
}

fn print_human(report: &SelftestReport) {
    println!();
    println!("fand selftest — wall-clock {} ms", report.wall_clock_ms);
    println!();
    for fan in &report.per_fan {
        print_human_fan(fan);
    }
    println!("summary:");
    println!("  fans tested:       {}", report.per_fan.len());
    println!("  total iterations:  {}", report.total_iterations);
    println!("  round trips:       {}", report.total_round_trips);
    println!("  mismatches:        {}", report.total_mismatches);
    println!("  wall clock:        {} ms", report.wall_clock_ms);
    print!("  result:            ");
    print_result_label(report.overall_result);
    println!();
    println!();
}

fn print_human_fan(fan: &SelftestFanReport) {
    println!("fan {}:", fan.fan_index);
    println!(
        "  iterations:        {} / {}",
        fan.iterations_completed, fan.iterations_requested
    );
    println!(
        "  round trips:       {} ({} mismatch{})",
        fan.round_trip_count,
        fan.mismatch_count,
        if fan.mismatch_count == 1 { "" } else { "es" }
    );
    println!(
        "  median @ min:      {:.1} RPM  (target: F0md=1, ≈ Mn)",
        fan.median_actual_at_min
    );
    println!(
        "  median @ auto:     {:.1} RPM  (target: F0md=0, system controlled)",
        fan.median_actual_at_auto
    );
    let delta_marker = if fan.delta_rpm >= DELTA_THRESHOLD_RPM { "✓" } else { "✗" };
    println!(
        "  delta:             {:.1} RPM  {}  (>= {} required)",
        fan.delta_rpm, delta_marker, DELTA_THRESHOLD_RPM as i32
    );
    print!("  result:            ");
    print_result_label(fan.result);
    println!();
    println!();
}

fn print_result_label(result: SelftestResult) {
    match result {
        SelftestResult::Pass => print!("PASS"),
        SelftestResult::Inconclusive => {
            print!("INCONCLUSIVE — observed delta too small to verify the write path");
        }
        SelftestResult::Fail => print!("FAIL — round-trip mismatch detected"),
        SelftestResult::WatchdogTimeout => print!("WATCHDOG TIMEOUT"),
        SelftestResult::ConflictDetected => print!("CONFLICT"),
    }
}

fn print_vacuous_pass_json(session: &WriteSession) {
    let session_id = session.session_id();
    print!("{{");
    print!(r#""$schema":"https://pedrohbalbino.github.io/fand/schemas/selftest-v1.json","#);
    print!(r#""$id":"urn:fand:session:{session_id}","#);
    print!(r#""schema_version":1,"#);
    print!(r#""subcommand":"selftest","#);
    print!(r#""fand_version":"{}","#, env!("CARGO_PKG_VERSION"));
    print!(r#""session_id":"{session_id}","#);
    print!(r#""per_fan":[],"#);
    print!(r#""summary":{{"#);
    print!(r#""fans_tested":0,"#);
    print!(r#""total_iterations":0,"#);
    print!(r#""total_round_trips":0,"#);
    print!(r#""total_mismatches":0,"#);
    print!(r#""wall_clock_ms":0,"#);
    print!(r#""overall_result":"pass""#);
    println!(r#"}}}}"#);
}

fn print_json(session: &WriteSession, report: &SelftestReport) {
    let session_id = session.session_id();
    let fand_version = env!("CARGO_PKG_VERSION");
    print!("{{");
    print!(r#""$schema":"https://pedrohbalbino.github.io/fand/schemas/selftest-v1.json","#);
    print!(r#""$id":"urn:fand:session:{session_id}","#);
    print!(r#""schema_version":1,"#);
    print!(r#""subcommand":"selftest","#);
    print!(r#""fand_version":"{fand_version}","#);
    print!(r#""session_id":"{session_id}","#);
    print!(r#""per_fan":["#);
    for (idx, fan) in report.per_fan.iter().enumerate() {
        if idx > 0 {
            print!(",");
        }
        print!("{{");
        print!(r#""fan_index":{},"#, fan.fan_index);
        print!(r#""iterations_completed":{},"#, fan.iterations_completed);
        print!(r#""iterations_requested":{},"#, fan.iterations_requested);
        print!(r#""round_trip_count":{},"#, fan.round_trip_count);
        print!(r#""mismatch_count":{},"#, fan.mismatch_count);
        print!(r#""median_actual_at_min":{:.1},"#, fan.median_actual_at_min);
        print!(r#""median_actual_at_auto":{:.1},"#, fan.median_actual_at_auto);
        print!(r#""delta_rpm":{:.1},"#, fan.delta_rpm);
        print!(r#""result":"{}""#, fan.result.as_str());
        print!("}}");
    }
    print!(r#"],"#);
    print!(r#""summary":{{"#);
    print!(r#""fans_tested":{},"#, report.per_fan.len());
    print!(r#""total_iterations":{},"#, report.total_iterations);
    print!(r#""total_round_trips":{},"#, report.total_round_trips);
    print!(r#""total_mismatches":{},"#, report.total_mismatches);
    print!(r#""wall_clock_ms":{},"#, report.wall_clock_ms);
    print!(r#""overall_result":"{}""#, report.overall_result.as_str());
    println!("}}}}");
}

fn code_from_smc_error(e: &SmcError) -> i32 {
    match e {
        SmcError::ConflictDetected { .. } => {
            eprintln!("fand selftest: {e}");
            5
        }
        SmcError::OpenFailed(_) | SmcError::ServiceNotFound => {
            eprintln!("fand selftest: {e}");
            eprintln!("  hint: run as root (sudo fand selftest ...)");
            2
        }
        SmcError::WatchdogFired { .. } => {
            eprintln!("fand selftest: {e}");
            4
        }
        _ => {
            eprintln!("fand selftest: {e}");
            1
        }
    }
}
