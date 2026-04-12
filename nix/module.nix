# nix-darwin module for fand — FR-055..058, FR-092..096.
#
# Usage in your nix-darwin configuration:
#
#   services.fand = {
#     enable = true;
#     settings = {
#       config_version = 1;
#       poll_interval_ms = 500;
#       log_level = "info";
#       fan = [{
#         index = 0;
#         sensors = [{ smc = "Tf04"; } { smc = "Tf09"; }];  # M3 Pro
#         curve = [[50 2317] [65 3500] [75 5000] [85 6550]];
#       }];
#     };
#   };
#
# FR-096: log rotation for /var/log/fand.{err,out} is the operator's
# responsibility. Recommended: add a newsyslog.d entry or use macOS's
# built-in asl rotation. A future enhancement may switch to os_log.
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.fand;
  tomlFormat = pkgs.formats.toml {};
in {
  options.services.fand = {
    enable = lib.mkEnableOption "fand Apple Silicon fan daemon";

    package = lib.mkPackageOption pkgs "fand" {};

    settings = lib.mkOption {
      type = tomlFormat.type;
      default = {};
      description = ''
        Contents of /etc/fand.toml. Passed through pkgs.formats.toml.
        See specs/006-fan-curve-config/contracts/config-schema.md for
        the full field reference.
      '';
      example = lib.literalExpression ''
        {
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
            curve = [[50 2317] [65 3500] [75 5000] [85 6550]];
          }];
        }
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    # FR-041: assertions for platform validation and config sanity.
    assertions = [
      {
        assertion = pkgs.stdenv.isDarwin;
        message = "fand is macOS-only — it cannot run on Linux or other platforms.";
      }
    ];

    warnings = lib.optional
      ((cfg.settings.fan or []) == [])
      "services.fand.settings.fan is empty — no fans will be controlled. Add at least one [[fan]] section.";

    # FR-056: generate /etc/fand.toml from Nix attribute set.
    environment.etc."fand.toml".source =
      tomlFormat.generate "fand.toml" cfg.settings;

    # FR-057 + FR-092 + FR-093: LaunchDaemon plist with hardening.
    launchd.daemons.fand = {
      # FR-058: pass --config /etc/fand.toml to the binary.
      command = "${cfg.package}/bin/fand run --config /etc/fand.toml";
      serviceConfig = {
        # FR-057: basic daemon lifecycle.
        RunAtLoad = true;
        ThrottleInterval = 5;

        # FR-093: KeepAlive with SuccessfulExit=false — respawn only
        # on non-zero exit. Prevents infinite crash loops on bad config.
        KeepAlive = {
          SuccessfulExit = false;
        };

        # FR-092: hardened plist options.
        UserName = "root";
        GroupName = "wheel";
        SessionCreate = false;
        Umask = 63; # 0o077
        WorkingDirectory = "/";
        EnvironmentVariables = {};

        # CHK023: Nice=-20 + Interactive for scheduling priority.
        Nice = -20;
        ProcessType = "Interactive";

        # FR-017: sandbox profile via nix store path (not hardcoded).
        # SandboxProfile = "${cfg.package}/share/fand/fand-set.sb";

        # Log output paths (FR-096: rotation is operator's responsibility).
        StandardOutPath = "/var/log/fand.out";
        StandardErrorPath = "/var/log/fand.err";
      };
    };

    # FR-094: SIGHUP on config change via postActivation.
    # Uses launchctl kill to target the exact launchd job by label,
    # avoiding PID races (research.md RD-06).
    system.activationScripts.postActivation.text = lib.mkAfter ''
      if /usr/bin/diff -q \
           /run/current-system/etc/fand.toml \
           ${config.environment.etc."fand.toml".source} &>/dev/null 2>&1; then
        : # config unchanged — no SIGHUP needed
      else
        echo "fand config changed, sending SIGHUP..." >&2
        /bin/launchctl kill SIGHUP system/com.github.yolo-labz.fand 2>/dev/null || true
      fi
    '';
  };
}
