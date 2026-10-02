#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    aikoql_storage_v2::fuzz::check_wal_replay(data);
});
