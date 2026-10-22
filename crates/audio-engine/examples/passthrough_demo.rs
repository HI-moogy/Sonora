use std::time::Duration;

use audio_engine::{enumerate_sessions, run_passthrough, ComGuard};

/// Usage: passthrough_demo <pid> [seconds]
/// Lists active sessions if no pid is given.
fn main() -> windows::core::Result<()> {
    let _com = ComGuard::init()?;

    let mut args = std::env::args().skip(1);
    let Some(pid_arg) = args.next() else {
        println!("Active audio sessions (pass a pid to route it to your speakers):");
        for session in enumerate_sessions()? {
            println!(
                "  pid={:<7} name={}",
                session.process_id, session.display_name
            );
        }
        return Ok(());
    };

    let pid: u32 = pid_arg.parse().expect("pid must be a number");
    let seconds: u64 = args
        .next()
        .map(|s| s.parse().expect("seconds must be a number"))
        .unwrap_or(10);

    println!("Routing pid {pid}'s audio to the default output device for {seconds}s...");
    run_passthrough(pid, Duration::from_secs(seconds))?;
    println!("Done.");
    Ok(())
}
