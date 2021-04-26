use std::error::Error;
use std::sync::Arc;

use actix_web::{post, web, App, HttpServer};
use log::{info, warn};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Pool, Postgres};
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::Duration;

pub use crate::domain::{AppState, Command};
pub use crate::flusher::Flusher;

mod domain;
mod flusher;
mod settings;

/// Maximum number of keys affected per flush operation in the database. This is necessary because
/// PostgreSQL has a limit of 65535 bindings per query (non-function).
///
/// Reference: https://www.postgresql.org/docs/12/protocol-message-formats.html (Bind)
const MAX_KEYS_PER_FLUSH: usize = 32767;

// Interval of 9.5 seconds. Since the state can only be outdated for **at most** 10 seconds,
// this should give enough room for small delays caused by the task scheduler.
pub const FLUSHER_INTERVAL: u64 = 9500;

pub async fn start_server() -> Result<(), Box<dyn Error>> {
  let settings = settings::Settings::new()?;

  info!("Starting Postgres connection pool.");
  let db_pool = PgPoolOptions::new()
    .max_connections(5)
    .connect(&settings.database_url)
    .await?;

  sqlx::migrate!().run(&db_pool).await?;

  let app_state = Arc::new(AppState::new());

  tokio::spawn(create_signal_handler(app_state.clone()));
  tokio::spawn(create_flusher(app_state.clone(), db_pool));

  {
    let app_state = app_state.clone();
    HttpServer::new(move || {
      App::new()
        .app_data(web::Data::from(app_state.clone()))
        .service(increment_service)
    })
    .bind(("0.0.0.0", 3333))?
    .run()
    .await?;
  }

  // Sends a termination to all the long running tasks.
  app_state.shutdown.send(())?;

  Ok(())
}

/// Defines the `POST /increment` route.
///
/// Stores each command in memory until they're flushed by a [`Flusher`] task.
#[post("/increment")]
async fn increment_service(state: web::Data<AppState>, command: web::Json<Command>) -> &'static str {
  state.increment(command.into_inner()).await;
  "OK"
}

/// Creates a flusher task that runs in
async fn create_flusher(state: Arc<AppState>, db_pool: Pool<Postgres>) {
  let flusher = Flusher::new(state, db_pool, Duration::from_millis(FLUSHER_INTERVAL));

  info!("Starting flusher task.");
  flusher.start().await;
  info!("Flusher terminated.");
}

/// Handles SIGINT and notify all long running tasks that they should terminate.
async fn create_signal_handler(state: Arc<AppState>) {
  if let Ok(mut stream) = signal(SignalKind::interrupt()) {
    stream.recv().await;
    if let Err(_) = state.shutdown.send(()) {
      warn!("Could not send shutdown notification.")
    }
  } else {
    warn!("Could not start signal handler.")
  }
}
