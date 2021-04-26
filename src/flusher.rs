use std::error::Error;
use std::sync::Arc;

use log::{debug, info};
use sqlx::postgres::PgQueryResult;
use sqlx::{Pool, Postgres};
use tokio::task::JoinHandle;
use tokio::time::Duration;

use crate::domain::{AppState, Command};
use crate::MAX_KEYS_PER_FLUSH;

/// A background task responsible for collecting and persisting the instance buffer.
pub struct Flusher {
  /// Shared application state.
  state: Arc<AppState>,

  /// Database connection pool.
  db_pool: Pool<Postgres>,

  /// Interval between flushes to the database.
  interval: Duration,
}

impl Flusher {
  /// Creates a new flusher.
  ///
  /// # Arguments
  ///
  /// * `state`: the shared application state.
  /// * `db_pool`: the database connection pool.
  /// * `interval`: the interval between flushes to the database.
  pub fn new(state: Arc<AppState>, db_pool: Pool<Postgres>, interval: Duration) -> Self {
    Flusher {
      state,
      db_pool,
      interval,
    }
  }

  /// Starts the flusher task in the thread pool and returns a handle used to notify when the task
  /// is finalized.
  pub async fn start(self) -> JoinHandle<()> {
    let mut signal_receiver = self.state.shutdown.subscribe();

    let handle = tokio::spawn(async move {
      let mut shutdown = false;
      let mut interval = tokio::time::interval(self.interval);

      while !shutdown {
        tokio::select! {
          _ = interval.tick() => {}

          _ = signal_receiver.recv() => {
            info!("Terminating background task. Flushing the current buffer.");
            shutdown = true;
          }
        }

        self.flush().await;
      }
    });

    handle
  }

  /// Aggregates the buffered state into commands and persists them in the database.
  async fn flush(&self) {
    debug!("Flushing current buffer.");

    let state = self.state.clone();
    let commands = state.collect_and_clear().await;
    // Retries until the operation succeeds.
    while !self.persist(&commands).await.is_ok() {}
  }

  /// Stores the buffer in the database.
  async fn persist(&self, buffer: &[Command]) -> Result<u64, Box<dyn Error>> {
    let mut keys_updated = 0;

    // The items to be stored are split in chunks to workaround PostgreSQL 16-bit binding id limit.
    for buffer_chunk in buffer.chunks(MAX_KEYS_PER_FLUSH) {
      let keys = buffer_chunk.iter().map(|c| c.key.clone()).collect::<Vec<String>>();
      let increments = buffer_chunk.iter().map(|c| c.value).collect::<Vec<i64>>();

      let affected_rows = sqlx::query(
        r#"
          INSERT INTO snapshots (key, value)
          SELECT k.key, v.value AS increment
          FROM UNNEST($1::VARCHAR[]) WITH ORDINALITY AS k(key, id)
          INNER JOIN UNNEST($2::INT8[]) WITH ORDINALITY AS v(value, id) ON k.id = v.id
          ON CONFLICT (key) DO UPDATE SET value = excluded.value + snapshots.value
        "#,
      )
      .bind(&keys)
      .bind(&increments)
      .execute(&self.db_pool)
      .await
      .map(|r: PgQueryResult| r.rows_affected())?;

      keys_updated += affected_rows;
    }

    info!("{} keys updated.", keys_updated);

    Ok(keys_updated)
  }
}

#[cfg(test)]
mod test {
  use std::error::Error;
  use std::future::Future;
  use std::sync::Arc;
  use std::time::Duration;

  use sqlx::postgres::PgPoolOptions;
  use sqlx::{Pool, Postgres};

  use crate::flusher::Flusher;
  use crate::{AppState, Command};

  #[tokio::test]
  async fn should_atomically_persist_increments() -> Result<(), Box<dyn Error>> {
    run_test(|pool| async move {
      let mut commands = Vec::new();
      commands.push(Command {
        key: "test-persistence".into(),
        value: 1,
      });

      let flusher = Flusher::new(Arc::new(AppState::new()), pool.clone(), Duration::from_millis(10));
      let keys_affected = flusher.persist(&commands).await?;

      assert_eq!(keys_affected, 1);

      let (snapshot,): (i64,) = sqlx::query_as("SELECT value FROM snapshots WHERE key = $1")
        .bind("test-persistence")
        .fetch_one(&pool)
        .await?;

      sqlx::query("DELETE FROM snapshots WHERE key = $1")
        .bind("test-persistence")
        .execute(&pool)
        .await?;

      assert_eq!(snapshot, 1);

      Ok(())
    })
    .await?;

    Ok(())
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 5)]
  async fn should_correctly_flush_the_counter_into_the_db() -> Result<(), Box<dyn Error>> {
    run_test(|pool| async move {
      let state = Arc::new(AppState::new());
      let flusher = Flusher::new(state.clone(), pool.clone(), Duration::from_secs(10));

      let flusher_task = flusher.start().await;

      for _ in 0..1_000_000 {
        state
          .increment(Command {
            key: "test-single-thread".into(),
            value: 1,
          })
          .await;
      }

      // Sends shutdown signal.
      state.shutdown.send(())?;

      // Awaits for the flusher to terminate.
      flusher_task.await?;

      let (snapshot,): (i64,) = sqlx::query_as("SELECT value FROM snapshots WHERE key = $1")
        .bind("test-single-thread")
        .fetch_one(&pool)
        .await?;

      sqlx::query("DELETE FROM snapshots WHERE key = $1")
        .bind("test-single-thread")
        .execute(&pool)
        .await?;

      assert_eq!(snapshot, 1_000_000);

      Ok(())
    })
    .await?;

    Ok(())
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 10)]
  async fn should_support_concurrent_states_in_same_database() -> Result<(), Box<dyn Error>> {
    run_test(|pool| async move {
      let mut states = Vec::new();
      let mut handles = Vec::new();

      for _ in 0..4usize {
        let state = Arc::new(AppState::new());
        let flusher = Flusher::new(state.clone(), pool.clone(), Duration::from_micros(1));

        states.push(state);
        handles.push(flusher.start().await);
      }

      let mut state_handles = Vec::new();

      for s in &states {
        let s = s.clone();
        state_handles.push(tokio::spawn(async move {
          for _ in 0..1_000_000 {
            s.increment(Command {
              key: "test-multi-thread".into(),
              value: 1,
            })
            .await
          }
        }));
      }

      // Await for all commands to be sent.
      futures::future::join_all(state_handles).await;

      // Sends shutdown signals.
      for s in &states {
        s.shutdown.send(())?;
      }

      // Awaits for all the flushers to terminate.
      futures::future::join_all(handles).await;

      let (snapshot,): (i64,) = sqlx::query_as("SELECT value FROM snapshots WHERE key = $1")
        .bind("test-multi-thread")
        .fetch_one(&pool)
        .await?;

      sqlx::query("DELETE FROM snapshots WHERE key = $1")
        .bind("test-multi-thread")
        .execute(&pool)
        .await?;

      assert_eq!(snapshot, 1_000_000 * (states.len() as i64));

      Ok(())
    })
    .await?;

    Ok(())
  }

  async fn run_test<T: Future<Output = Result<(), Box<dyn Error>>>>(
    test: fn(Pool<Postgres>) -> T,
  ) -> Result<(), Box<dyn Error>> {
    let db_pool = PgPoolOptions::new()
      .max_connections(5)
      .connect("postgres://postgres:password@localhost:5433/postgres")
      .await?;

    sqlx::migrate!().run(&db_pool).await?;

    test(db_pool).await?;

    Ok(())
  }
}
