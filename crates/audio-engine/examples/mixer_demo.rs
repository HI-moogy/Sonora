use std::time::Duration;

use audio_engine::{ComGuard, Mixer};

/// Usage: mixer_demo <pid1> [pid2 ...] [--seconds N]
/// Adds each pid to the mixer on the "Game" bus at full volume and mixes
/// them to the default output device for N seconds (default 10).
fn main() -> windows::core::Result<()> {
    let _com = ComGuard::init()?;

    let mut pids = Vec::new();
    let mut seconds = 10u64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--seconds" {
            seconds = args.next().expect("value for --seconds").parse().unwrap();
        } else {
            pids.push(arg.parse::<u32>().expect("pid must be a number"));
        }
    }
    if pids.is_empty() {
        eprintln!("usage: mixer_demo <pid1> [pid2 ...] [--seconds N]");
        std::process::exit(1);
    }

    let mixer = Mixer::start()?;
    let mut ids = Vec::new();
    for pid in &pids {
        let id = mixer.add_source(*pid, format!("pid-{pid}"), "Game")?;
        ids.push(id);
        println!("added pid {pid} as source {id} on bus Game");
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(seconds);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        let levels: Vec<String> = mixer
            .sources()
            .iter()
            .map(|s| format!("{}={:.3}", s.name, s.level))
            .collect();
        println!(
            "master={:.3}  {}",
            mixer.master_level(),
            levels.join("  ")
        );
    }

    for id in ids {
        mixer.remove_source(id);
    }
    println!("done");
    Ok(())
}
