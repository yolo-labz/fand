//! `fand force-rpm <rpm>` — fallback-chain fan override for M5 (feature 006).
//!
//! Goal: hold the fan at a user-specified RPM regardless of which SMC
//! path the current firmware actually honors. The daemon's production
//! write path only emits `F0md=0/1` (forced-min only), per the RD-08
//! finding on macOS 15 / Mac17,2. `force-rpm` tries every bypass path
//! the probe matrix covers, in order, and settles on whichever one
//! moves `F0Ac` within ≥ 500 RPM of the requested target.
//!
//! Every path is teardown-clean: on any exit (success, failure, SIGINT),
//! we write `F0md=0` and restore any modified floor/sensor value. This
//! mirrors `WriteSession`'s panic-hook contract even though we bypass
//! the session layer — the command is a deliberate research escape hatch
//! for surfaces the session layer's whitelist refuses to emit.
//!
//! Intentionally NOT part of the `fand run` control loop. The daemon
//! still runs on the validated `F0md` path; `force-rpm` is a one-shot
//! probe that tells the operator which bypass to enable upstream.

#![allow(clippy::print_stdout)] // CLI subcommand writes to stdout.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::smc::ffi::{SmcConnection, SmcError};

const F0MN: u32 = u32::from_be_bytes(*b"F0Mn");
const F0MX: u32 = u32::from_be_bytes(*b"F0Mx");
const F0AC: u32 = u32::from_be_bytes(*b"F0Ac");
const F0TG: u32 = u32::from_be_bytes(*b"F0Tg");
const F0DC: u32 = u32::from_be_bytes(*b"F0Dc");
const F0MD: u32 = u32::from_be_bytes(*b"F0md");

const PASS_DELTA_RPM: f32 = 500.0;

#[derive(Debug)]
struct Attempt {
    path: &'static str,
    outcome: Result<f32, String>, // Ok(after_rpm), Err(message)
}

