#![no_main]

#[path = "../checks.rs"]
mod checks;

libfuzzer_sys::fuzz_target!(|data: &[u8]| checks::tool_pattern(data));
