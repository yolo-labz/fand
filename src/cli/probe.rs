//! `fand probe` — live hardware probe of every known SMC fan-write path.
//!
//! Purpose: find out, empirically, which SMC write path actually moves the
//! fan on *this* SoC revision. The `RD-08` finding (feature 005, Mac17,2)
//! concluded that only `F0md=1` / `F0md=0` accept writes. That was true at
//! the time and for the firmware shipped with macOS 15. As macOS and the
//! M-series RTKit firmware ship revisions, the write surface can change —
//! agoodkind/macos-smc-fan and Macs Fan Control 1.5.18 report direct
//! `F0Tg` control restored on M3/M4 in 2025. This probe tests each path
//! and prints a pass/fail matrix so we can decide, with data, whether to
//! lift the forced-min-only gate in `WriteSession::commit_set_fan`.
//!
//! Probe is read-mostly. Every write either succeeds with a visible fan
//! response or fails fast; we never leave the SMC in a non-auto state on
//! exit. The teardown writes `F0md=0` unconditionally, matching the
//! `WriteSession` panic-hook contract.

#![allow(clippy::print_stdout)] // CLI subcommand writes to stdout.

use std::time::{Duration, Instant};

use crate::smc::ffi::{SmcConnection, SmcError};

const F0MN: u32 = u32::from_be_bytes(*b"F0Mn");
const F0MX: u32 = u32::from_be_bytes(*b"F0Mx");
const F0AC: u32 = u32::from_be_bytes(*b"F0Ac");
const F0TG: u32 = u32::from_be_bytes(*b"F0Tg");
const F0DC: u32 = u32::from_be_bytes(*b"F0Dc");
const F0MD: u32 = u32::from_be_bytes(*b"F0md");

/// Each probe attempts a specific write, waits for the fan to respond, and
/// records the actual RPM (`F0Ac`) at the end of the settle window.
#[derive(Debug)]
struct ProbeOutcome {
    path: &'static str,
    write_result: Result<(), String>,
    actual_rpm_before: f32,
    actual_rpm_after: f32,
    delta_rpm: f32,
}

impl ProbeOutcome {
    fn passed(&self, min_delta: f32) -> bool {
        self.write_result.is_ok() && self.delta_rpm.abs() >= min_delta
    }
}

