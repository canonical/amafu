#![no_main]

use amafu_interfaces::SliceChange;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    for line in bytes.split(|byte| *byte == b'\n') {
        if let Ok(change) = serde_json::from_slice::<SliceChange>(line) {
            let encoded = serde_json::to_vec(&change).expect("a decoded change encodes again");
            let decoded: SliceChange = serde_json::from_slice(&encoded).expect("an encoded change decodes");
            assert_eq!(decoded, change);
        }
    }
});
