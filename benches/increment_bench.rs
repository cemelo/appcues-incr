use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use appcues_incr::{AppState, Command};
use std::time::Duration;

/// Benchmarks how fast the server is able to handle an increment request.
fn benchmark_server(c: &mut Criterion) {
  std::env::set_var(
    "APPCUES_DATABASE_URL",
    "postgres://postgres:password@localhost:5433/postgres",
  );

  let mut server_group = c.benchmark_group("server");
  server_group.significance_level(0.1).sample_size(500);
  server_group.noise_threshold(0.05);

  // Starts the server in the background
  std::thread::spawn(move || {
    actix_web::rt::System::new()
      .block_on(appcues_incr::start_server())
      .unwrap();
  });

  let client = reqwest::blocking::ClientBuilder::new().build().unwrap();

  // Sleeps for 3 seconds to allow the server to start and connect to Postgres
  std::thread::sleep(Duration::from_secs(3));

  server_group.bench_function("request", move |b| {
    b.iter(|| {
      client
        .post("http://localhost:3333/increment")
        .json(&appcues_incr::Command {
          key: "bench-test".into(),
          value: 1,
        })
        .send()
        .unwrap()
    });
  });

  server_group.finish();
}

/// Benchmarks the map and read-write lock implementations.
fn benchmark_map(c: &mut Criterion) {
  let state = Arc::new(AppState::new());

  let mut increment_group = c.benchmark_group("increment");
  increment_group.significance_level(0.1).sample_size(500);
  increment_group.noise_threshold(0.05);

  {
    // This group benchmarks how fast a single-key map is able to update its value.

    let state = state.clone();
    increment_group.bench_function("single-key", move |b| {
      let mut iteration = 0;
      b.iter(|| {
        // This calls itoa, which has a non negligible overhead. Since we use this in the multi-key
        // benchmark, it's important to have the same overhead in the single-key bench to ensure a fair
        // comparison baseline.
        black_box(iteration.to_string());

        black_box(futures::executor::block_on(state.increment(Command {
          key: "test".into(),
          value: 1,
        })));

        iteration += 1;
      });
    });
  }

  {
    // This group benchmarks how fast a map is able to grow.

    let state = state.clone();
    increment_group.bench_function("multi-key", move |b| {
      let mut iteration = 0;
      b.iter(|| {
        black_box(futures::executor::block_on(state.increment(Command {
          key: iteration.to_string(),
          value: 1,
        })));

        iteration += 1;
      });
    });
  }

  {
    // This group benchmarks how fast a map with a lot of keys is able to update its values.

    let state = state.clone();
    increment_group.bench_function("multi-key-updates", move |b| {
      let mut iteration = 0;
      b.iter(|| {
        black_box(futures::executor::block_on(state.increment(Command {
          key: (iteration % 1000).to_string(),
          value: 1,
        })));

        iteration += 1;
      });
    });
  }

  increment_group.finish();
}

fn benchmark(c: &mut Criterion) {
  benchmark_map(c);
  benchmark_server(c);
}

criterion_group!(benches, benchmark);
criterion_main!(benches);
