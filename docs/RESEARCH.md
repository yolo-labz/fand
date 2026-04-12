# Research Log

Two research passes informed this project. Both are reproduced below.

---

## Pass 1 — Why no existing tool works on Apple Silicon

### Candidate verdicts

- **macfand (sasamuku)** — FAIL. Linux-only daemon for ThinkPads
  (`/sys/class/hwmon`). Not macOS.
- **mac-fans-daemon / fand / macos-fand** — FAIL. No such projects exist
  on GitHub for Apple Silicon. `fand` exists for FreeBSD/Linux only.
- **smcFanControl (hholtmann)** — FAIL on Apple Silicon. The bundled `smc`
  binary uses Intel SMC key semantics; community reports (issues #100,
  #119) confirm `F0Tg` writes return `kIOReturnNotPrivileged` or no-op on
  M1+. No working AS fork merged upstream.
- **SMCKit (beltex)** — FAIL. Unmaintained since 2017, Intel-only key
  map, Swift 3. No AS port.
- **exelban/stats** — FAIL for control. GUI-only SwiftUI app. Has a fan
  control module but it shells out to a helper that only works on Intel;
  AS support is read-only (issues #1262, #1683 explicitly state "fan
  control is not available on Apple Silicon").
- **iStats (Ruby gem)** — FAIL. Read-only, Intel keys, abandoned.
- **Asahi fan tooling** — FAIL on macOS host. `asahi-bless` /
  `macsmc-hwmon` are Linux kernel drivers; nothing runs under macOS.
- **nixpkgs `pkgs.darwin.*`** — Nothing fan-related. Confirmed via
  nixpkgs grep.

### Technical reality

Writing `F0Tg`/`F1Tg` via `IOConnectCallStructMethod` against `AppleSMC`
**does still work from userspace as root** on M1–M5 (Tahoe 26 included)
— but only via the `SMC_CMD_WRITE_KEYINFO`/`WRITE_BYTES` selectors, and
only when invoked by a root process. No entitlement is required, but the
writes are silently clamped/overridden by `powerd` unless the caller
**continuously rewrites** the target (every 1–2s), because the OS thermal
manager re-asserts its own setpoint.

This is why every working AS fan tool is a persistent daemon, not a
one-shot. Every working implementation (TG Pro, Macs Fan Control, Volta)
ships a closed-source signed root helper installed via `SMJobBless`.

---

## Pass 2 — Custom daemon design

### Language: Rust (definitive)

- `io-kit-sys` + `core-foundation-sys` + `mach2` give direct
  `IOServiceOpen` / `IOConnectCallStructMethod` FFI with zero runtime
  overhead.
- Swift's IOKit bridging on AS works but drags the Swift runtime and has
  worse nix packaging (Xcode SDK gymnastics).
- C works but reinvents error handling.
- Go's cgo adds a goroutine/thread-pinning tax for a tight SMC write
  loop and produces bigger binaries.
- Nixpkgs story: `rustPlatform.buildRustPackage` on `aarch64-darwin` is
  battle-tested. Link IOKit + CoreFoundation via
  `darwin.apple_sdk.frameworks.{IOKit,CoreFoundation}`. Static-ish
  binary ~800KB–2MB.

**Crates**: `io-kit-sys`, `core-foundation`, `mach2`, `serde` + `toml`,
`signal-hook`, `tracing`. **Do not** pull `smc` crate from crates.io —
Intel-only and unmaintained.

### SMC protocol — concrete references

The userspace `AppleSMC` IOKit interface on Apple Silicon is **the same
`SMCParamStruct` protocol as Intel** — Apple kept the shim. The kernel
path differs (it talks to the real `macsmc` over mailbox), but userspace
sees identical selectors.

- **Selector**: `kSMCHandleYPCEvent = 2` for `IOConnectCallStructMethod`.
  Struct size = 80 bytes (`sizeof(SMCParamStruct)`).
- **Canonical struct layout**: `hholtmann/smcFanControl` →
  `smc-command/smc.{c,h}`. Copy `SMCKeyData_t`,
  `SMCKeyData_keyInfo_t`, command bytes (`kSMCReadKey=5`,
  `kSMCWriteKey=6`, `kSMCGetKeyInfo=9`).
- **AS read reference**: `exelban/stats` → `Modules/Sensors/smc.swift`
  (class `SMCService`). Confirms `IOServiceMatching("AppleSMC")` +
  `IOConnectCallStructMethod(conn, 2, &input, 80, &output, &outSize)`.
- **Asahi `macsmc.c`** (`drivers/platform/apple/smc_core.c`,
  `rtkit.c`) documents the wire format to the SMC coprocessor — useful
  for key semantics, NOT for userspace code.

### Apple Silicon key encodings

Critical: types differ from Intel.

| Key family       | Intel type | Apple Silicon type | Notes                       |
| ---------------- | ---------- | ------------------ | --------------------------- |
| `Tp01`…`Tp09`    | `sp78`     | `flt ` (4-byte LE) | p-core die temps            |
| `Tp0f` / `Tp0j`  | —          | `flt `             | e-core die                  |
| `Tg0f` / `Tg0j`  | —          | `flt `             | GPU die                     |
| `F0Ac` / `F1Ac`  | `fpe2`     | `flt `             | current RPM                 |
| `F0Tg` / `F1Tg`  | `fpe2`     | `flt `             | target RPM (write here)     |
| `F0Mn` / `F0Mx`  | `fpe2`     | `flt `             | min/max RPM bounds          |
| `F0Md` / `F1Md`  | `ui8 `     | `ui8 `             | mode: 0=auto, 1=forced      |
| `FNum`           | `ui8 `     | `ui8 `             | fan count                   |

**Always read the key's `dataType` via `kSMCGetKeyInfo` first and branch.**
Do not hardcode types.

### Fan count

- 14" M4/M5 Pro/Max = 2 fans
- 16" M4/M5 = 2 fans
- Base 14" M4 = 1 fan
- Enumerate via `FNum` key at startup; do not hardcode.

### powerd contention

- **500ms write cadence** reliably wins (empirical via stats issue
  tracker + smcFanControl Intel-era data).
- Write **both** `FxMd=1` and `FxTg=<rpm>` every tick — powerd flips
  `Md` back to 0 otherwise.
- No known boot-arg, nvram, or sysctl disables powerd's thermal loop
  on AS. Accept the race and out-write it.

### Code signing & launchd

- Root `LaunchDaemon` in `/Library/LaunchDaemons/*.plist` is
  **mandatory** — `AppleSMC` write selector requires root uid 0. User
  LaunchAgents cannot write SMC.
- **Ad-hoc signing (`codesign -s -`) is sufficient** on macOS 15/26 for
  a LaunchDaemon that only touches IOKit/AppleSMC.
- No entitlement required — `com.apple.developer.driverkit.*` is only
  for DriverKit dext bundles.
- Gatekeeper does NOT gate `launchctl bootstrap system` for locally
  built binaries invoked by root. Nix-store binaries have no quarantine
  xattr.
- Add `postFixup = "codesign -f -s - $out/bin/fand";` in the derivation
  to silence `amfid` on first run.
- `nix-darwin`: use `launchd.daemons.fand = { command = "${pkgs.fand}/bin/fand --config /etc/fand.toml"; serviceConfig = { RunAtLoad = true; KeepAlive = true; }; };`
  This writes to `/Library/LaunchDaemons/org.nixos.fand.plist` and runs
  as root by default.

### Risks & unknowns

- **macOS 26 Tahoe**: no public reports of `AppleSMC` userclient being
  gated behind a new entitlement as of 2026-04. Monitor `stats` /
  `iStat Menus` release notes — they break first as canaries.
- **M4/M5 SMC**: same userspace interface as M1–M3 per stats 2.11+
  changelogs. M4 added more `Tp*` keys, not new types.
- **`F0Mn` floor clamp**: some M2 Pro units refuse `Tg < Mn`. Clamp in
  userspace to `[Mn, Mx]` defensively.
- **SIP**: may currently be disabled on `macbook-pro` (per
  `~/.claude/.../MEMORY.md`). Re-enabling does NOT break `fand` — no
  kext, no dext, no protected FS writes.

### Prior art reading order

1. **`hholtmann/smcFanControl`** → `smc-command/smc.{c,h}` — canonical
   `SMCParamStruct` + selector 2.
2. **`exelban/stats`** → `Modules/Sensors/smc.swift` — AS-confirmed
   read path, key enumeration via `#KEY` / `#NUM`.
3. **`beltex/SMCKit`** — clean Swift abstraction; useful for key-type
   decoding tables.
4. **`AsahiLinux/linux`** → `drivers/platform/apple/smc*.c` +
   `macsmc-hwmon.c` — key semantics in C enums.
5. **`fermion-star/apple_fan_control`** + similar gists — partial AS
   experiments showing `IOConnectCallStructMethod` against `AppleSMC`
   on M1.
