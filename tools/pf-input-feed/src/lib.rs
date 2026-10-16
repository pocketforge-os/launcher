//! pf-input-feed — PRIVATE-STREAM input producer.
//!
//! Encodes the focus-walk contract sequence as native `struct input_event` byte frames
//! (key down, `SYN_REPORT`, key up, `SYN_REPORT`) for delivery over a caller-chosen private
//! sink: stdout, a regular file or a named FIFO. No kernel input device is ever created —
//! the uinput executable front end was removed by incident tsp-ihne1.56.5 (uinput is
//! host-global; containers isolate nothing). The stream framing is exactly what pf-shell's
//! regular-file/FIFO evdev decoder consumes (crates/pf-shell/src/lib.rs: `decode_with`
//! writes an event stream to a regular file and feeds `EvdevActionSource::open_with_map`;
//! EVIOCGRAB applies to character devices only).
//!
//! The fail-closed host-open authorization guard ported in tsp-ihne1.56.1 lives on as
//! [`uinput_domain`] (kept for the wave-2 fixture migration; this crate no longer calls
//! it from any executable path).

use evdev::{EventType, InputEvent};
use std::io;

pub mod uinput_domain;

pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const CONTRACT_KEYS: [u16; 11] = [305, 304, 307, 308, 316, 314, 315, 103, 108, 105, 106];

/// Size of one native `struct input_event` record: a zeroed `struct timeval`
/// (two `c_long`s) plus `u16` type, `u16` code and `i32` value.
pub const INPUT_EVENT_SIZE: usize = 2 * std::mem::size_of::<libc::c_long>() + 8;

/// One native `input_event` record with a zero timestamp, in native endian —
/// the same encoding pf-shell's decoder-test helper builds.
#[must_use]
pub fn event_bytes(kind: u16, code: u16, value: i32) -> [u8; INPUT_EVENT_SIZE] {
    let mut record = [0u8; INPUT_EVENT_SIZE];
    let ts = 2 * std::mem::size_of::<libc::c_long>();
    record[ts..ts + 2].copy_from_slice(&kind.to_ne_bytes());
    record[ts + 2..ts + 4].copy_from_slice(&code.to_ne_bytes());
    record[ts + 4..ts + 8].copy_from_slice(&value.to_ne_bytes());
    record
}

pub fn focus_walk(count: usize) -> impl Iterator<Item = u16> {
    (0..count).map(|index| if index % 2 == 0 { KEY_RIGHT } else { KEY_LEFT })
}

#[must_use]
pub fn encoded_press(code: u16) -> [InputEvent; 2] {
    [
        InputEvent::new(EventType::KEY.0, code, 1),
        InputEvent::new(EventType::KEY.0, code, 0),
    ]
}

/// One complete press frame for `code`: key-down, `SYN_REPORT`, key-up, `SYN_REPORT`,
/// as raw native `input_event` bytes. The `SYN_REPORT` after every event is what the
/// evdev decoders resync on — a stream without it is malformed, not equivalent.
#[must_use]
pub fn encode_press_frame(code: u16) -> [u8; 4 * INPUT_EVENT_SIZE] {
    let [down, up] = encoded_press(code);
    let syn = event_bytes(0, 0, 0);
    let mut frame = [0u8; 4 * INPUT_EVENT_SIZE];
    frame[..INPUT_EVENT_SIZE].copy_from_slice(&event_bytes(
        down.event_type().0,
        down.code(),
        down.value(),
    ));
    frame[INPUT_EVENT_SIZE..2 * INPUT_EVENT_SIZE].copy_from_slice(&syn);
    frame[2 * INPUT_EVENT_SIZE..3 * INPUT_EVENT_SIZE].copy_from_slice(&event_bytes(
        up.event_type().0,
        up.code(),
        up.value(),
    ));
    frame[3 * INPUT_EVENT_SIZE..].copy_from_slice(&syn);
    frame
}

