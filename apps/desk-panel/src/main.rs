//! Desk-Remote operator desktop management panel entry point.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = desk_logger::LoggerConfig {
        file_name_prefix: "desk-panel".to_string(),
        ..Default::default()
    };
    let _guard = desk_logger::init(&config)?;
    tracing::info!("Desk-Remote operator panel skeleton initialized");
    Ok(())
}
