#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

mod cli;
mod config;
mod control;
mod correlation;
mod iohid;
mod ipc;
mod launchd;
mod log;
mod security;
mod smc;

fn main() {
    // FR-061: scrub environment variables via the allowlist policy BEFORE
    // any other initialization. Defeats DYLD_* injection and Malloc* tricks
    // inherited across the sudo boundary.
    security::scrub_env();

    let args: Vec<String> = std::env::args().collect();
    let subcommand = args.get(1).map(String::as_str);

    match subcommand {
        Some("run") => cli::run::execute(&args[2..]),
        Some("status") => cli::status::execute(&args[2..]),
        Some("show") => cli::show::execute(&args[2..]),
        Some("set") => cli::set::execute(&args[2..]),
        Some("selftest") => cli::selftest::execute(&args[2..]),
        Some("curve") => cli::curve_cmd::execute(&args[2..]),
        Some("keys") => cli::keys::execute(&args[2..]),
        Some("validate") => cli::validate::execute(&args[2..]),
        Some("reload") => cli::reload::execute(&args[2..]),
        Some("version") | Some("--version") | Some("-V") => cli::version::execute(),
        Some("--help") | Some("-h") => cli::help::execute(),
        None => cli::status::execute(&[]),
        Some(unknown) => {
            eprintln!("fand: unknown subcommand '{unknown}'");
            eprintln!("usage: fand [run|curve|status|show|set|keys|validate|reload|version]");
            std::process::exit(64);
        }
    }
}
