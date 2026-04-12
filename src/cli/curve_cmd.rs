//! `fand curve --config <path> [--fan N]` — ASCII curve visualization.
//!
//! FR-050..054: prints a textplots ASCII chart of the configured
//! temperature-to-RPM curve. Does not require root unless the
//! operator wants the live-sensor-reading marker.

use crate::config::load::load_config;
use crate::control::curve;

use std::path::Path;

#[allow(clippy::print_stdout)]
pub fn execute(args: &[String]) {
    let mut config_path = "/etc/fand.toml".to_string();
    let mut fan_filter: Option<u8> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                if i < args.len() {
                    config_path.clone_from(&args[i]);
                } else {
                    eprintln!("fand curve: --config requires a path");
                    std::process::exit(64);
                }
            }
            "--fan" => {
                i += 1;
                if i < args.len() {
                    match crate::cli::parse::parse_fan_index(&args[i]) {
                        Ok(idx) => fan_filter = Some(idx),
                        Err(e) => {
                            eprintln!("fand curve: {e}");
                            std::process::exit(64);
                        }
                    }
                } else {
                    eprintln!("fand curve: --fan requires an index");
                    std::process::exit(64);
                }
            }
            "--help" | "-h" => {
                eprintln!("usage: fand curve [--config PATH] [--fan N]");
                return;
            }
            other => {
                eprintln!("fand curve: unknown option '{other}'");
                std::process::exit(64);
            }
        }
        i += 1;
    }

    // Load config.
    let config = match load_config(Path::new(&config_path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fand curve: {e}");
            std::process::exit(1);
        }
    };

    // Validate.
    let errors = crate::config::validate::validate(&config);
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("fand curve: {e}");
        }
        std::process::exit(1);
    }

    // FR-052: plot all fans if --fan is omitted, or just the specified one.
    let fans_to_plot: Vec<&crate::config::schema::FanBinding> = match fan_filter {
        Some(idx) => match config.fan.iter().find(|f| f.index == idx) {
            Some(f) => vec![f],
            None => {
                eprintln!("fand curve: fan index {idx} not found in config");
                std::process::exit(1);
            }
        },
        None => config.fan.iter().collect(),
    };

    for fan in fans_to_plot {
        print_curve(fan);
    }
}

#[allow(clippy::print_stdout, clippy::cast_precision_loss)]
fn print_curve(fan: &crate::config::schema::FanBinding) {
    let sensors_str: Vec<String> = fan
        .sensors
        .iter()
        .map(|s| match s {
            crate::config::schema::SensorRef::Name(n) => n.clone(),
            crate::config::schema::SensorRef::Smc { smc } => smc.clone(),
        })
        .collect();

    println!(
        "Fan {} — sensors: {} — hysteresis: {:.1}/{:.1}°C",
        fan.index,
        sensors_str.join(", "),
        fan.hysteresis_up,
        fan.hysteresis_down,
    );

    if fan.curve.len() < 2 {
        println!("  (curve has fewer than 2 points — cannot plot)");
        return;
    }

    let t_min = fan.curve[0].0 - 5.0;
    let t_max = fan.curve[fan.curve.len() - 1].0 + 5.0;
    let rpm_max = fan.curve.iter().map(|&(_, r)| r).max().unwrap_or(6550) as f32;

    // Generate 100 interpolation points for the line.
    let interpolated: Vec<(f32, f32)> = (0..=100)
        .map(|i| {
            let t = t_min + (t_max - t_min) * i as f32 / 100.0;
            (t, curve::evaluate(&fan.curve, t))
        })
        .collect();

    // Curve breakpoints as point markers.
    let knots: Vec<(f32, f32)> = fan.curve.iter().map(|&(t, rpm)| (t, rpm as f32)).collect();

    // Use textplots.
    use textplots::{Chart, Plot, Shape};
    Chart::new_with_y_range(120, 40, t_min, t_max, 0.0, rpm_max + 200.0)
        .lineplot(&Shape::Lines(&interpolated))
        .lineplot(&Shape::Points(&knots))
        .display();

    // Print curve points table.
    println!("  Curve points:");
    for &(temp, rpm) in &fan.curve {
        println!("    {temp:>6.1}°C → {rpm:>5} RPM");
    }
    if fan.hysteresis_down > 0.0 {
        println!(
            "  Hysteresis band: cooling requires {:.1}°C additional drop before RPM decreases.",
            fan.hysteresis_down
        );
    }
    println!("  (no SMC access — live sensor marker unavailable)");
    println!();
}
