#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    aikoql_sdk::fuzz::check_error_frame(data);
});
