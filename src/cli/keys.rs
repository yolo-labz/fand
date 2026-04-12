//! `fand keys` subcommand — the byte-perfect safety gate (T033-T036).
//!
//! Default mode: enumerate fans, print a human-readable catalog, exit 0.
//! `--debug-open`: open the SMC connection, print the result, exit.
//! `--json`: emit the catalog as machine-readable JSON with `schema_version: 1`.

// CLI subcommands legitimately write to stdout as their primary output.
// The `print_stdout = "deny"` workspace lint is intended for library code.
#![allow(clippy::print_stdout)]

use std::process::Command;

use crate::smc::enumerate::{Fan, enumerate_fans};
use crate::smc::ffi::{SmcConnection, SmcError};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Default,
    DebugOpen,
    Json,
    /// `--all` (feature 005 Phase 4 T040, early-landed as RD-08 unblocker):
    /// enumerate every SMC key starting with `F` and print fourcc + data_type
    /// + data_size. Used to identify the writable fan target key on SoCs
    /// where the Intel-convention `F<i>Tg` is an alias.
    AllFanKeys,
    /// `--read <fourcc>`: raw diagnostic read of a single SMC key by fourcc
    /// (4-character string). Used for RD-08 keyspace research to inspect
    /// F0S0..F0S7 step presets, F0Dc duty cycle, and other candidates.
    Read(String),
    /// `--write <fourcc> <type>:<value>`: raw diagnostic write of a single
    /// SMC key, used for RD-08 reverse-engineering experiments. The type
    /// prefix is one of `u8`, `u32`, `f32`, `hex` (raw byte string).
    /// Examples:
    ///   --write F0md u8:1
    ///   --write F0Dc f32:0.5
    ///   --write F0St u8:3
    /// **DEBUG ONLY** — bypasses the WritableKey whitelist. Used by the
    /// project maintainer for keyspace research, NOT for end-user fan control.
    DebugWrite { fourcc: String, value: String },
}

pub fn execute(args: &[String]) {
    let mode = match parse_args(args) {
        Ok(m) => m,
        Err(msg) => {
            eprintln!("fand keys: {msg}");
            eprintln!("usage: fand keys [--debug-open | --json | --all]");
            std::process::exit(64);
        }
    };

    let exit_code = match &mode {
        Mode::Default => run_default(),
        Mode::DebugOpen => run_debug_open(),
        Mode::Json => run_json(),
        Mode::AllFanKeys => run_all_fan_keys(),
        Mode::Read(fourcc) => run_read_key(fourcc),
        Mode::DebugWrite { fourcc, value } => run_debug_write(fourcc, value),
    };
    std::process::exit(i32::from(exit_code));
}

fn parse_args(args: &[String]) -> Result<Mode, String> {
    let mut mode = Mode::Default;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--all" => {
                if mode != Mode::Default {
                    return Err("--all is mutually exclusive with other modes".into());
                }
                mode = Mode::AllFanKeys;
            }
            "--read" => {
                if mode != Mode::Default {
                    return Err("--read is mutually exclusive with other modes".into());
                }
                i += 1;
                let v = args.get(i).ok_or_else(|| "--read requires a 4-character fourcc".to_string())?;
                if v.as_bytes().len() != 4 {
                    return Err(format!("--read fourcc must be exactly 4 bytes, got {}", v.len()));
                }
                mode = Mode::Read(v.clone());
            }
            "--write" => {
                if mode != Mode::Default {
                    return Err("--write is mutually exclusive with other modes".into());
                }
                i += 1;
                let fourcc = args
                    .get(i)
                    .ok_or_else(|| "--write requires <fourcc> <type:value>".to_string())?
                    .clone();
                if fourcc.as_bytes().len() != 4 {
                    return Err(format!("--write fourcc must be 4 bytes, got {}", fourcc.len()));
                }
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "--write requires a type:value argument".to_string())?
                    .clone();
                mode = Mode::DebugWrite { fourcc, value };
            }
            "--debug-open" => {
                if mode != Mode::Default {
                    return Err("--debug-open is mutually exclusive".into());
                }
                mode = Mode::DebugOpen;
            }
            "--json" => {
                if mode != Mode::Default {
                    return Err("--json is mutually exclusive".into());
                }
                mode = Mode::Json;
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(mode)
}

// ------------------------------------------------------------------------
// Default mode
// ------------------------------------------------------------------------

fn run_default() -> u8 {
    let mut conn = match open_with_diagnostic() {
        Ok(c) => c,
        Err(code) => return code,
    };

    // FR-020: write Ftst=0 as a whitelist probe (no-op, exercises the write
    // path under the WritableKey boundary). If the probe fails we still
    // try to enumerate — the probe is diagnostic, not blocking.
    let _ = conn.probe_write_ftst_zero();

    let fans = match enumerate_fans(&mut conn) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("fand keys: fan enumeration failed: {e}");
            return 1;
        }
    };

    print_header();
    print_catalog(&fans);

    // Second Ftst=0 write before exit (FR-020).
    let _ = conn.probe_write_ftst_zero();

    drop(conn);
    0
}

