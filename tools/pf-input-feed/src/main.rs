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
//! The event stream is PURE binary on every sink: the `fed=N wall_ms=M` status
//! line goes to stderr, so a consumer reading stdout to EOF (the default sink
//! or `--out -`) sees only whole `input_event` records.
//!
//! Arguments are parsed single-pass and arity-aware: a missing value, a flag
//! consumed as another flag's value, a stray positional token or a duplicate
//! flag is an error, never a silent default — `--out --count 1` must not
//! create a file named `--count`.
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
const REMOVED_FRONT_END_NOTE: &str = "the uinput front end and its --consumer-pid/--sequence \
     flags were removed by tsp-ihne1.56.5; this producer writes a private \
     input_event stream to --count/--interval-ms/--out";

#[derive(Debug, PartialEq, Eq)]
struct Options {
    count: usize,
    interval_ms: usize,
    out: Option<String>,
}

fn parse_number(flag: &str, raw: &str) -> Result<usize, String> {
    raw.parse()
        .map_err(|error| format!("{flag}: expected a non-negative integer, got {raw:?}: {error}"))
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        count: 120,
        interval_ms: 150,
        out: None,
    };
    let mut seen = [false; KNOWN_FLAGS.len()];
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((name, rest)) => (name, Some(rest)),
            None => (arg.as_str(), None),
        };
        let Some(index) = KNOWN_FLAGS.iter().position(|known| *known == flag) else {
            if flag.starts_with("--") {
                return Err(format!("unknown flag: {flag} ({REMOVED_FRONT_END_NOTE})"));
            }
            return Err(format!(
                "unexpected positional argument: {arg} \
                 (only --count/--interval-ms/--out[=VALUE] are accepted)"
            ));
        };
        if seen[index] {
            return Err(format!("duplicate flag: {flag} (pass it exactly once)"));
        }
        seen[index] = true;
        let value = match inline {
            Some(rest) => rest.to_string(),
            None => match iter.next() {
                None => return Err(format!("{flag}: missing value")),
                Some(next) if next.starts_with("--") => {
                    return Err(format!(
                        "{flag}: missing value (refusing to consume the flag {next} as a value)"
                    ));
                }
                Some(next) => next.clone(),
            },
        };
        match index {
            0 => options.count = parse_number("--count", &value)?,
            1 => options.interval_ms = parse_number("--interval-ms", &value)?,
            _ => options.out = Some(value),
        }
    }
    Ok(options)
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
    let options = parse_args(args)?;
    let mut sink = open_sink(options.out.as_deref())?;

    let interval = Duration::from_millis(u64::try_from(options.interval_ms).unwrap_or(u64::MAX));
    let started = Instant::now();
    let mut fed = 0usize;
    for code in focus_walk(options.count) {
        sink.write_all(&encode_press_frame(code))
            .map_err(|error| format!("write: {error}"))?;
        sink.flush().map_err(|error| format!("flush: {error}"))?;
        fed += 1;
        if interval > Duration::ZERO {
            thread::sleep(interval);
        }
    }
    // Status rides stderr: stdout may BE the binary sink (default or `--out -`).
    eprintln!("fed={fed} wall_ms={}", started.elapsed().as_millis());
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

    fn strs(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
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
        let error = run(&strs(&["--consumer-pid", "1234"])).unwrap_err();
        assert!(
            error.contains("--consumer-pid"),
            "unexpected error: {error}"
        );
        let error = run(&strs(&["--sequence", "focus-walk"])).unwrap_err();
        assert!(error.contains("--sequence"), "unexpected error: {error}");
    }

    #[test]
    fn parser_rejects_a_missing_value_for_every_option() {
        for flag in KNOWN_FLAGS {
            let error = parse_args(&strs(&[flag])).unwrap_err();
            assert!(
                error.contains(flag) && error.contains("missing value"),
                "{flag}: {error}"
            );
            // run() must reject at parse time — before any sink is opened.
            let error = run(&strs(&[flag])).unwrap_err();
            assert!(error.contains("missing value"), "{flag}: {error}");
        }
    }

    #[test]
    fn parser_rejects_a_following_flag_consumed_as_the_value() {
        for (flag, victim) in [
            ("--out", "--count"),
            ("--count", "--interval-ms"),
            ("--interval-ms", "--out"),
        ] {
            let error = parse_args(&strs(&[flag, victim, "1"])).unwrap_err();
            assert!(
                error.contains(flag) && error.contains("missing value"),
                "{flag} {victim}: {error}"
            );
        }
        // The old parser silently created/truncated a file named `--count`
        // here; the fix must reject BEFORE open_sink touches the filesystem.
        let stray = Path::new("--count");
        let error = run(&strs(&["--out", "--count", "1"])).unwrap_err();
        assert!(error.contains("missing value"), "{error}");
        assert!(
            !stray.exists(),
            "run() rejected late: open_sink already created {stray:?}"
        );
    }

    #[test]
    fn parser_rejects_stray_positional_arguments() {
        let error = parse_args(&strs(&["junk"])).unwrap_err();
        assert!(error.contains("positional"), "{error}");
        let error = parse_args(&strs(&["--count", "3", "junk"])).unwrap_err();
        assert!(error.contains("positional"), "{error}");
        let error = run(&strs(&["--interval-ms", "0", "--count", "1", "extra"])).unwrap_err();
        assert!(error.contains("positional"), "{error}");
    }

    #[test]
    fn parser_rejects_duplicate_flags() {
        for args in [
            ["--count", "1", "--count", "2"],
            ["--interval-ms", "0", "--interval-ms", "1"],
            ["--out", "-", "--out", "-"],
        ] {
            let error = parse_args(&strs(&args)).unwrap_err();
            assert!(error.contains("duplicate"), "{args:?}: {error}");
        }
    }

    #[test]
    fn parser_keeps_documented_defaults_and_accepts_inline_values() {
        assert_eq!(
            parse_args(&[]).unwrap(),
            Options {
                count: 120,
                interval_ms: 150,
                out: None
            }
        );
        assert_eq!(
            parse_args(&strs(&["--count=2", "--interval-ms=0", "--out=-"])).unwrap(),
            Options {
                count: 2,
                interval_ms: 0,
                out: Some("-".to_string())
            }
        );
        let error = parse_args(&strs(&["--count", "twelve"])).unwrap_err();
        assert!(error.contains("--count"), "{error}");
    }
}
