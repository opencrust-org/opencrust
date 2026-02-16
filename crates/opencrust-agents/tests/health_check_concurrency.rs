use async_trait::async_trait;
use opencrust_agents::{AgentRuntime, LlmProvider, LlmRequest, LlmResponse};
use opencrust_common::Result;
use std::time::{Duration, Instant};

struct MockProvider {
    id: String,
    delay: Duration,
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn provider_id(&self) -> &str {
        &self.id
    }

    async fn complete(&self, _request: &LlmRequest) -> Result<LlmResponse> {
        unimplemented!()
    }

    async fn health_check(&self) -> Result<bool> {
        tokio::time::sleep(self.delay).await;
        Ok(true)
    }
}

#[tokio::test]
async fn test_health_check_performance() {
    let mut runtime = AgentRuntime::new();
    let delay = Duration::from_millis(100);
    let count = 5;

    for i in 0..count {
        let provider = MockProvider {
            id: format!("mock-{}", i),
            delay,
        };
        runtime.register_provider(Box::new(provider));
    }

    let start = Instant::now();
    let results = runtime.health_check_all().await.unwrap();
    let elapsed = start.elapsed();

    println!("Elapsed: {:?}", elapsed);
    assert_eq!(results.len(), count);

    // For verification: check if execution is concurrent
    // It should take roughly the delay time, not sequential sum
    // We add a buffer for overhead (e.g., 2x delay is generous enough)
    let max_expected_duration = delay * 2;
    assert!(elapsed < max_expected_duration, "Execution was slower than expected for concurrent processing. Elapsed: {:?}, Expected less than: {:?}", elapsed, max_expected_duration);

    // Also ensure it took at least the delay
    assert!(elapsed >= delay, "Execution was faster than the delay itself!");
}
