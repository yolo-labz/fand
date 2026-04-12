# Apple Silicon SMC Protocol Reference

This is a working reference for implementing `fand`. Source: cross-checked
from `hholtmann/smcFanControl/smc-command/smc.{c,h}`, `exelban/stats`
`Modules/Sensors/smc.swift`, `beltex/SMCKit`, and `AsahiLinux/linux`
`drivers/platform/apple/macsmc-hwmon.c`.

## Userspace interface

Apple kept the same `AppleSMC` IOKit shim from Intel. From userspace:

```c
io_service_t   service = IOServiceGetMatchingService(
                              kIOMasterPortDefault,
                              IOServiceMatching("AppleSMC"));
io_connect_t   conn;
IOServiceOpen(service, mach_task_self(), 0, &conn);

SMCParamStruct input  = { 0 };
SMCParamStruct output = { 0 };
size_t out_size = sizeof(SMCParamStruct);  // 80

input.key  = key_fourcc;          // e.g. 'F0Tg' as u32 big-endian
input.data8 = kSMCReadKey;        // 5 = read, 6 = write, 9 = get key info

IOConnectCallStructMethod(
    conn,
    /* selector */ 2,             // kSMCHandleYPCEvent
    &input,  sizeof(input),
    &output, &out_size);
```

**Selector 2** is the only selector needed for read/write/get-info. The
struct size is exactly **80 bytes** on both architectures.

## SMCParamStruct (80 bytes)

```c
typedef struct {
    UInt32                  key;            // 4 bytes, fourcc
    SMCKeyData_vers_t       vers;           // 6 bytes
    SMCKeyData_pLimitData_t pLimitData;     // 16 bytes
    SMCKeyData_keyInfo_t    keyInfo;        // 12 bytes (size, type, attr)
    UInt8                   result;
    UInt8                   status;
    UInt8                   data8;          // command byte
    UInt32                  data32;
    SMCBytes_t              bytes;          // 32 bytes payload
} SMCParamStruct;

typedef struct {
    UInt32  dataSize;
    UInt32  dataType;     // fourcc, e.g. 'flt ', 'ui8 ', 'fpe2'
    UInt8   dataAttributes;
} SMCKeyData_keyInfo_t;
```

Total: **80 bytes** with padding/alignment as in `smc.h`.

## Command bytes (`data8`)

| Constant            | Value | Use                                       |
| ------------------- | ----- | ----------------------------------------- |
| `kSMCReadKey`       | 5     | Read value of `key`                       |
| `kSMCWriteKey`      | 6     | Write `bytes[0..keyInfo.dataSize]` to key |
| `kSMCGetKeyInfo`    | 9     | Populate `keyInfo` (size + type) for key  |
| `kSMCGetKeyFromIdx` | 8     | Used with `#KEY` enumeration              |

## Read flow

1. Set `input.key = key`, `input.data8 = kSMCGetKeyInfo`. Call. Read
   `output.keyInfo.dataSize` and `output.keyInfo.dataType`.
2. Set `input.keyInfo = output.keyInfo`, `input.data8 = kSMCReadKey`.
   Call. Read `output.bytes[0..dataSize]`.
3. Decode `output.bytes` according to `dataType`.

## Write flow