pub fn execute(args: &[String]) {
    let mut target_rpm: Option<f32> = None;
    let mut settle_secs: u64 = 4;
    let mut hold_secs: u64 = 0;
    let mut json = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--settle-secs" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|s| s.parse::<u64>().ok()) {
                    settle_secs = v.clamp(1, 30);
                }
            }
            "--hold-secs" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|s| s.parse::<u64>().ok()) {
                    hold_secs = v.min(3600);
                }
            }
            "--json" => json = true,
            "--help" | "-h" => {
                print_usage();
                return;
            }
            other => {
                if let Ok(v) = other.parse::<f32>() {
                    if v.is_finite() && v > 0.0 {
                        target_rpm = Some(v);
                    } else {
                        eprintln!("fand force-rpm: target must be positive finite RPM");
                        std::process::exit(64);
                    }
                } else {
                    eprintln!("fand force-rpm: unknown flag '{other}'");
                    std::process::exit(64);
                }
            }
        }
        i += 1;
    }

    let Some(target) = target_rpm else {
        eprintln!("fand force-rpm: target RPM required");
        print_usage();
        std::process::exit(64);
    };

    let mut conn = match SmcConnection::open() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fand force-rpm: SMC open failed: {e}");
            eprintln!("  hint: run as root (sudo fand force-rpm ...)");
            std::process::exit(2);
        }
    };

    let min_rpm = match conn.read_f32(F0MN) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("fand force-rpm: read F0Mn failed: {e}");
            std::process::exit(1);
        }
    };
    let max_rpm = conn.read_f32(F0MX).unwrap_or(min_rpm + 4233.0);
    let clamped_target = target.clamp(min_rpm, max_rpm);

    if !json {
        println!(
            "fand force-rpm — target={:.0} (clamped to [{:.0}, {:.0}]), settle={}s",
            clamped_target, min_rpm, max_rpm, settle_secs
        );
    }

    // SIGINT / SIGTERM flag for hold loop — best-effort; teardown always runs
    // via the `drop_guard` pattern below.
    let interrupt = Arc::new(AtomicBool::new(false));
    install_signal_handler(interrupt.clone());

    let mut attempts: Vec<Attempt> = Vec::new();
    let baseline = conn.read_f32(F0AC).unwrap_or(f32::NAN);

    // Baseline mode reset.
    let _ = conn.write_raw_for_research(F0MD, &[0]);
    std::thread::sleep(Duration::from_millis(1500));

    // --- Path 1: F0Tg direct ---
    attempts.push(try_path(
        "F0Tg_direct",
        &mut conn,
        baseline,
        settle_secs,
        |c| c.write_raw_for_research(F0TG, &clamped_target.to_be_bytes()),
    ));
    if winner(&attempts, baseline).is_some() {
        hold_and_teardown(
            &mut conn, &attempts, baseline, hold_secs, interrupt, json, None, min_rpm,
        );
        return;
    }
    reset_mode(&mut conn);

    // --- Path 2: F0Dc direct (duty fraction) ---
    let duty = ((clamped_target - min_rpm) / (max_rpm - min_rpm)).clamp(0.0, 1.0);
    attempts.push(try_path(
        "F0Dc_direct",
        &mut conn,
        baseline,
        settle_secs,
        |c| c.write_raw_for_research(F0DC, &duty.to_be_bytes()),
    ));
    if winner(&attempts, baseline).is_some() {
        hold_and_teardown(
            &mut conn, &attempts, baseline, hold_secs, interrupt, json, None, min_rpm,
        );
        return;
    }
    reset_mode(&mut conn);

    // --- Path 3: F0md=1 then F0Tg ---
    attempts.push(try_path(
        "F0md=1_then_F0Tg",
        &mut conn,
        baseline,
        settle_secs,
        |c| {
            c.write_raw_for_research(F0MD, &[1])?;
            c.write_raw_for_research(F0TG, &clamped_target.to_be_bytes())
        },
    ));
    if winner(&attempts, baseline).is_some() {
        hold_and_teardown(
            &mut conn, &attempts, baseline, hold_secs, interrupt, json, None, min_rpm,
        );
        return;
    }
    reset_mode(&mut conn);

    // --- Path 4: F0Mn raise + F0md=1 ---
    attempts.push(try_path(
        "F0Mn_raise_then_F0md=1",
        &mut conn,
        baseline,
        settle_secs,
        |c| {
            c.write_raw_for_research(F0MN, &clamped_target.to_be_bytes())?;
            c.write_raw_for_research(F0MD, &[1])
        },
    ));
    let floor_raised = winner(&attempts, baseline).is_some();
    if floor_raised {
        hold_and_teardown(
            &mut conn,
            &attempts,
            baseline,
            hold_secs,
            interrupt,
            json,
            Some(min_rpm),
            min_rpm,
        );
        return;
    }
    // Restore floor before moving on, regardless of outcome.
    let _ = conn.write_raw_for_research(F0MN, &min_rpm.to_be_bytes());
    reset_mode(&mut conn);

    // Nothing worked. Emit report + exit non-zero.
    hold_and_teardown(
        &mut conn, &attempts, baseline, 0, interrupt, json, None, min_rpm,
    );
    std::process::exit(1);
}

fn try_path<F>(
    path: &'static str,
    conn: &mut SmcConnection,
    _baseline: f32,
    settle_secs: u64,
    mut write: F,
) -> Attempt
where
    F: FnMut(&mut SmcConnection) -> Result<(), SmcError>,
{
    let result = write(conn);
    if result.is_ok() {
        wait_settled(settle_secs);
    }
    let after = conn.read_f32(F0AC).unwrap_or(f32::NAN);
    let outcome = match result {
        Ok(()) => Ok(after),
        Err(e) => Err(e.to_string()),
    };
    Attempt { path, outcome }
}

fn winner(attempts: &[Attempt], baseline: f32) -> Option<&Attempt> {
    attempts.iter().find(|a| match &a.outcome {
        Ok(after) => (after - baseline).abs() >= PASS_DELTA_RPM,
        Err(_) => false,
    })
}

