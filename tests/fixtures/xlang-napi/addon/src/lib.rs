// Fixture (P5, napi): napi-rs export (JavaScript name is camelCase).
use napi_derive::napi;

/// Unique: `addon.sumValues(...)` -> proven.
#[napi]
pub fn sum_values(a: i32, b: i32) -> i32 {
    a + b
}
