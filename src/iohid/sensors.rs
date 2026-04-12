// SAFETY (module-level soundness contract):
// This module wraps IOHIDEventSystemClient for temperature sensor enumeration.
// The safe public API (enumerate_sensors, read_all_sensors) upholds these invariants:
// - Matching dictionaries use PrimaryUsagePage/PrimaryUsage for temperature class only.
// - Sensor values are validated (range, rate-of-change, stuck-at) before exposure.
// - All CFType references are released via core-foundation's Drop impls.
