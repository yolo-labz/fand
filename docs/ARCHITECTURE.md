# Architecture

## Project layout (planned)

```
fand/
  Cargo.toml
  rust-toolchain.toml
  src/
    main.rs        # arg parsing, config load, signal handling
    smc.rs         # IOKit FFI, SMCParamStruct, read/write key
    sensors.rs     # Tp0x enumeration via #KEY/#NUM
    curve.rs       # linear interp + hysteresis
    daemon.rs      # 500ms control loop
  nix/
    package.nix    # rustPlatform.buildRustPackage + ad-hoc sign
    module.nix     # nix-darwin services.fand options
  docs/
    RESEARCH.md
    ARCHITECTURE.md
    SMC-PROTOCOL.md
  flake.nix
  README.md
  LICENSE
```

Estimated MVP: **600–900 LOC Rust** + ~50 LOC Nix.

## Control loop

```
loop {
    for fan in fans {
        let temps = sensors.iter().map(|k| smc.read_flt(k)).collect();
        let driver_temp = temps.into_iter().reduce(f32::max).unwrap();
        let target_rpm = curve.evaluate(driver_temp);
        let clamped = target_rpm.clamp(fan.min_rpm, fan.max_rpm);
        smc.write_u8(fan.mode_key, 1);          // force mode every tick
        smc.write_flt(fan.target_key, clamped); // target RPM every tick
    }
    sleep(500ms);
}
```

- Single thread, no async runtime
- Both `FxMd=1` and `FxTg` written every tick to out-write powerd
- `driver_temp = max(sensors)` — hottest-of strategy
- Hysteresis applied inside `curve.evaluate()` to prevent RPM thrash
  when temp oscillates near a curve point

## Config schema

`/etc/fand.toml`:

```toml
# Global tuning
poll_interval_ms = 500
log_level = "info"          # error|warn|info|debug|trace

# Per-fan curves. Index matches FNum enumeration order.
[[fan]]
index = 0
sensors = ["Tp01", "Tp05", "Tp09"]   # hottest-of these p-core dies
hysteresis_c = 3.0
# (temp_c, rpm) breakpoints — linearly interpolated between
curve = [
  [50.0, 0],       # below 50°C: minimum RPM (clamped to FxMn)
  [65.0, 2500],
  [80.0, 6000],   # at/above 80°C: maximum RPM (clamped to FxMx)
]

[[fan]]
index = 1
sensors = ["Tp01", "Tp05", "Tp09"]
hysteresis_c = 3.0
curve = [[50.0, 0], [65.0, 2500], [80.0, 6000]]
```

Curve evaluation:
- Below first breakpoint → first RPM
- Above last breakpoint → last RPM
- Between → linear interpolation
- After interpolation → clamp to `[FxMn, FxMx]` read from SMC at startup
- Hysteresis: ignore changes smaller than `hysteresis_c` °C since last
  setpoint update

## Signal handling

- `SIGTERM` / `SIGINT` → write `FxMd=0` (auto) to all fans, exit cleanly
- `SIGHUP` → reload `/etc/fand.toml` without dropping fan control
- Panic / crash → launchd `KeepAlive=true` restarts; SMC will fall back
  to `Md=0` when our writes stop arriving (powerd takes over within ~2s)

## Logging

`tracing` to stderr; launchd captures to `/var/log/fand.err`.

Per-tick logging is too noisy. Log:
- Startup: detected fan count, min/max RPM per fan, sensor type per key
- Curve transitions: when target RPM changes by more than 200 RPM
- Errors: any IOKit return code != `kIOReturnSuccess`

## Nix derivation shape

