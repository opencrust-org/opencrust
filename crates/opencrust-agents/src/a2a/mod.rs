pub mod client;
pub mod model;

pub use client::A2AClient;
pub use model::{
    A2AArtifact, A2AMessage, A2APart, A2ATask, AgentCapabilities, AgentCard, AgentSkill,
    CreateTaskRequest, TaskStatus,
};