fn run_debug_open() -> u8 {
    match SmcConnection::open() {
        Ok(conn) => {
            println!("fand keys --debug-open: IOServiceOpen success");
            drop(conn);
            0
        }
        Err(SmcError::OpenFailed(kr)) => {
            eprintln!("fand keys --debug-open: IOServiceOpen failed: {kr:#X}");
            1
        }
        Err(e) => {
            eprintln!("fand keys --debug-open: {e}");
            if matches!(e, SmcError::OpenFailed(_)) { 1 } else { 2 }
        }
    }
}

/// `fand keys --all` (feature 005 Phase 4 T040, early-landed 2026-04-11 for
/// RD-08 unblocking): iterate every SMC key index `0..#KEY` and print each
/// key's fourcc + data_type + data_size. Filters to `F`-prefixed keys by
/// default because the immediate question is which fan key is the writable
/// target on Apple Silicon.
fn run_all_fan_keys() -> u8 {
    let mut conn = match open_with_diagnostic() {
        Ok(c) => c,
        Err(code) => return code,
    };

    let key_count = match crate::smc::enumerate::read_key_count(&mut conn) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("fand keys --all: failed to read #KEY: {e}");
            return 1;
        }
    };

    println!("fand keys --all — {key_count} keys in SMC keyspace");
    println!("filtering to F-prefixed keys (fan-related)");
    println!();
    println!("  {:<8} {:<8} {:<10} {:<12} {}", "idx", "fourcc", "data_type", "data_size", "attributes");

    let mut f_count = 0u32;
    let mut errors = 0u32;

    for i in 0..key_count {
        // Read the fourcc at this index via kSMCGetKeyFromIdx.
        let fourcc = match conn.read_key_at_index(i) {
            Ok(f) => f,
            Err(_) => {
                errors = errors.saturating_add(1);
                continue;
            }
        };

        // Filter: first byte must be 'F' (fan-related).
        let first_byte = ((fourcc >> 24) & 0xFF) as u8;
        if first_byte != b'F' {
            continue;
        }
        f_count = f_count.saturating_add(1);

        // Fetch the key info (data_size + data_type).
        let info = match conn.read_key_info(fourcc) {
            Ok(i) => i,
            Err(_) => {
                println!(
                    "  {:<8} {:<8} (read_key_info failed)",
                    i,
                    fourcc_to_string(fourcc)
                );
                continue;
            }
        };

        println!(
            "  {:<8} {:<8} {:<10} {:<12} {}",
            i,
            fourcc_to_string(fourcc),
            fourcc_to_string(info.data_type),
            info.data_size,
            format_attributes_placeholder()
        );
    }

    println!();
    println!("matched: {f_count} F-prefixed keys");
    if errors > 0 {
        println!("errors:  {errors} indices failed to read");
    }
    0
}

/// Placeholder — feature 004's `read_key_info` does not expose the
/// attribute byte yet. Phase 4 T043 is the full attribute-filter task.
fn format_attributes_placeholder() -> &'static str {
    "(unknown)"
}

