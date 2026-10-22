use audio_engine::{enumerate_sessions, ComGuard};

fn main() -> windows::core::Result<()> {
    let _com = ComGuard::init()?;
    let sessions = enumerate_sessions()?;
    if sessions.is_empty() {
        println!("No active audio sessions on the default render device.");
    }
    for session in sessions {
        println!(
            "pid={:<7} system_sounds={:<5} name={}",
            session.process_id, session.is_system_sounds, session.display_name
        );
    }
    Ok(())
}
