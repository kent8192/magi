//! Shared integration-test support.
//!
//! Provides an [`rstest`] fixture, [`redis_fixture`], backed by a single Redis
//! server that is provisioned in a Docker container via [`testcontainers`].
//!
//! Each integration-test binary starts exactly one container — the first time
//! `redis_fixture` is requested — and shares it across every test in that
//! binary. Tests isolate themselves with unique key prefixes (`unique_name` in
//! each test file), which is the same isolation model the suite used when it
//! ran against an externally supplied `MAGI_TEST_REDIS_URL`. Sharing one
//! long-lived container (rather than one per test) avoids the rapid
//! create/destroy churn that makes Docker Desktop's host port-forward refuse
//! new connections. Docker must be available for these tests to run.

// Not every test binary that includes this module uses every item.
#![allow(dead_code)]

use std::time::Duration;

use rstest::fixture;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::redis::Redis;
use tokio::sync::OnceCell;

/// Redis image tag used for the test container.
///
/// `testcontainers-modules` defaults the standalone Redis image to `5.0`, but
/// magi's inbox cursor uses exclusive `XRANGE` bounds (`(<id>`), which Redis
/// only supports from 6.2 onward. Pin to a 7.x image so the test server matches
/// the Redis versions magi actually targets.
const REDIS_TAG: &str = "7-alpine";

/// Process-wide Redis container handle plus its client URL, started once per
/// test binary and shared by every test in that binary.
struct SharedRedis {
    // Kept alive for the lifetime of the test process; the testcontainers
    // reaper removes the container once the process exits. Never read directly.
    _container: ContainerAsync<Redis>,
    url: String,
}

static SHARED: OnceCell<SharedRedis> = OnceCell::const_new();

/// Lazily starts (or returns the already-started) shared Redis container.
async fn shared() -> &'static SharedRedis {
    SHARED
        .get_or_init(|| async {
            let container = Redis::default()
                .with_tag(REDIS_TAG)
                .start()
                .await
                .expect("start redis testcontainer");
            let host = container.get_host().await.expect("redis container host");
            let port = container
                .get_host_port_ipv4(6379_u16)
                .await
                .expect("redis container mapped port");
            let url = format!("redis://{host}:{port}");
            wait_until_reachable(&url).await;
            SharedRedis {
                _container: container,
                url,
            }
        })
        .await
}

/// Probes the mapped port until Redis accepts a connection and answers `PING`.
///
/// `start()` only guarantees the server is ready *inside* the container; on
/// Docker Desktop the host-side port-forward can briefly refuse connections
/// right after start, so confirm host reachability before any test runs.
async fn wait_until_reachable(url: &str) {
    let client = redis::Client::open(url).expect("redis client");
    for _ in 0..100 {
        if let Ok(mut connection) = client.get_multiplexed_async_connection().await {
            let pong: redis::RedisResult<String> =
                redis::cmd("PING").query_async(&mut connection).await;
            if pong.is_ok() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("redis container at {url} did not become reachable");
}

/// A handle to the shared test Redis server.
pub struct RedisFixture {
    url: String,
}

impl RedisFixture {
    /// Returns the `redis://host:port` URL of the shared test Redis server.
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// rstest fixture exposing the shared per-binary Redis server (see module docs).
///
/// Use it from an async test as:
///
/// ```ignore
/// #[rstest]
/// #[tokio::test]
/// async fn my_test(#[future(awt)] redis_fixture: RedisFixture) {
///     let url = redis_fixture.url();
///     // ... exercise the URL against a live Redis ...
/// }
/// ```
#[fixture]
pub async fn redis_fixture() -> RedisFixture {
    RedisFixture {
        url: shared().await.url.clone(),
    }
}
