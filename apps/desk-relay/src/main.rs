//! Desk-Remote standalone signaling and TURN relay daemon entry point.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = desk_logger::LoggerConfig {
        file_name_prefix: "desk-relay".to_string(),
        ..Default::default()
    };
    let _guard = desk_logger::init(&config)?;
    tracing::info!("Desk-Remote relay server skeleton initialized");
    Ok(())
}
