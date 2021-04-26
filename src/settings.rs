use config::{ConfigError, Config, File, Environment};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Settings {
  pub database_url: String,
}

impl Settings {
  pub fn new() -> Result<Self, ConfigError> {
    let mut settings = Config::new();

    // Loads the settings from a file
    settings.merge(File::with_name("settings"))?;

    // Merges environment configuration using the APPCUES prefix, e.g., APPCUES_DATABASE_URL
    settings.merge(Environment::with_prefix("appcues"))?;

    // Freezes the configuration
    settings.try_into()
  }
}