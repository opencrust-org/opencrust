use opencrust_common::{Error, Result};
use reqwest::Client;

use super::model::{A2ATask, AgentCard, CreateTaskRequest};

/// Client for communicating with remote A2A-compatible agents.
pub struct A2AClient {
    http: Client,
}

impl Default for A2AClient {
    fn default() -> Self {
        Self::new()
    }
}

impl A2AClient {
    pub fn new() -> Self {
        Self {
            http: Client::new(),
        }
    }

    /// Fetch the agent card from a remote agent's well-known endpoint.
    pub async fn fetch_agent_card(&self, base_url: &str) -> Result<AgentCard> {
        let url = format!("{}/.well-known/agent.json", base_url.trim_end_matches('/'));
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("failed to fetch agent card from {url}: {e}")))?;

        if !resp.status().is_success() {
            return Err(Error::Agent(format!(
                "agent card request failed with status {}",
                resp.status()
            )));
        }

        resp.json::<AgentCard>()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse agent card: {e}")))
    }

    /// Create a task on a remote A2A agent.
    pub async fn create_task(
        &self,
        base_url: &str,
        request: &CreateTaskRequest,
    ) -> Result<A2ATask> {
        let url = format!("{}/a2a/tasks", base_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .json(request)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("failed to create task on {url}: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Agent(format!(
                "create task failed with status {status}: {body}"
            )));
        }

        resp.json::<A2ATask>()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse task response: {e}")))
    }

    /// Get the status of a task on a remote A2A agent.
    pub async fn get_task(&self, base_url: &str, task_id: &str) -> Result<A2ATask> {
        let url = format!("{}/a2a/tasks/{}", base_url.trim_end_matches('/'), task_id);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("failed to get task from {url}: {e}")))?;

        if !resp.status().is_success() {
            return Err(Error::Agent(format!(
                "get task failed with status {}",
                resp.status()
            )));
        }

        resp.json::<A2ATask>()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse task response: {e}")))
    }

    /// Cancel a task on a remote A2A agent.
    pub async fn cancel_task(&self, base_url: &str, task_id: &str) -> Result<A2ATask> {
        let url = format!(
            "{}/a2a/tasks/{}/cancel",
            base_url.trim_end_matches('/'),
            task_id
        );
        let resp = self
            .http
            .post(&url)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("failed to cancel task on {url}: {e}")))?;

        if !resp.status().is_success() {
            return Err(Error::Agent(format!(
                "cancel task failed with status {}",
                resp.status()
            )));
        }

        resp.json::<A2ATask>()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse task response: {e}")))
    }
}
