//! Desk-Remote session helper executable entry point (Active user session).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = desk_logger::LoggerConfig {
        file_name_prefix: "desk-session-helper".to_string(),
        ..Default::default()
    };
    let _guard = desk_logger::init(&config)?;
    tracing::info!("Desk-Remote session helper skeleton initialized");
    Ok(())
}
