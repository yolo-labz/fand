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
//! Ultrathink bypass extensions (feature 006): also tests F0Mn floor
//! raise, Tp0a sensor spoof, and offers `--enumerate` to dump every SMC
//! key whose attribute byte advertises the writable bit. These surfaces
//! side-step the F0md=1 clamp that pins fan to hardware min.
//!
//! Probe is read-mostly. Every write either succeeds with a visible fan
//! response or fails fast; we never leave the SMC in a non-auto state on
//! exit. The teardown writes `F0md=0` unconditionally, matching the
//! `WriteSession` panic-hook contract.

#![allow(clippy::print_stdout)] // CLI subcommand writes to stdout.

use std::time::{Duration, Instant};

use crate::smc::ffi::{SmcConnection, SmcError};
use crate::smc::keys::ATTR_WRITABLE;

const F0MN: u32 = u32::from_be_bytes(*b"F0Mn");
const F0MX: u32 = u32::from_be_bytes(*b"F0Mx");
const F0AC: u32 = u32::from_be_bytes(*b"F0Ac");
const F0TG: u32 = u32::from_be_bytes(*b"F0Tg");
const F0DC: u32 = u32::from_be_bytes(*b"F0Dc");
const F0MD: u32 = u32::from_be_bytes(*b"F0md");
const TP0A: u32 = u32::from_be_bytes(*b"Tp0a");

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
    let mut enumerate = false;

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
            "--enumerate" => enumerate = true,
            "--help" | "-h" => {
                eprintln!(
                    "usage: fand probe [--settle-secs N] [--target-rpm RPM] [--json] [--enumerate]\n\
                     \n\
                     Runs the full SMC fan-write probe matrix against fan 0:\n\
                       1. F0Tg direct (float RPM)\n\
                       2. F0Dc direct (float duty cycle 0..1)\n\
                       3. F0md=1 then F0Tg (combined forced-min + target)\n\
                       4. F0Mn raise + F0md=1 (forced-min floor bypass)\n\
                       5. Tp0a sensor spoof (fake performance-core temperature)\n\
                     \n\
                     Each path reads F0Ac before and after a settle window.\n\
                     A path passes if the actual RPM deviates by ≥ 500 from\n\
                     baseline. Teardown always writes F0md=0 (auto) and\n\
                     restores any modified floor / sensor value.\n\
                     \n\
                     --enumerate dumps every SMC key whose attribute byte\n\
                     advertises the writable bit (0x40). Useful for finding\n\
                     unknown control surfaces on newer SoC revisions."
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

    if enumerate {
        run_enumerate(&mut conn, json);
        return;
    }

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
    let _ = conn.write_raw_for_research(F0MD, &[0]);
    std::thread::sleep(Duration::from_secs(2));

    outcomes.push(probe_floor_raise(
        &mut conn,
        min_rpm,
        effective_target,
        settle_secs,
    ));
    // Floor teardown happens inside probe_floor_raise; mode reset below.
    let _ = conn.write_raw_for_research(F0MD, &[0]);
    std::thread::sleep(Duration::from_secs(2));

    outcomes.push(probe_sensor_spoof(&mut conn, settle_secs));

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
            "{:<28} {:>8} {:>8} {:>8} {:>7} {}",
            "path", "before", "after", "delta", "passed", "error"
        );
        println!("{:-<78}", "");
        for o in &outcomes {
            let err = match &o.write_result {
                Ok(()) => String::new(),
                Err(s) => s.clone(),
            };
            println!(
                "{:<28} {:>8.0} {:>8.0} {:>8.0} {:>7} {}",
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
                "no direct path moved the fan — SMC surface remains F0md=0/1 only on this SoC; try `fand probe --enumerate` to dump the writable keyspace"
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

/// Raise the SMC's declared fan floor (`F0Mn`) to the effective target,
/// then engage `F0md=1`. If the firmware honors the new floor, the fan
/// spools to the raised minimum even though we never touched `F0Tg`.
/// Teardown restores the original `F0Mn` value so we never leave the
/// platform with a falsely elevated minimum across the next boot.
fn probe_floor_raise(
    conn: &mut SmcConnection,
    original_min: f32,
    target_rpm: f32,
    settle_secs: u64,
) -> ProbeOutcome {
    let before = conn.read_f32(F0AC).unwrap_or(f32::NAN);

    let raise_result = conn
        .write_raw_for_research(F0MN, &target_rpm.to_be_bytes())
        .map_err(|e: SmcError| format!("F0Mn raise: {e}"));
    if let Err(msg) = raise_result {
        let after = conn.read_f32(F0AC).unwrap_or(f32::NAN);
        return ProbeOutcome {
            path: "F0Mn_raise_then_F0md=1",
            write_result: Err(msg),
            actual_rpm_before: before,
            actual_rpm_after: after,
            delta_rpm: after - before,
        };
    }

    let mode_result = conn
        .write_raw_for_research(F0MD, &[1])
        .map_err(|e: SmcError| format!("F0md=1: {e}"));
    let combined = mode_result;

    if combined.is_ok() {
        wait_settled(settle_secs);
    }

    let after = conn.read_f32(F0AC).unwrap_or(f32::NAN);

    // Restore the original floor regardless of pass/fail.
    let _ = conn.write_raw_for_research(F0MN, &original_min.to_be_bytes());

    let delta = after - before;
    ProbeOutcome {
        path: "F0Mn_raise_then_F0md=1",
        write_result: combined,
        actual_rpm_before: before,
        actual_rpm_after: after,
        delta_rpm: delta,
    }
}

/// Spoof the performance-core temperature sensor (`Tp0a`, flt °C) to a
/// high value so `thermalmonitord` reacts by spinning the fan via its own
/// policy. If the sensor key is user-writable at all, this is the path
/// that bypasses the SMC target clamp entirely — we ask the OS to spin
/// the fan for us instead of fighting the firmware's fan controller.
/// Teardown writes back the observed baseline temp; on failure we accept
/// a brief restoration window rather than leaving a fake value in place.
fn probe_sensor_spoof(conn: &mut SmcConnection, settle_secs: u64) -> ProbeOutcome {
    let before_rpm = conn.read_f32(F0AC).unwrap_or(f32::NAN);
    let baseline_temp = conn.read_f32(TP0A).unwrap_or(45.0);
    let fake_temp: f32 = 99.0;

    let write_result = conn
        .write_raw_for_research(TP0A, &fake_temp.to_be_bytes())
        .map_err(|e: SmcError| format!("Tp0a spoof: {e}"));

    if write_result.is_ok() {
        wait_settled(settle_secs);
    }

    let after_rpm = conn.read_f32(F0AC).unwrap_or(f32::NAN);

    // Restore real temperature best-effort.
    let _ = conn.write_raw_for_research(TP0A, &baseline_temp.to_be_bytes());

    ProbeOutcome {
        path: "Tp0a_spoof",
        write_result,
        actual_rpm_before: before_rpm,
        actual_rpm_after: after_rpm,
        delta_rpm: after_rpm - before_rpm,
    }
}

/// Walk the full SMC keyspace via `kSMCGetKeyFromIdx` and print every key
/// whose `data_attributes` byte advertises `ATTR_WRITABLE` (0x40). This
/// exposes unknown control surfaces without needing to write anything.
fn run_enumerate(conn: &mut SmcConnection, json: bool) {
    let total = match conn.read_u32(u32::from_be_bytes(*b"#KEY")) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("fand probe --enumerate: read #KEY failed: {e}");
            std::process::exit(1);
        }
    };

    let total = total.min(8000);
    let mut writable: Vec<(u32, u32, u32, u8)> = Vec::new();

    for idx in 0..total {
        let fourcc = match conn.read_key_at_index(idx) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let (info, attrs) = match conn.read_key_info_full(fourcc) {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        if attrs & ATTR_WRITABLE != 0 {
            writable.push((fourcc, info.data_size, info.data_type, attrs));
        }
    }

    if json {
        print!("{{\"total_keys\":{total},\"writable\":[");
        for (i, (fcc, size, ty, attrs)) in writable.iter().enumerate() {
            if i > 0 {
                print!(",");
            }
            print!(
                "{{\"fourcc\":\"{}\",\"size\":{size},\"type\":\"{}\",\"attrs\":\"0x{attrs:02X}\"}}",
                fourcc_str(*fcc),
                fourcc_str(*ty),
            );
        }
        println!("]}}");
    } else {
        println!(
            "fand probe --enumerate — {} total keys, {} writable",
            total,
            writable.len()
        );
        println!("{:-<40}", "");
        println!("{:<8} {:>5} {:<6} {}", "fourcc", "size", "type", "attrs");
        println!("{:-<40}", "");
        for (fcc, size, ty, attrs) in &writable {
            println!(
                "{:<8} {:>5} {:<6} 0x{:02X}",
                fourcc_str(*fcc),
                size,
                fourcc_str(*ty),
                attrs
            );
        }
        println!("{:-<40}", "");
    }
}

fn fourcc_str(fourcc: u32) -> String {
    let bytes = [
        ((fourcc >> 24) & 0xFF) as u8,
        ((fourcc >> 16) & 0xFF) as u8,
        ((fourcc >> 8) & 0xFF) as u8,
        (fourcc & 0xFF) as u8,
    ];
    bytes
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                char::from(b)
            } else {
                '?'
            }
        })
        .collect()
}

fn wait_settled(secs: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(250));
    }
}
