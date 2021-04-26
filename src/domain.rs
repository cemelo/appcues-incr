use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

/// A command sent by the client.
#[derive(Serialize, Deserialize, Debug)]
pub struct Command {
  pub key: String,
  pub value: i64,
}

/// Buffered state of the application.
pub struct AppState {
  /// Map used to store unflushed data.
  pub cache: RwLock<HashMap<String, i64>>,

  /// Sender used to signal termination to background tasks.
  pub shutdown: broadcast::Sender<()>,
}

impl AppState {
  /// Creats a new `AppState` instance.
  pub fn new() -> Self {
    AppState {
      cache: Default::default(),
      shutdown: tokio::sync::broadcast::channel::<()>(10).0,
    }
  }

  /// Atomically applies a command into the buffered state.
  pub async fn increment(&self, command: Command) {
    let mut writable_ref = self.cache.write().await;

    let current_entry = writable_ref.entry(command.key.clone()).or_insert(0i64);
    *current_entry += command.value;
  }

  /// Collects all buffered commands in a [vector](std::vec::Vec) and clears the buffer.
  pub async fn collect_and_clear(&self) -> Vec<Command> {
    let mut writable_ref = self.cache.write().await;

    let mut collected_commands = Vec::with_capacity(writable_ref.len());

    writable_ref
      .iter_mut()
      .filter(|v| *((*v).1) != 0i64)
      .for_each(|e| {
        collected_commands.push(Command {
          key: e.0.clone(),
          value: *(e.1),
        });

        (*e.1) = 0;
      });

    collected_commands
  }
}

#[cfg(test)]
mod test {
  use std::error::Error;
  use std::sync::Arc;

  use crate::{AppState, Command};

  #[tokio::test]
  async fn should_properly_increment_the_counter() -> Result<(), Box<dyn Error>> {
    let state = AppState::new();

    for _ in 0..10_000 {
      state.increment(Command {
        key: "test".into(),
        value: 1,
      }).await;
    }

    assert_eq!(*state.cache.read().await.get("test").unwrap(), 10_000);

    Ok(())
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
  async fn should_properly_increment_the_counter_in_parallel() -> Result<(), Box<dyn Error>> {
    let state = Arc::new(AppState::new());

    let task1 = tokio::spawn({
      let state = state.clone();
      async move {
        for _ in 0..1_000_000 {
          state.increment(Command {
            key: "test".into(),
            value: 1,
          }).await;
        }
      }
    });

    let task2 = tokio::spawn({
      let state = state.clone();
      async move {
        for _ in 0..1_000_000 {
          state.increment(Command {
            key: "test".into(),
            value: -1,
          }).await;
        }
      }
    });

    task1.await?;
    task2.await?;

    assert_eq!(*state.cache.read().await.get("test").unwrap(), 0);

    Ok(())
  }
}