1. Get `keyInfo` as above (must match the key's actual type).
2. Encode value into `input.bytes[0..dataSize]` per `dataType`.
3. Set `input.keyInfo = keyInfo`, `input.data8 = kSMCWriteKey`. Call.
4. Check `output.result == 0` (`kIOReturnSuccess`).

## Type encodings (Apple Silicon)

| Type fourcc | Size | Decode                                          |
| ----------- | ---- | ----------------------------------------------- |
| `flt `      | 4    | `f32::from_le_bytes(bytes[0..4])` — IEEE754 LE  |
| `ui8 `      | 1    | `bytes[0] as u8`                                |
| `ui16`      | 2    | `u16::from_be_bytes(bytes[0..2])`               |
| `ui32`      | 4    | `u32::from_be_bytes(bytes[0..4])`               |
| `si8 `      | 1    | `bytes[0] as i8`                                |
| `sp78`      | 2    | Intel temp; `(be_i16) / 256.0` — NOT used on AS |
| `fpe2`      | 2    | Intel fan; `be_u16 >> 2` — NOT used on AS       |
| `flag`      | 1    | `bytes[0] != 0`                                 |

**Critical**: on Apple Silicon, temperatures and fan RPMs are `flt `,
not `sp78` / `fpe2`. Always read `keyInfo.dataType` first; never assume.

## Key catalog (Apple Silicon M-series)

### Enumeration

| Key    | Type   | Meaning                          |
| ------ | ------ | -------------------------------- |
| `#KEY` | `ui32` | Total number of SMC keys         |
| `#NUM` | `ui8`  | (Intel legacy, sometimes absent) |
| `FNum` | `ui8`  | Number of fans                   |

Iterate all keys via `kSMCGetKeyFromIdx` with index 0..`#KEY`.

### Temperatures (most relevant)

| Key    | Type   | Meaning                       |
| ------ | ------ | ----------------------------- |
| `Tp01` | `flt ` | P-core 1 die                  |
| `Tp05` | `flt ` | P-core 5 die                  |
| `Tp09` | `flt ` | P-core 9 die                  |
| `Tp0D` | `flt ` | P-core D die                  |
| `Tp0f` | `flt ` | E-core f die                  |
| `Tp0j` | `flt ` | E-core j die                  |
| `Tg0f` | `flt ` | GPU f die                     |
| `Tg0j` | `flt ` | GPU j die                     |
| `TH0x` | `flt ` | NAND/SSD                      |
| `TW0P` | `flt ` | Wireless                      |
| `TB1T` | `flt ` | Battery 1                     |

The exact set of `Tp0*` keys varies by SoC variant (M1/M2/M3/M4/M5).
Enumerate at startup; don't hardcode beyond a default set in config.

### Fan keys

**⚠️ This table reflects the Intel-era SMCKit convention. See the Apple Silicon section below for the actual writability semantics on M-series.**

For each fan index `N` in `0..FNum`:

| Key    | Type   | Intel R/W | Apple Silicon R/W | Meaning                                |
| ------ | ------ | --------- | ----------------- | -------------------------------------- |
| `FNAc` | `flt ` | R         | R                 | Current actual RPM                     |
| `FNTg` | `flt ` | RW        | **R only** (alias) | Intel: target RPM. AS: read-only alias for thermalmonitord effective view |
| `FNMn` | `flt ` | R         | R                 | Minimum RPM bound                      |
| `FNMx` | `flt ` | R         | R                 | Maximum RPM bound                      |
| `FNMd` | `ui8 ` | RW        | not present       | Intel: uppercase mode. AS: use lowercase `FNmd` instead |
| `FNmd` | `ui8 ` | not present | **RW** (values 0/1 only safe; 2/3 stop fan) | **Apple Silicon mode register — THE ONLY WRITABLE CONTROL** |
| `FNDc` | `flt ` | not present | R only            | AS duty cycle sensor (refuses writes with 0x86) |
| `FNSt` | `ui8 ` | not present | R only            | AS step index (0–7, refuses writes with 0x86) |
| `FNS0..FNS7` | `hex` 2B | not present | unknown   | AS 8 step preset slots (probable step-curve control) |
| `FNSf` | `flt ` | R         | R                 | Safe RPM                               |
| `FNID` | `ch8*` | R         | R                 | Fan name string (Intel only on most M-series) |

(Where `N` is the fan index character: `0`, `1`, ...)

To take control of fan `0` on **Intel Macs** (Intel-era SMCKit, NOT what fand targets):
1. Write `F0Md = 1`
2. Write `F0Tg = <desired_rpm>`
3. Repeat both every 500ms — powerd will reset `F0Md` to 0 otherwise.

To release control on Intel:
1. Write `F0Md = 0`

### **Apple Silicon M-series — verified live on Mac17,2 (RD-08, 2026-04-11)**

The Apple Silicon SMC has a SIGNIFICANTLY different writable surface from Intel. The Intel-era `F0Md` + `F0Tg` recipe DOES NOT WORK on M-series. Live findings from feature 005 session 5 on Mac17,2 (M3 Pro, macOS 26.4):

| Key | Type | Behaviour on M-series |
|---|---|---|
| `F0md` *(lowercase)* | ui8 | **WRITABLE**, accepts values 0–3 with sticky read-back. Values ≥4 return SMC `0x82`. |
| `F0Md` *(uppercase)* | — | Not present on Mac17,2 — Intel convention only |
| `F0Tg` | flt | **READ-ONLY ALIAS** for thermalmonitord's effective-target view. Writes are accepted by the SMC firmware but the readback returns the current `F0Ac`, not the written value (verified via round-trip mismatch). |
| `F0Dc` | flt | **READ-ONLY** duty-cycle sensor in [0.0, 1.0] range. Writes return SMC `0x86` regardless of `F0md` state. |
| `F0St` | ui8 | Read-only step index (currently 0–7). Writes return SMC `0x86`. |
| `F0S0..F0S7` | hex | 8 step preset slots. Probable role in the M-series step-curve control model but writability not yet verified. |
| `Ftst` | — | **DOES NOT EXIST on Mac17,2**. The Intel-era diagnostic unlock key is absent on this SoC. Reading or writing returns SMC `0x84` (key not found). Feature 005's `begin_diagnostic_unlock` gracefully falls through with a WARN log. |

**`F0md` value semantics on Apple Silicon M-series**:

| Value | Effect on F0Ac (actual RPM) | Effect on F0Dc (duty) | Safe? |
|---|---|---|---|
| 0 | system thermal manager controlled | varies (0.07 idle → 0.5+ under load) | ✅ baseline |
| **1** | **forced minimum (~`F0Mn`)** | **~0.07 (7%)** | ✅ **THE PATH** |
| 2 | F0Ac → 0 (fan stops) | F0Dc → 0 | ⚠️ **DANGEROUS** |
| 3 | F0Ac → 0 (fan stops) | F0Dc → 0 | ⚠️ **DANGEROUS** |
| ≥4 | rejected with SMC `0x82` ("system mode rejects") | n/a | rejected |

**Apple Silicon recipe** (verified by feature 005 live testing):

To engage forced-minimum on fan `0`:
1. `IOServiceOpen` → AppleSMC user client (no entitlement required, root needed for write)
2. Write `F0md = 1` via `IOConnectCallStructMethod(selector=2, cmd=6)` — round-trips byte-for-byte within the same call
3. The SMC firmware automatically updates `F0Dc` to ~0.07 within ~250 ms; `F0Ac` drops to within ±50 RPM of `F0Mn` within 1–2 seconds
4. **No `Ftst` unlock is required** — the write succeeds directly

To release:
1. Write `F0md = 0` — the system thermal manager (`thermalmonitord`) immediately resumes control

**There is NO known mechanism on Apple Silicon M-series to set an arbitrary RPM target via the SMC interface.** `F0Tg` writes are silently aliased; `F0Dc` writes are refused; `F0St` writes are refused. The only writable control is the binary `F0md=0/1`. Operators wanting per-RPM control must wait for an RTKit endpoint or future macOS interface — neither is currently identified.

**Why `Ftst` is absent**: feature 001's RD-01 research assumed `Ftst` is universal across M1–M5 based on `agoodkind/macos-smc-fan` (an Intel + early-AS project). Live testing on Mac17,2 in 2026-04-11 falsified this — the key simply does not exist in the SMC keyspace on this SoC. This is documented in `specs/005-smc-write-roundtrip/research.md` RD-08 sessions 4–5 with the full `fand keys --all` enumeration evidence.

## Reference implementations

- **`hholtmann/smcFanControl`** — `smc-command/smc.c` lines 1–400.
  Canonical struct + selector. Type decode functions are Intel-only;
  ignore them, use AS table above.
- **`exelban/stats`** — `Modules/Sensors/smc.swift` class `SMCService`.
  Confirmed working READ path on Apple Silicon. See `read()` method
  and `getValue()` for the type-switch pattern.
- **`AsahiLinux/linux`** — `drivers/platform/apple/macsmc-hwmon.c`
  enumerates the same keys from the kernel side. Useful for
  cross-checking which keys are read-only vs read-write.

## Temperature sensor keys by Apple Silicon generation (RD-03)

**Critical**: sensor key naming changes completely between generations. The same
fourcc code can mean different things on different SoCs. Always discover sensors
at runtime via `sudo fand keys --all | grep '^T'`. Source: `exelban/stats`
`Modules/Sensors/values.swift`.

| Generation | E-core prefix | P-core prefix | Example P-core keys | GPU prefix |
|-----------|--------------|--------------|---------------------|------------|
| M1 | `Tp0*` (09, 0T) | `Tp0*` (01, 05, 0D, 0H, 0L, 0P, 0X, 0b) | `Tp01`, `Tp05` | `Tg0*` |
| M2 | `Tp1*` (1h, 1t, 1p, 1l) | `Tp0*` (01, 05, 09, 0D, 0X, 0b, 0f, 0j) | `Tp01`, `Tp05` | `Tg0*` |
| M3 | `Te0*` (05, 0L, 0P, 0S) | **`Tf0*`/`Tf4*`** (04, 09, 0A..0E, 44..4E) | `Tf04`, `Tf09` | `Tf1*`/`Tf2*` |
| M4 | `Te0*` (05, 0S, 09, 0H) | **Back to `Tp0*`** (01, 05, 09, 0D, 0V..0e) | `Tp01`, `Tp05` | `Tg0*`/`Tg1*` |
| M5 | `Tp0*` (super+P) | `Tp0*` (0O, 0R, 0U..0y) | `Tp0O`, `Tp0R` | `Tg0*`/`Tg1*` |

**Mac17,2 (M3 Pro)**: P-core sensors are `Tf04`, `Tf09`, `Tf0A`, `Tf0B`, `Tf0D`, `Tf0E`.
E-cores: `Te05`, `Te0L`. GPU: `Tf14`, `Tf18`, `Tf19`, `Tf1A`.

**Discovering sensors on your machine**:
```bash
sudo fand keys --all | grep '^T'     # all temperature keys
sudo fand keys --read Tf04           # read a specific sensor
```

## Known gotchas

- Some M2 Pro units **refuse `Tg < Mn`** writes silently. Always clamp
  to `[Mn, Mx]` in userspace.
- `kIOReturnNotPrivileged` on write means the process is not root.
- Intermittent `kIOReturnTimeout` on read happens during sleep
  transitions; retry once after 50ms.
- The `result` byte (`output.result`) is the SMC's own status, separate
  from the IOKit return code. Both must be 0.
