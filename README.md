# fand — Apple Silicon Fan Control Daemon

A FOSS CLI fan controller for Apple Silicon MacBooks (M1–M5). Manages fan speed via temperature-to-RPM curves with hysteresis, rate limiting, and bumpless transfer. Designed to run as a root LaunchDaemon managed declaratively via nix-darwin.

> **Important**: On Apple Silicon M-series, the SMC only accepts `F0md=0` (auto) and `F0md=1` (forced minimum). Arbitrary RPM targets are read-only. The daemon's curve output maps to a binary decision: if the curve says RPM near minimum → forced minimum; otherwise → auto (thermalmonitord manages). See [RD-08](docs/SMC-PROTOCOL.md) for details.

## Installation

### Via nix-darwin (recommended)

Add fand as a flake input and enable the module:

```nix
# flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-25.05-darwin";
    nix-darwin.url = "github:LnL7/nix-darwin";
    fand.url = "github:yolo-labz/fand";
  };

  outputs = { self, nixpkgs, nix-darwin, fand, ... }: {
    darwinConfigurations.macbook-pro = nix-darwin.lib.darwinSystem {
      system = "aarch64-darwin";
      modules = [
        fand.darwinModules.default
        {
          services.fand = {
            enable = true;
            settings = {
              config_version = 1;
              poll_interval_ms = 500;
              log_level = "info";
              fan = [{
                index = 0;
                sensors = [{ smc = "Tf04"; } { smc = "Tf09"; } { smc = "Tf0D"; }];
                hysteresis_up = 1.0;
                hysteresis_down = 2.0;
                smoothing_alpha = 0.25;
                ramp_down_rpm_per_s = 600;
                panic_temp_c = 95.0;
                panic_hold_s = 10;
                curve = [
                  [50 2317]   # below 50C: hardware minimum
                  [65 3500]   # moderate load
                  [75 5000]   # heavy load
                  [85 6550]   # thermal emergency: hardware maximum
                ];
              }];
            };
          };
        }
      ];
    };
  };
}
```

Then `darwin-rebuild switch` deploys the daemon.

### Manual build

```bash
cargo build --release
./scripts/sign-release.sh  # optional: ad-hoc codesign with hardened runtime
sudo cp target/release/fand /usr/local/bin/
```

### Via nix build

```bash
nix build github:yolo-labz/fand
result/bin/fand --version
```

## Configuration

fand reads `/etc/fand.toml` (or the path given by `--config`). The schema:

```toml
config_version = 1
poll_interval_ms = 500     # tick interval: 100-5000 ms
log_level = "info"         # error | warn | info | debug

[[fan]]
index = 0                  # SMC fan index (0-based)
sensors = [{smc = "Tf04"}, {smc = "Tf09"}]  # temperature sensor fourcc keys
hysteresis_up = 1.0        # heating threshold (C)
hysteresis_down = 2.0      # cooling threshold (C)
smoothing_alpha = 0.25     # EMA smoothing factor
ramp_down_rpm_per_s = 600  # max RPM decrease per second
panic_temp_c = 95.0        # emergency: ramp to max above this
panic_hold_s = 10          # hold max RPM for this long after panic
curve = [                  # [temperature_C, RPM] breakpoints
  [50.0, 2317],
  [65.0, 3500],
  [75.0, 5000],
  [85.0, 6550],
]
```

## Discovering Sensors

Sensor key names differ between Apple Silicon generations:

| Generation | P-core prefix | E-core prefix | Example |
|-----------|--------------|--------------|---------|
| M1 | `Tp0*` | `Tp0*` | `Tp01`, `Tp05` |
| M2 | `Tp0*` | `Tp1*` | `Tp01`, `Tp1h` |
| M3 | **`Tf0*`/`Tf4*`** | `Te0*` | `Tf04`, `Te05` |
| M4 | `Tp0*` | `Te0*` | `Tp01`, `Te05` |
| M5 | `Tp0*` | `Tp0*` | `Tp0O`, `Tp0R` |

Find your machine's sensors:

```bash
sudo fand keys --all | grep '^T'     # list all temperature keys
sudo fand keys --read Tf04           # read a specific sensor
```

## Commands

```bash
fand run --config /etc/fand.toml              # persistent daemon
fand run --config test.toml --dry-run         # print planned writes
fand run --config test.toml --dry-run --json  # JSONL output
fand run --config test.toml --once            # single tick, then exit
fand curve --config test.toml --fan 0         # ASCII curve plot
fand set --fan 0 --rpm 2317 --commit          # one-shot write
fand selftest --iterations 5                  # round-trip verification
fand keys                                     # read fan metadata
fand keys --all                               # enumerate all SMC keys
```

## Security

- **Codesign**: ad-hoc signed (no Apple Developer ID). The nix store hash is the trust anchor.
- **Threat model**: see [`docs/SECURITY.md`](docs/SECURITY.md) and [`specs/005-smc-write-roundtrip/threat-model.md`](specs/005-smc-write-roundtrip/threat-model.md)
- **Supply chain**: cargo-vet attestations in `supply-chain/audits.toml`, cargo-deny policy in `deny.toml`
- **Verify provenance**: `gh attestation verify ./fand-aarch64-darwin --owner yolo-labz`

## Contributing

```bash
nix develop                  # dev shell with Rust + tools
cargo test                   # run all tests
cargo clippy -- -D warnings  # lint
```

## License

MIT — see [LICENSE](LICENSE).