pub fn execute(args: &[String]) {
    let mut settle_secs: u64 = 4;
    let mut target_rpm: f32 = 0.0; // 0 → use min + 1500 by default
    let mut json = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--settle-secs" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(parsed) = v.parse::<u64>() {
                        settle_secs = parsed.clamp(1, 30);
                    }
                }
            }
            "--target-rpm" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    if let Ok(parsed) = v.parse::<f32>() {
                        if parsed.is_finite() && parsed > 0.0 {
                            target_rpm = parsed;
                        }
                    }
                }
            }
            "--json" => json = true,
            "--help" | "-h" => {
                eprintln!(
                    "usage: fand probe [--settle-secs N] [--target-rpm RPM] [--json]\n\
                     \n\
                     Runs the full SMC fan-write probe matrix against fan 0:\n\
                       1. F0Tg direct (float RPM)\n\
                       2. F0Dc direct (float duty cycle 0..1)\n\
                       3. F0md=1 then F0Tg (combined forced-min + target)\n\
                     \n\
                     Each path reads F0Ac before and after a settle window.\n\
                     A path passes if the actual RPM deviates by ≥ 500 from\n\
                     baseline. Teardown always writes F0md=0 (auto)."
                );
                return;
            }
            other => {
                eprintln!("fand probe: unknown flag '{other}'");
                std::process::exit(64);
            }
        }
        i += 1;
    }

    let mut conn = match SmcConnection::open() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fand probe: SMC open failed: {e}");
            eprintln!("  hint: run as root (sudo fand probe)");
            std::process::exit(2);
        }
    };

    let min_rpm = match conn.read_f32(F0MN) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("fand probe: read F0Mn failed: {e}");
            std::process::exit(1);
        }
    };
    let max_rpm = conn.read_f32(F0MX).unwrap_or(min_rpm + 4233.0);

    let effective_target = if target_rpm > 0.0 {
        target_rpm.clamp(min_rpm, max_rpm)
    } else {
        (min_rpm + 1500.0).min(max_rpm - 100.0)
    };

    if !json {
        println!(
            "fand probe — fan 0 envelope min={min_rpm:.0} max={max_rpm:.0}, target={effective_target:.0}, settle={settle_secs}s"
        );
    }

    let mut outcomes: Vec<ProbeOutcome> = Vec::new();

    // Ensure we start from auto mode so each path begins with a clean baseline.
    let _ = conn.write_raw_for_research(F0MD, &[0]);
    std::thread::sleep(Duration::from_secs(2));

    outcomes.push(probe_direct_f32(
        &mut conn,
        "F0Tg_direct",
        F0TG,
        effective_target,
        settle_secs,
    ));
    let _ = conn.write_raw_for_research(F0MD, &[0]);
    std::thread::sleep(Duration::from_secs(2));

    let duty_target = ((effective_target - min_rpm) / (max_rpm - min_rpm)).clamp(0.0, 1.0);
    outcomes.push(probe_direct_f32(
        &mut conn,
        "F0Dc_direct",
        F0DC,
        duty_target,
        settle_secs,
    ));
    let _ = conn.write_raw_for_research(F0MD, &[0]);
    std::thread::sleep(Duration::from_secs(2));

    outcomes.push(probe_mode_then_target(
        &mut conn,
        effective_target,
        settle_secs,
    ));

    // Teardown — always back to auto.
    let _ = conn.write_raw_for_research(F0MD, &[0]);

    if json {
        print!("{{\"min_rpm\":{min_rpm:.0},\"max_rpm\":{max_rpm:.0},\"target_rpm\":{effective_target:.0},\"settle_secs\":{settle_secs},\"paths\":[");
        for (i, o) in outcomes.iter().enumerate() {
            if i > 0 {
                print!(",");
            }
            let err_field = match &o.write_result {
                Ok(()) => "null".to_string(),
                Err(s) => format!("\"{}\"", s.replace('"', "'")),
            };
            print!(
                "{{\"path\":\"{}\",\"ok\":{},\"error\":{},\"before\":{:.0},\"after\":{:.0},\"delta\":{:.0},\"passed\":{}}}",
                o.path,
                o.write_result.is_ok(),
                err_field,
                o.actual_rpm_before,
                o.actual_rpm_after,
                o.delta_rpm,
                o.passed(500.0),
            );
        }
        println!("]}}");
    } else {
        println!("{:-<78}", "");
        println!(
            "{:<20} {:>10} {:>10} {:>10} {:>8} {}",
            "path", "before", "after", "delta", "passed", "error"
        );
        println!("{:-<78}", "");
        for o in &outcomes {
            let err = match &o.write_result {
                Ok(()) => String::new(),
                Err(s) => s.clone(),
            };
            println!(
                "{:<20} {:>10.0} {:>10.0} {:>10.0} {:>8} {}",
                o.path,
                o.actual_rpm_before,
                o.actual_rpm_after,
                o.delta_rpm,
                o.passed(500.0),
                err
            );
        }
        println!("{:-<78}", "");
        let any_pass = outcomes.iter().any(|o| o.passed(500.0));
        if any_pass {
            println!(
                "at least one path moved the fan ≥500 RPM — the forced-min-only gate can be lifted for that path"
            );
        } else {
            println!(
                "no direct path moved the fan — SMC surface remains F0md=0/1 only on this SoC"
            );
        }
    }

    std::process::exit(if outcomes.iter().any(|o| o.passed(500.0)) {
        0
    } else {
        1
    });
}

fn probe_direct_f32(
    conn: &mut SmcConnection,
    path: &'static str,
    fourcc: u32,
    value: f32,
    settle_secs: u64,
) -> ProbeOutcome {
    let before = conn.read_f32(F0AC).unwrap_or(f32::NAN);
    let bytes = value.to_be_bytes();
    let write_result = conn
        .write_raw_for_research(fourcc, &bytes)
        .map_err(|e: SmcError| e.to_string());

    if write_result.is_ok() {
        wait_settled(settle_secs);
    }

    let after = conn.read_f32(F0AC).unwrap_or(f32::NAN);
    let delta = after - before;
    ProbeOutcome {
        path,
        write_result,
        actual_rpm_before: before,
        actual_rpm_after: after,
        delta_rpm: delta,
    }
}

fn probe_mode_then_target(
    conn: &mut SmcConnection,
    target_rpm: f32,
    settle_secs: u64,
) -> ProbeOutcome {
    let before = conn.read_f32(F0AC).unwrap_or(f32::NAN);
    let mode_result = conn
        .write_raw_for_research(F0MD, &[1])
        .map_err(|e: SmcError| format!("F0md=1: {e}"));
    if let Err(msg) = mode_result {
        let after = conn.read_f32(F0AC).unwrap_or(f32::NAN);
        return ProbeOutcome {
            path: "F0md=1_then_F0Tg",
            write_result: Err(msg),
            actual_rpm_before: before,
            actual_rpm_after: after,
            delta_rpm: after - before,
        };
    }

    let target_result = conn
        .write_raw_for_research(F0TG, &target_rpm.to_be_bytes())
        .map_err(|e: SmcError| format!("F0Tg: {e}"));

    if target_result.is_ok() {
        wait_settled(settle_secs);
    }

    let after = conn.read_f32(F0AC).unwrap_or(f32::NAN);
    let delta = after - before;
    ProbeOutcome {
        path: "F0md=1_then_F0Tg",
        write_result: target_result,
        actual_rpm_before: before,
        actual_rpm_after: after,
        delta_rpm: delta,
    }
}

fn wait_settled(secs: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(250));
    }
}
