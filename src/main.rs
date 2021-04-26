use std::error::Error;

use log::LevelFilter;
use simple_logger::SimpleLogger;

#[actix_web::main]
async fn main() -> Result<(), Box<dyn Error>> {
  SimpleLogger::new()
    .with_level(LevelFilter::Warn)
    .with_module_level("appcues_incr", LevelFilter::Debug)
    .init()?;

  appcues_incr::start_server().await
}