```nix
# nix/package.nix
{ lib, rustPlatform, darwin }:
rustPlatform.buildRustPackage {
  pname = "fand";
  version = "0.1.0";
  src = ../.;
  cargoLock.lockFile = ../Cargo.lock;
  buildInputs = [
    darwin.apple_sdk.frameworks.IOKit
    darwin.apple_sdk.frameworks.CoreFoundation
  ];
  postFixup = ''
    /usr/bin/codesign -f -s - $out/bin/fand
  '';
  meta = with lib; {
    description = "Apple Silicon fan control daemon";
    platforms = platforms.darwin;
    license = licenses.mit;  # TBD
  };
}
```

## nix-darwin module shape

```nix
# nix/module.nix
{ config, lib, pkgs, ... }:
let
  cfg = config.services.fand;
  tomlFormat = pkgs.formats.toml { };
in {
  options.services.fand = {
    enable = lib.mkEnableOption "fand Apple Silicon fan daemon";
    package = lib.mkPackageOption pkgs "fand" { };
    settings = lib.mkOption {
      type = tomlFormat.type;
      default = { };
      description = "Contents of /etc/fand.toml";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.etc."fand.toml".source =
      tomlFormat.generate "fand.toml" cfg.settings;

    launchd.daemons.fand = {
      command = "${cfg.package}/bin/fand --config /etc/fand.toml";
      serviceConfig = {
        RunAtLoad = true;
        KeepAlive = true;
        StandardErrorPath = "/var/log/fand.err";
        StandardOutPath   = "/var/log/fand.out";
        ProcessType       = "Background";
      };
    };
  };
}
```

## Build / iteration plan

1. **Phase 0 — FFI scaffolding**: Cargo project, `smc.rs` with
   `IOServiceOpen` + struct definitions. Compile, no behavior yet.
2. **Phase 1 — Read-only**: implement `read_key` for `flt`, `ui8`,
   `ui32`. Validate against `sudo powermetrics --samplers smc -n 1` and
   `stats` app readings on the same machine. **Do not enable writes
   until reads are byte-perfect.**
3. **Phase 2 — Enumeration**: read `FNum`, `#KEY`, iterate `Tp0*` and
   `F0*`/`F1*` keys. Print a startup report.
4. **Phase 3 — Writes**: ✅ **GATE PASSED 2026-04-11 on Mac17,2** (feature 005). The Apple Silicon SMC interface exposes only `F0md=0` (auto) and `F0md=1` (forced minimum) — arbitrary RPM targets via `F0Tg`/`F0Dc`/`F0St` are read-only or refused per RD-08. `sudo fand set --fan 0 --rpm 2317 --commit` engages forced-minimum and the fan physically drops from ~4700 → ~2316 RPM within 5 seconds; `pkill -TERM` triggers the signal-thread teardown which writes `F0md=0` and the fan recovers to system-controlled RPM. `sudo fand selftest --iterations 5` runs 10 round trips with 0 mismatches and a 1500+ RPM delta. See `specs/005-smc-write-roundtrip/verification-20260411.md` for the full evidence and `specs/005-smc-write-roundtrip/research.md` RD-08 for the SoC-specific findings.
5. **Phase 4 — Control loop**: TOML config, curve evaluation, 500ms
   loop, signal handling.
6. **Phase 5 — Nix packaging**: derivation + nix-darwin module, run as
   root LaunchDaemon on `macbook-pro`.
7. **Phase 6 — Extraction**: move to `pedrohbalbino/fand` GitHub repo,
   add as flake input, drop staging copy from `Apple/fand/`.

## Safety rails

- **Phase 3 must round-trip identical values before any curve logic.**
  A wrong type encoding could drive a fan to 0 RPM during heavy load.
- Always clamp final write to `[FxMn, FxMx]` read from SMC at startup
  (defends against curve config errors AND M2 Pro `Tg < Mn` rejection).
- On any IOKit error during a write, immediately set `FxMd=0` (release
  to powerd) and exit with non-zero so launchd respawns into a clean
  state.
- Have a hard `--dry-run` flag that does everything except `WriteKey`
  calls, used during all testing until Phase 4.
