use std::fs::OpenOptions;
use std::io;
use std::path::Path;

const FRAME_HEADER_BYTES: usize = 8;
pub const LOG_FILE: &str = "wal";

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let field: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(field))
}

#[must_use]
pub fn zeroed_tail_start(bytes: &[u8]) -> Option<usize> {
    let mut offset = 0;
    while offset + FRAME_HEADER_BYTES <= bytes.len() {
        let length = usize::try_from(read_u32(bytes, offset)?).ok()?;
        if length == 0 {
            break;
        }
        let payload = bytes.get(offset + FRAME_HEADER_BYTES..offset + FRAME_HEADER_BYTES + length)?;
        if crc32fast::hash(payload) != read_u32(bytes, offset + 4)? {
            return None;
        }
        offset += FRAME_HEADER_BYTES + length;
    }
    let tail = &bytes[offset..];
    (!tail.is_empty() && tail.iter().all(|byte| *byte == 0)).then_some(offset)
}

pub fn discard_zeroed_tail(data_directory: &Path) -> io::Result<Option<u64>> {
    let path = data_directory.join(LOG_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(start) = zeroed_tail_start(&bytes) else {
        return Ok(None);
    };
    let file = OpenOptions::new().write(true).open(&path)?;
    let kept = u64::try_from(start).map_err(io::Error::other)?;
    file.set_len(kept)?;
    file.sync_all()?;
    let removed = u64::try_from(bytes.len() - start).map_err(io::Error::other)?;
    tracing::warn!(removed, kept, "discarded a zero-filled tail of the state log left by a power loss");
    Ok(Some(removed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut framed = u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec();
        framed.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
        framed.extend_from_slice(payload);
        framed
    }

    #[test]
    fn a_zero_filled_tail_after_valid_frames_is_found() {
        let mut log = frame(b"first");
        log.extend(frame(b"second"));
        let valid_length = log.len();
        log.extend([0_u8; 4096]);
        assert_eq!(zeroed_tail_start(&log), Some(valid_length));
    }

    #[test]
    fn a_log_without_a_zeroed_tail_is_left_alone() {
        let mut log = frame(b"first");
        assert_eq!(zeroed_tail_start(&log), None);
        log.extend(&frame(b"second")[..5]);
        assert_eq!(zeroed_tail_start(&log), None, "a torn frame is Quorium's to handle");
    }

    #[test]
    fn non_zero_damage_is_never_truncated() {
        let mut log = frame(b"first");
        log.extend([0_u8; 16]);
        log.extend([7_u8; 3]);
        assert_eq!(zeroed_tail_start(&log), None);
        let mut corrupt = frame(b"first");
        corrupt[9] ^= 0xff;
        corrupt.extend([0_u8; 16]);
        assert_eq!(zeroed_tail_start(&corrupt), None);
    }

    #[test]
    fn the_tail_is_truncated_on_disk() {
        let directory = tempfile::tempdir().unwrap();
        let mut log = frame(b"first");
        log.extend([0_u8; 64]);
        std::fs::write(directory.path().join(LOG_FILE), &log).unwrap();
        assert_eq!(discard_zeroed_tail(directory.path()).unwrap(), Some(64));
        assert_eq!(std::fs::read(directory.path().join(LOG_FILE)).unwrap(), frame(b"first"));
        assert_eq!(discard_zeroed_tail(directory.path()).unwrap(), None);
    }
}
