//! pf-input-feed executable — writes the focus-walk contract sequence as a
//! PRIVATE `input_event` byte stream (see the crate-level docs in lib.rs).
//!
//! Sinks: stdout (default), a regular file, or a named FIFO given via `--out`.
//! Opening a FIFO write-only blocks until a reader attaches, which IS the
//! readiness handshake — the old /proc fd scan of kernel-node holders and the
//! uinput virtual-device front end were removed by incident tsp-ihne1.56.5:
//! a host kernel input node is global to every seat, so this producer never
//! creates one. Create the FIFO first (`mkfifo`); a non-FIFO path is written
//! as a regular file.
//!
//! Usage:
//!   pf-input-feed [--count 120] [--interval-ms 150] [--out PATH|-]

use pf_input_feed::{encode_press_frame, focus_walk};
use std::{
    env, fs, io,
    os::unix::fs::FileTypeExt,
    path::Path,
    thread,
    time::{Duration, Instant},
};

const KNOWN_FLAGS: [&str; 3] = ["--count", "--interval-ms", "--out"];

fn value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn parse_usize(args: &[String], flag: &str, default: usize) -> Result<usize, String> {
    value(args, flag).map_or(Ok(default), |raw| {
        raw.parse().map_err(|error| format!("{flag}: {error}"))
    })
}

fn open_sink(path: Option<&str>) -> Result<Box<dyn io::Write>, String> {
    let Some(path) = path.filter(|candidate| *candidate != "-") else {
        return Ok(Box::new(io::BufWriter::new(io::stdout())));
    };
    let target = Path::new(path);
    let is_fifo = match fs::metadata(target) {
        Ok(metadata) => metadata.file_type().is_fifo(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(format!("{path}: {error}")),
    };
    let opened = if is_fifo {
        // O_WRONLY on a FIFO blocks until a reader attaches: natural readiness.
        fs::OpenOptions::new().write(true).open(target)
    } else {
        fs::File::create(target)
    };
    opened
        .map(|file| Box::new(io::BufWriter::new(file)) as Box<dyn io::Write>)
        .map_err(|error| format!("open {path}: {error}"))
}

fn run(args: &[String]) -> Result<(), String> {
    for arg in args {
        if arg.starts_with("--") && !KNOWN_FLAGS.contains(&arg.as_str()) {
            return Err(format!(
                "unknown flag: {arg} (the uinput front end and its --consumer-pid/--sequence \
                 flags were removed by tsp-ihne1.56.5; this producer writes a private \
                 input_event stream to --count/--interval-ms/--out)"
            ));
        }
    }
    let count = parse_usize(args, "--count", 120)?;
    let interval_ms = parse_usize(args, "--interval-ms", 150)?;
    let mut sink = open_sink(value(args, "--out").as_deref())?;

    let interval = Duration::from_millis(u64::try_from(interval_ms).unwrap_or(u64::MAX));
    let started = Instant::now();
    let mut fed = 0usize;
    for code in focus_walk(count) {
        sink.write_all(&encode_press_frame(code))
            .map_err(|error| format!("write: {error}"))?;
        sink.flush().map_err(|error| format!("flush: {error}"))?;
        fed += 1;
        if interval > Duration::ZERO {
            thread::sleep(interval);
        }
    }
    println!("fed={fed} wall_ms={}", started.elapsed().as_millis());
    Ok(())
}

fn main() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    run(&args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pf_input_feed::INPUT_EVENT_SIZE;
    use std::{io::Read, process::Command};

    fn temp_path(tag: &str) -> std::path::PathBuf {
        env::temp_dir().join(format!("pf-input-feed-{tag}-{}", std::process::id()))
    }

    fn feed_args(path: &Path, count: &str) -> Vec<String> {
        let out = path.display().to_string();
        [("--count", count), ("--interval-ms", "0"), ("--out", &out)]
            .iter()
            .flat_map(|(flag, value)| [(*flag).to_string(), (*value).to_string()])
            .collect()
    }

    #[test]
    fn run_writes_native_frames_to_a_regular_file() {
        let path = temp_path("regular");
        run(&feed_args(&path, "3")).unwrap();
        let bytes = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();

        assert_eq!(bytes.len(), 3 * 4 * INPUT_EVENT_SIZE);
        // focus_walk starts RIGHT: first record is a key-down on KEY_RIGHT.
        let ts = 2 * std::mem::size_of::<libc::c_long>();
        assert_eq!(
            u16::from_ne_bytes([bytes[ts], bytes[ts + 1]]),
            evdev::EventType::KEY.0
        );
        assert_eq!(
            u16::from_ne_bytes([bytes[ts + 2], bytes[ts + 3]]),
            pf_input_feed::KEY_RIGHT
        );
        assert_eq!(
            i32::from_ne_bytes([bytes[ts + 4], bytes[ts + 5], bytes[ts + 6], bytes[ts + 7]]),
            1
        );
        // Second record is the SYN_REPORT the decoder resyncs on.
        assert_eq!(
            &bytes[INPUT_EVENT_SIZE..2 * INPUT_EVENT_SIZE],
            &pf_input_feed::event_bytes(0, 0, 0)
        );
    }

    #[test]
    fn fifo_stream_arrives_intact_once_a_reader_attaches() {
        // Private-FIFO route: a named pipe in the temp dir — no kernel input
        // device, no host node. Mirrors pf-shell's FIFO decoder coverage.
        let path = temp_path("fifo");
        let status = Command::new("mkfifo").arg(&path).status().unwrap();
        assert!(status.success(), "mkfifo must succeed for the FIFO test");

        let writer_path = path.clone();
        let writer = thread::spawn(move || {
            // Blocks in open(O_WRONLY) until the reader below attaches.
            run(&feed_args(&writer_path, "2")).unwrap();
        });
        let mut received = Vec::new();
        fs::File::open(&path)
            .unwrap()
            .read_to_end(&mut received)
            .unwrap();
        writer.join().unwrap();
        fs::remove_file(&path).unwrap();

        assert_eq!(received.len(), 2 * 4 * INPUT_EVENT_SIZE);
        assert_eq!(
            &received[..INPUT_EVENT_SIZE],
            &encode_press_frame(pf_input_feed::KEY_RIGHT)[..INPUT_EVENT_SIZE]
        );
    }

    #[test]
    fn removed_front_end_flags_are_rejected_not_ignored() {
        let error = run(&["--consumer-pid".to_string(), "1234".to_string()]).unwrap_err();
        assert!(
            error.contains("--consumer-pid"),
            "unexpected error: {error}"
        );
        let error = run(&["--sequence".to_string(), "focus-walk".to_string()]).unwrap_err();
        assert!(error.contains("--sequence"), "unexpected error: {error}");
    }
}
