//! The harness's one timing primitive.

use std::time::Duration;

use anyhow::{bail, Result};
use tokio::time::sleep;

/// Polls `attempt` every 500 ms until it yields `Some`, or fails after ~20 s.
pub async fn poll_until<T, Fut>(mut attempt: impl FnMut() -> Fut, description: &str) -> Result<T>
where
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..40 {
        if let Some(value) = attempt().await {
            return Ok(value);
        }
        sleep(Duration::from_millis(500)).await;
    }
    bail!("timed out {description}")
}

#[cfg(test)]
mod tests {
    use super::poll_until;

    #[tokio::test]
    async fn returns_the_first_some_and_gives_up_on_a_never() {
        let mut attempts = 0;
        let value = poll_until(
            || {
                attempts += 1;
                async move { (attempts > 1).then_some(attempts) }
            },
            "a value on the second attempt",
        )
        .await
        .expect("the second attempt yields a value");
        assert_eq!(value, 2);

        // A predicate that never matches must fail rather than hang; the
        // deadline is ~20 s, so this case is checked with a tiny timeout.
        let timed_out = tokio::time::timeout(
            std::time::Duration::from_millis(1200),
            poll_until(|| async { None::<()> }, "something that never happens"),
        )
        .await;
        assert!(
            timed_out.is_err(),
            "poll_until must keep polling until its own deadline"
        );
    }
}