/// `fand keys --write <fourcc> <type:value>` — DEBUG-ONLY raw write that
/// bypasses the `WritableKey` whitelist. Used by RD-08 research to find
/// the actual writable fan-control key on Apple Silicon M-series.
///
/// Type prefixes: `u8`, `u32`, `f32`, `hex` (raw byte string, hex-encoded).
fn run_debug_write(fourcc_str: &str, value_str: &str) -> u8 {
    let bytes = fourcc_str.as_bytes();
    if bytes.len() != 4 {
        eprintln!("fand keys --write: fourcc must be exactly 4 bytes");
        return 64;
    }
    let fourcc = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);

    // Parse the type:value form.
    let (type_prefix, raw_value) = match value_str.split_once(':') {
        Some(p) => p,
        None => {
            eprintln!("fand keys --write: value must be type:value (e.g. u8:1, f32:0.5)");
            return 64;
        }
    };

    let payload: Vec<u8> = match type_prefix {
        "u8" => match raw_value.parse::<u8>() {
            Ok(v) => vec![v],
            Err(e) => {
                eprintln!("fand keys --write: u8 parse failed: {e}");
                return 64;
            }
        },
        "u32" => match raw_value.parse::<u32>() {
            Ok(v) => v.to_be_bytes().to_vec(),
            Err(e) => {
                eprintln!("fand keys --write: u32 parse failed: {e}");
                return 64;
            }
        },
        "f32" => match raw_value.parse::<f32>() {
            Ok(v) => v.to_le_bytes().to_vec(),
            Err(e) => {
                eprintln!("fand keys --write: f32 parse failed: {e}");
                return 64;
            }
        },
        "hex" => {
            let cleaned: String =
                raw_value.chars().filter(|c| !c.is_ascii_whitespace()).collect();
            if cleaned.len() % 2 != 0 {
                eprintln!("fand keys --write: hex must have even number of chars");
                return 64;
            }
            let mut out = Vec::with_capacity(cleaned.len() / 2);
            let chars: Vec<char> = cleaned.chars().collect();
            for chunk in chars.chunks(2) {
                let s: String = chunk.iter().collect();
                match u8::from_str_radix(&s, 16) {
                    Ok(b) => out.push(b),
                    Err(e) => {
                        eprintln!("fand keys --write: hex parse failed: {e}");
                        return 64;
                    }
                }
            }
            out
        }
        other => {
            eprintln!("fand keys --write: unknown type prefix '{other}' (use u8|u32|f32|hex)");
            return 64;
        }
    };

    let mut conn = match open_with_diagnostic() {
        Ok(c) => c,
        Err(code) => return code,
    };

    // Read the key info first so we can confirm size compatibility before write.
    let info = match conn.read_key_info(fourcc) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("fand keys --write {fourcc_str}: read_key_info failed: {e}");
            return 1;
        }
    };
    println!(
        "fand keys --write {fourcc_str}: type={} size={} payload={:02X?}",
        fourcc_to_string(info.data_type),
        info.data_size,
        payload
    );

    if payload.len() != info.data_size as usize {
        eprintln!(
            "fand keys --write {fourcc_str}: payload size {} != SMC data_size {}",
            payload.len(),
            info.data_size
        );
        return 64;
    }

    match conn.write_raw_for_research(fourcc, &payload) {
        Ok(()) => {
            println!("  write succeeded (kIOReturnSuccess + result byte 0x00)");
            // Read back so the operator can see whether the write was sticky.
            let readback = conn.read_key(fourcc);
            match readback {
                Ok((_info, raw)) => {
                    let valid_len = (info.data_size as usize).min(raw.len());
                    println!("  read-back: {:02X?}", &raw[..valid_len]);
                    let matches = raw[..valid_len] == payload[..];
                    if matches {
                        println!("  STATUS: read-back matches written value (write took effect at register level)");
                    } else {
                        println!("  STATUS: read-back DIFFERS from written value (write was overridden or aliased)");
                    }
                }
                Err(e) => println!("  read-back failed: {e}"),
            }
            0
        }
        Err(e) => {
            eprintln!("fand keys --write {fourcc_str}: write failed: {e}");
            1
        }
    }
}

/// `fand keys --read <fourcc>` — read a single SMC key by fourcc and print
/// raw bytes + typed interpretations. Used for RD-08 keyspace research to
/// inspect candidate writable fan control keys.
fn run_read_key(fourcc_str: &str) -> u8 {
    let bytes = fourcc_str.as_bytes();
    if bytes.len() != 4 {
        eprintln!("fand keys --read: fourcc must be exactly 4 bytes");
        return 64;
    }
    let fourcc = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);

    let mut conn = match open_with_diagnostic() {
        Ok(c) => c,
        Err(code) => return code,
    };

    // Fetch key info first to know the data type + size.
    let info = match conn.read_key_info(fourcc) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("fand keys --read {fourcc_str}: read_key_info failed: {e}");
            return 1;
        }
    };

    println!(
        "fand keys --read {fourcc_str}: type={} size={}",
        fourcc_to_string(info.data_type),
        info.data_size
    );

    // Print typed interpretations based on data_type.
    let type_str = fourcc_to_string(info.data_type);
    match (type_str.as_str(), info.data_size) {
        ("ui8 ", 1) => match conn.read_u8(fourcc) {
            Ok(v) => println!("  ui8  = {v} (0x{v:02X})"),
            Err(e) => println!("  ui8  read failed: {e}"),
        },
        ("ui32", 4) => match conn.read_u32(fourcc) {
            Ok(v) => println!("  ui32 = {v} (0x{v:08X})"),
            Err(e) => println!("  ui32 read failed: {e}"),
        },
        ("flt ", 4) => match conn.read_f32(fourcc) {
            Ok(v) => println!("  flt  = {v}"),
            Err(e) => println!("  flt  read failed: {e}"),
        },
        _ => {
            // For unknown/hex_ types, read via read_key which returns 32 raw bytes.
            match conn.read_key(fourcc) {
                Ok((_info, raw)) => {
                    let valid_len = (info.data_size as usize).min(raw.len());
                    println!(
                        "  raw  = {:02X?}",
                        &raw[..valid_len]
                    );
                    // Also attempt decoding as u16/u32 from both endiannesses.
                    if valid_len == 2 {
                        let be = u16::from_be_bytes([raw[0], raw[1]]);
                        let le = u16::from_le_bytes([raw[0], raw[1]]);
                        println!("  u16  BE={be} LE={le}");
                    } else if valid_len == 4 {
                        let be = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
                        let le = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                        println!("  u32  BE={be} LE={le}");
                        let fle = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                        println!("  f32  LE={fle}");
                    }
                }
                Err(e) => println!("  read_key failed: {e}"),
            }
        }
    }
    0
}