/// Writes one [`encode_press_frame`] per code into `sink`, flushing after each frame.
/// Returns the number of presses written.
///
/// # Errors
///
/// Returns the first `write`/`flush` error from `sink` (e.g. `EPIPE` when a FIFO
/// reader went away mid-stream). A partial write is never reported as success.
pub fn write_press_frames<W: io::Write>(
    sink: &mut W,
    codes: impl Iterator<Item = u16>,
) -> io::Result<usize> {
    let mut fed = 0;
    for code in codes {
        sink.write_all(&encode_press_frame(code))?;
        sink.flush()?;
        fed += 1;
    }
    Ok(fed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};

    #[test]
    fn focus_walk_alternates_visible_focus_moves() {
        assert_eq!(focus_walk(5).collect::<Vec<_>>(), [106, 105, 106, 105, 106]);
    }

    #[test]
    fn encoding_is_a_key_press_then_release() {
        let events = encoded_press(KEY_RIGHT);
        assert_eq!(events[0].event_type(), EventType::KEY);
        assert_eq!(events[0].code(), KEY_RIGHT);
        assert_eq!(events[0].value(), 1);
        assert_eq!(events[1].code(), KEY_RIGHT);
        assert_eq!(events[1].value(), 0);
    }

    #[test]
    fn encode_press_frame_matches_manual_native_encoding() {
        let syn = event_bytes(0, 0, 0);
        let mut expected = Vec::new();
        expected.extend_from_slice(&event_bytes(EventType::KEY.0, KEY_LEFT, 1));
        expected.extend_from_slice(&syn);
        expected.extend_from_slice(&event_bytes(EventType::KEY.0, KEY_LEFT, 0));
        expected.extend_from_slice(&syn);
        assert_eq!(encode_press_frame(KEY_LEFT).to_vec(), expected);
        assert_eq!(expected.len(), 4 * INPUT_EVENT_SIZE);
    }

    #[test]
    fn frame_carries_syn_report_after_every_event() {
        // Positive control: the decoder in pf-shell (regular-file/FIFO evdev decoder
        // tests, crates/pf-shell/src/lib.rs) resyncs on SYN_REPORT — every event in
        // our stream must be followed by one.
        let frame = encode_press_frame(KEY_RIGHT);
        let records: Vec<&[u8]> = frame.chunks(INPUT_EVENT_SIZE).collect();
        assert_eq!(records.len(), 4);
        for record in [records[1], records[3]] {
            assert_eq!(record, &event_bytes(0, 0, 0)[..]);
        }
        // Negative control: the same press WITHOUT the SYN framing must differ —
        // a partial/unsynced stream is not silently accepted as equivalent.
        let mut missing_syn = Vec::new();
        missing_syn.extend_from_slice(&event_bytes(EventType::KEY.0, KEY_RIGHT, 1));
        missing_syn.extend_from_slice(&event_bytes(EventType::KEY.0, KEY_RIGHT, 0));
        assert_ne!(frame.to_vec(), missing_syn);
    }

    #[test]
    fn write_press_frames_streams_the_focus_walk() {
        let mut sink: Vec<u8> = Vec::new();
        let fed = write_press_frames(&mut sink, focus_walk(3)).unwrap();
        assert_eq!(fed, 3);
        assert_eq!(sink.len(), 3 * 4 * INPUT_EVENT_SIZE);
        // focus_walk starts RIGHT: first record is a key-down on KEY_RIGHT.
        let ts = 2 * std::mem::size_of::<libc::c_long>();
        assert_eq!(
            u16::from_ne_bytes([sink[ts], sink[ts + 1]]),
            EventType::KEY.0
        );
        assert_eq!(u16::from_ne_bytes([sink[ts + 2], sink[ts + 3]]), KEY_RIGHT);
        assert_eq!(
            i32::from_ne_bytes([sink[ts + 4], sink[ts + 5], sink[ts + 6], sink[ts + 7]]),
            1
        );
    }

    struct BrokenPipe;

    impl Write for BrokenPipe {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from_raw_os_error(libc::EPIPE))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from_raw_os_error(libc::EPIPE))
        }
    }

    #[test]
    fn write_press_frames_propagates_a_broken_sink() {
        // Negative control: a reader that vanished mid-stream (closed FIFO) must
        // surface EPIPE — never a silent partial write.
        let mut sink = BrokenPipe;
        let error = write_press_frames(&mut sink, focus_walk(2)).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EPIPE));
    }
}
