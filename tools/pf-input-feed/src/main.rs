use evdev::{AttributeSet, KeyCode, uinput::VirtualDevice};
use pf_input_feed::{CONTRACT_KEYS, encoded_press, focus_walk};
use std::{
    env, fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

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

fn parse_pid(args: &[String], flag: &str) -> Result<Option<u32>, String> {
    value(args, flag)
        .map(|raw| raw.parse().map_err(|error| format!("{flag}: {error}")))
        .transpose()
}

fn process_consumes(node: &Path, pid: u32) -> bool {
    fs::read_dir(format!("/proc/{pid}/fd")).is_ok_and(|fds| {
        fds.flatten()
            .any(|fd| fs::read_link(fd.path()).is_ok_and(|target| target == node))
    })
}

fn consumed(node: &Path, consumer_pid: Option<u32>) -> bool {
    if let Some(pid) = consumer_pid {
        return process_consumes(node, pid);
    }
    let Ok(processes) = fs::read_dir("/proc") else {
        return false;
    };
    processes
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .filter_map(|process| process.file_name().to_string_lossy().parse().ok())
        .any(|pid| process_consumes(node, pid))
}

fn main() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let count = parse_usize(&args, "--count", 120)?;
    let interval_ms = parse_usize(&args, "--interval-ms", 150)?;
    let consumer_pid = parse_pid(&args, "--consumer-pid")?;
    if consumer_pid.is_none() {
        eprintln!(
            "warning: --consumer-pid was not provided; any process holding the input node may satisfy readiness and early events may be lost"
        );
    }
    let sequence = value(&args, "--sequence").unwrap_or_else(|| "focus-walk".into());
    if sequence != "focus-walk" {
        return Err(format!("unknown sequence: {sequence}"));
    }

    let mut keys = AttributeSet::<KeyCode>::new();
    for code in CONTRACT_KEYS {
        keys.insert(KeyCode(code));
    }
    let mut device = VirtualDevice::builder()
        .map_err(|e| e.to_string())?
        .name("PocketForge static action feeder")
        .with_keys(&keys)
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    let node = device
        .enumerate_dev_nodes_blocking()
        .map_err(|e| e.to_string())?
        .find_map(Result::ok)
        .ok_or("uinput device node did not appear")?;
    let wait_started = Instant::now();
    while !consumed(&node, consumer_pid) {
        if wait_started.elapsed() >= Duration::from_secs(15) {
            return Err(format!(
                "timed out waiting for {} to be consumed",
                node.display()
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }

    let started = Instant::now();
    for code in focus_walk(count) {
        let [down, up] = encoded_press(code);
        device.emit(&[down]).map_err(|e| e.to_string())?;
        device.emit(&[up]).map_err(|e| e.to_string())?;
        thread::sleep(Duration::from_millis(
            u64::try_from(interval_ms).unwrap_or(u64::MAX),
        ));
    }
    println!("fed={count} wall_ms={}", started.elapsed().as_millis());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
    };

    #[test]
    fn pid_scoped_wait_ignores_a_non_target_holder() {
        let path = env::temp_dir().join(format!(
            "pf-input-feed-consumer-test-{}",
            std::process::id()
        ));
        fs::write(&path, b"fixture").unwrap();
        let mut holder = Command::new("sh")
            .args([
                "-c",
                "exec 3<\"$1\"; echo ready; sleep 30",
                "pf-input-feed-test",
            ])
            .arg(&path)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(holder.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready");
        let mut target = Command::new("sleep").arg("30").spawn().unwrap();

        assert!(process_consumes(&path, holder.id()));
        assert!(!consumed(&path, Some(target.id())));
        assert!(consumed(&path, None));

        holder.kill().unwrap();
        target.kill().unwrap();
        holder.wait().unwrap();
        target.wait().unwrap();
        fs::remove_file(path).unwrap();
    }
}