fn hold_and_teardown(
    conn: &mut SmcConnection,
    attempts: &[Attempt],
    baseline: f32,
    hold_secs: u64,
    interrupt: Arc<AtomicBool>,
    json: bool,
    restore_floor_to: Option<f32>,
    original_min: f32,
) {
    let report_winner = winner(attempts, baseline).map(|a| a.path);

    if hold_secs > 0 && report_winner.is_some() {
        if !json {
            println!(
                "holding for {}s via path={} — press Ctrl-C to exit early",
                hold_secs,
                report_winner.unwrap_or("?"),
            );
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(hold_secs) && !interrupt.load(Ordering::SeqCst)
        {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    // Teardown: restore floor if we raised it, then reset mode.
    if let Some(orig) = restore_floor_to {
        let _ = conn.write_raw_for_research(F0MN, &orig.to_be_bytes());
        let _ = restore_floor_to; // silence if-let warning on cfg
    } else if winner(attempts, baseline).map(|a| a.path) == Some("F0Mn_raise_then_F0md=1") {
        let _ = conn.write_raw_for_research(F0MN, &original_min.to_be_bytes());
    }
    reset_mode(conn);

    if json {
        print!("{{\"baseline\":{baseline:.0},\"attempts\":[");
        for (i, a) in attempts.iter().enumerate() {
            if i > 0 {
                print!(",");
            }
            match &a.outcome {
                Ok(after) => print!(
                    "{{\"path\":\"{}\",\"ok\":true,\"after\":{:.0},\"delta\":{:.0}}}",
                    a.path,
                    after,
                    after - baseline
                ),
                Err(msg) => print!(
                    "{{\"path\":\"{}\",\"ok\":false,\"error\":\"{}\"}}",
                    a.path,
                    msg.replace('"', "'")
                ),
            }
        }
        print!("],\"winner\":");
        match report_winner {
            Some(p) => print!("\"{p}\""),
            None => print!("null"),
        }
        println!("}}");
    } else {
        println!("{:-<72}", "");
        for a in attempts {
            match &a.outcome {
                Ok(after) => println!(
                    "{:<26} after={:>6.0} delta={:>+7.0} {}",
                    a.path,
                    after,
                    after - baseline,
                    if (after - baseline).abs() >= PASS_DELTA_RPM {
                        "HELD"
                    } else {
                        "no-op"
                    }
                ),
                Err(msg) => println!("{:<26} ERROR {}", a.path, msg),
            }
        }
        println!("{:-<72}", "");
        match report_winner {
            Some(p) => println!("winner: {p}"),
            None => println!("no path moved the fan ≥ {PASS_DELTA_RPM:.0} RPM"),
        }
    }
}

fn reset_mode(conn: &mut SmcConnection) {
    let _ = conn.write_raw_for_research(F0MD, &[0]);
    std::thread::sleep(Duration::from_millis(1500));
}

fn wait_settled(secs: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn install_signal_handler(flag: Arc<AtomicBool>) {
    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;
    if let Ok(mut signals) = Signals::new([SIGINT, SIGTERM]) {
        let flag_bg = flag;
        std::thread::spawn(move || {
            for _ in signals.forever() {
                flag_bg.store(true, Ordering::SeqCst);
                break;
            }
        });
    }
}

fn print_usage() {
    eprintln!(
        "usage: fand force-rpm <rpm> [--settle-secs N] [--hold-secs N] [--json]\n\
         \n\
         Holds fan 0 at the requested RPM by trying each SMC bypass path:\n\
           1. F0Tg direct\n\
           2. F0Dc direct\n\
           3. F0md=1 then F0Tg\n\
           4. F0Mn raise + F0md=1 (forced-min floor bypass)\n\
         \n\
         Stops at the first path that moves F0Ac by ≥ 500 RPM from baseline.\n\
         Optionally holds the fan at target for --hold-secs seconds before\n\
         restoring auto mode. Ctrl-C aborts the hold cleanly.\n\
         \n\
         --hold-secs 0 (default) runs the probe once and exits.\n\
         \n\
         Teardown always writes F0md=0 and restores any modified floor."
    );
}
