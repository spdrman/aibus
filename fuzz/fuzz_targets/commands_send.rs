#![no_main]

#[path = "../commands_checks.rs"]
mod commands_checks;

libfuzzer_sys::fuzz_target!(|data: &[u8]| commands_checks::commands_send(data));