fn run_json() -> u8 {
    let mut conn = match open_with_diagnostic() {
        Ok(c) => c,
        Err(code) => return code,
    };

    let fans = match enumerate_fans(&mut conn) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("fand keys: fan enumeration failed: {e}");
            return 1;
        }
    };

    let macos = system_string("sw_vers", &["-productVersion"]).unwrap_or_default();
    let model = sysctl_string("hw.model").unwrap_or_default();
    let arch = system_string("uname", &["-m"]).unwrap_or_default();

    // Hand-rolled JSON. We avoid pulling serde into the CLI hot path.
    print!("{{");
    print!(r#""schema_version":1,"#);
    print!(r#""fand_version":"{}","#, env!("CARGO_PKG_VERSION"));
    print!(r#""macos":"{}","#, json_escape(macos.trim()));
    print!(r#""model":"{}","#, json_escape(model.trim()));
    print!(r#""arch":"{}","#, json_escape(arch.trim()));
    print!(r#""fans":["#);
    for (i, f) in fans.iter().enumerate() {
        if i > 0 {
            print!(",");
        }
        print!(
            r#"{{"index":{},"min_rpm":{:.1},"max_rpm":{:.1},"safe_rpm":{},"actual_rpm":{:.1},"mode_key":"{}"}}"#,
            f.index,
            f.min_rpm,
            f.max_rpm,
            match f.safe_rpm {
                Some(v) => format!("{v:.1}"),
                None => "null".into(),
            },
            f.actual_rpm,
            fourcc_to_string(f.mode_key),
        );
    }
    println!("]}}");

    drop(conn);
    0
}

// ------------------------------------------------------------------------
// Shared helpers
// ------------------------------------------------------------------------

/// Open an SMC connection with a full diagnostic on failure. Returns either
/// the open connection or the exit code the caller should propagate.
fn open_with_diagnostic() -> Result<SmcConnection, u8> {
    match SmcConnection::open() {
        Ok(c) => Ok(c),
        Err(e @ SmcError::ServiceNotFound) => {
            eprintln!("fand keys: {e}");
            Err(1)
        }
        Err(SmcError::OpenFailed(kr)) => {
            eprintln!("fand keys: IOServiceOpen failed: {kr:#X}");
            eprintln!("  hint: run as root (sudo fand keys)");
            Err(2)
        }
        Err(e) => {
            eprintln!("fand keys: {e}");
            Err(1)
        }
    }
}

fn print_header() {
    let version = env!("CARGO_PKG_VERSION");
    let macos = system_string("sw_vers", &["-productVersion"]).unwrap_or_default();
    let model = sysctl_string("hw.model").unwrap_or_default();
    let arch = system_string("uname", &["-m"]).unwrap_or_default();

    println!("fand {version} — SMC fan catalog");
    println!(
        "  host: {} ({}) macOS {}",
        model.trim(),
        arch.trim(),
        macos.trim()
    );
}

fn print_catalog(fans: &[Fan]) {
    if fans.is_empty() {
        println!("  no fans reported (fanless chassis)");
        return;
    }
    println!(
        "  {:<5} {:>8} {:>8} {:>8} {:>8}  {}",
        "idx", "min_rpm", "max_rpm", "safe", "actual", "mode_key"
    );
    for f in fans {
        println!(
            "  {:<5} {:>8.1} {:>8.1} {:>8} {:>8.1}  {}",
            f.index,
            f.min_rpm,
            f.max_rpm,
            match f.safe_rpm {
                Some(v) => format!("{v:.1}"),
                None => "—".into(),
            },
            f.actual_rpm,
            fourcc_to_string(f.mode_key),
        );
    }
}

fn fourcc_to_string(fourcc: u32) -> String {
    let bytes = fourcc.to_be_bytes();
    bytes
        .iter()
        .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
        .collect()
}

fn json_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            '\n' => vec!['\\', 'n'],
            '\r' => vec!['\\', 'r'],
            '\t' => vec!['\\', 't'],
            c if (c as u32) < 0x20 => format!("\\u{:04x}", c as u32).chars().collect(),
            c => vec![c],
        })
        .collect()
}

fn system_string(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

fn sysctl_string(key: &str) -> Option<String> {
    let out = Command::new("sysctl").args(["-n", key]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

