pub fn execute() {
    eprintln!("usage: fand [subcommand] [options]");
    eprintln!();
    eprintln!("subcommands:");
    eprintln!("  run        Start the fan control daemon");
    eprintln!("  status     Show current thermal and fan state");
    eprintln!("  show       Live curve plot with operating point");
    eprintln!("  set        Temporarily override fan speed");
    eprintln!("  keys       List available fans and sensors");
    eprintln!("  validate   Check a config file for errors");
    eprintln!("  reload     Reload daemon config");
    eprintln!("  version    Print version info");
    eprintln!();
    eprintln!("Without a subcommand, `fand` is equivalent to `fand status`.");
}
