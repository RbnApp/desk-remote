//! Desk-Remote Windows Service daemon entry point (Session 0).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = desk_logger::LoggerConfig {
        file_name_prefix: "desk-service".to_string(),
        ..Default::default()
    };
    let _guard = desk_logger::init(&config)?;
    tracing::info!("Desk-Remote service skeleton initialized");
    Ok(())
}
