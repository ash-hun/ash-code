//! ash-ipc — gRPC client (and re-exported server traits) for talking to
//! the `ashpy` Python sidecar.
//!
//! Generated types live under [`pb`]. A thin [`SidecarClient`] wrapper
//! exposes the handful of RPCs the Rust host actually needs in M1.

use std::time::Duration;

use anyhow::{Context, Result};

pub mod pb {
    //! Generated protobuf / tonic types for `ash.v1`.
    tonic::include_proto!("ash.v1");
}

pub const CRATE_NAME: &str = "ash-ipc";
pub const DEFAULT_SIDECAR_ENDPOINT: &str = "http://127.0.0.1:50051";
pub const CLIENT_IDENTITY: &str = concat!("ash-cli/", env!("CARGO_PKG_VERSION"));

/// Small convenience wrapper around the generated `HealthClient`.
///
/// More services (LlmProvider, Harness, …) get their own accessor methods
/// as later milestones implement them.
#[derive(Debug, Clone)]
pub struct SidecarClient {
    endpoint: String,
    channel: tonic::transport::Channel,
}

impl SidecarClient {
    /// Connect to the sidecar at `endpoint`, waiting up to `connect_timeout`.
    ///
    /// Per-RPC timeout is deliberately NOT set — streaming chat RPCs can run
    /// for minutes against real providers. Individual RPCs may still set
    /// their own `tonic::Request::set_timeout` where appropriate.
    pub async fn connect(endpoint: impl Into<String>, connect_timeout: Duration) -> Result<Self> {
        let endpoint = endpoint.into();
        let ch = tonic::transport::Endpoint::from_shared(endpoint.clone())
            .context("invalid sidecar endpoint")?
            .connect_timeout(connect_timeout)
            .connect()
            .await
            .with_context(|| format!("failed to connect to sidecar at {endpoint}"))?;
        Ok(Self {
            endpoint,
            channel: ch,
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Issues a `Health.Ping` RPC and returns the server's response.
    pub async fn ping(&self) -> Result<pb::PingResponse> {
        use pb::health_client::HealthClient;
        let now_ms = chrono_like_now_ms();
        let req = pb::PingRequest {
            client: CLIENT_IDENTITY.to_string(),
            sent_unix_ms: now_ms,
        };
        let mut client = HealthClient::new(self.channel.clone());
        let resp = client
            .ping(req)
            .await
            .context("Health.Ping RPC failed")?
            .into_inner();
        Ok(resp)
    }

    /// `LlmProvider.ListProviders` — enumerate every provider the sidecar knows about.
    pub async fn list_providers(&self) -> Result<Vec<pb::ProviderInfo>> {
        use pb::llm_provider_client::LlmProviderClient;
        let mut client = LlmProviderClient::new(self.channel.clone());
        let resp = client
            .list_providers(pb::ListProvidersRequest {})
            .await
            .context("LlmProvider.ListProviders RPC failed")?
            .into_inner();
        Ok(resp.providers)
    }

    /// `LlmProvider.Switch` — change the active provider / model.
    pub async fn switch_provider(
        &self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<pb::SwitchResponse> {
        use pb::llm_provider_client::LlmProviderClient;
        let mut client = LlmProviderClient::new(self.channel.clone());
        let resp = client
            .switch(pb::SwitchRequest {
                provider: provider.into(),
                model: model.into(),
            })
            .await
            .context("LlmProvider.Switch RPC failed")?
            .into_inner();
        Ok(resp)
    }

    /// `LlmProvider.ChatStream` — open a server-streaming chat session.
    pub async fn chat_stream(
        &self,
        request: pb::ChatRequest,
    ) -> Result<tonic::Streaming<pb::ChatDelta>> {
        use pb::llm_provider_client::LlmProviderClient;
        let mut client = LlmProviderClient::new(self.channel.clone());
        let stream = client
            .chat_stream(request)
            .await
            .context("LlmProvider.ChatStream RPC failed")?
            .into_inner();
        Ok(stream)
    }

    // --- Harness hooks (M3) ------------------------------------------------

    pub async fn on_turn_start(&self, ctx: pb::TurnContext) -> Result<pb::HookDecision> {
        use pb::harness_client::HarnessClient;
        let mut client = HarnessClient::new(self.channel.clone());
        Ok(client
            .on_turn_start(ctx)
            .await
            .context("Harness.OnTurnStart RPC failed")?
            .into_inner())
    }

    pub async fn on_tool_call(&self, event: pb::ToolCallEvent) -> Result<pb::HookDecision> {
        use pb::harness_client::HarnessClient;
        let mut client = HarnessClient::new(self.channel.clone());
        Ok(client
            .on_tool_call(event)
            .await
            .context("Harness.OnToolCall RPC failed")?
            .into_inner())
    }

    pub async fn on_stream_delta(&self, event: pb::DeltaEvent) -> Result<()> {
        use pb::harness_client::HarnessClient;
        let mut client = HarnessClient::new(self.channel.clone());
        let _ = client
            .on_stream_delta(event)
            .await
            .context("Harness.OnStreamDelta RPC failed")?;
        Ok(())
    }

    // --- SkillRegistry / CommandRegistry (M5/M6) ----------------------------

    pub async fn list_skills(&self) -> Result<Vec<pb::Skill>> {
        use pb::skill_registry_client::SkillRegistryClient;
        let mut client = SkillRegistryClient::new(self.channel.clone());
        let resp = client
            .list(pb::ListSkillsRequest {})
            .await
            .context("SkillRegistry.List RPC failed")?
            .into_inner();
        Ok(resp.skills)
    }

    pub async fn invoke_skill(
        &self,
        name: &str,
        args: std::collections::HashMap<String, String>,
    ) -> Result<pb::InvokeSkillResponse> {
        use pb::skill_registry_client::SkillRegistryClient;
        let mut client = SkillRegistryClient::new(self.channel.clone());
        let resp = client
            .invoke(pb::InvokeSkillRequest {
                name: name.to_string(),
                args,
                context: std::collections::HashMap::new(),
            })
            .await
            .context("SkillRegistry.Invoke RPC failed")?
            .into_inner();
        Ok(resp)
    }

    pub async fn render_command(
        &self,
        name: &str,
        args: std::collections::HashMap<String, String>,
    ) -> Result<pb::RunCommandResponse> {
        use pb::command_registry_client::CommandRegistryClient;
        let mut client = CommandRegistryClient::new(self.channel.clone());
        let resp = client
            .run(pb::RunCommandRequest {
                name: name.to_string(),
                args,
                context: std::collections::HashMap::new(),
            })
            .await
            .context("CommandRegistry.Run RPC failed")?
            .into_inner();
        Ok(resp)
    }

    pub async fn list_commands(&self) -> Result<Vec<pb::Command>> {
        use pb::command_registry_client::CommandRegistryClient;
        let mut client = CommandRegistryClient::new(self.channel.clone());
        let resp = client
            .list(pb::ListCommandsRequest {})
            .await
            .context("CommandRegistry.List RPC failed")?
            .into_inner();
        Ok(resp.commands)
    }

    pub async fn on_turn_end(&self, result: pb::TurnResult) -> Result<()> {
        use pb::harness_client::HarnessClient;
        let mut client = HarnessClient::new(self.channel.clone());
        client
            .on_turn_end(result)
            .await
            .context("Harness.OnTurnEnd RPC failed")?;
        Ok(())
    }
}

/// Tiny dependency-free milliseconds-since-epoch helper so that `ash-ipc`
/// does not pull in `chrono` just for a timestamp.
fn chrono_like_now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_endpoint_loopback() {
        assert!(DEFAULT_SIDECAR_ENDPOINT.starts_with("http://127.0.0.1"));
    }

    #[test]
    fn generated_types_present() {
        // Smoke-test that tonic-build produced the expected symbols.
        let req = pb::PingRequest {
            client: "test".to_string(),
            sent_unix_ms: 0,
        };
        assert_eq!(req.client, "test");
    }

    /// End-to-end in-process test: spin up a minimal `Health` server,
    /// connect `SidecarClient` to it, and verify the ping round-trip.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_ping_roundtrip() -> Result<()> {
        use pb::health_server::{Health, HealthServer};
        use std::net::SocketAddr;
        use tokio::net::TcpListener;
        use tokio_stream::wrappers::TcpListenerStream;

        #[derive(Default)]
        struct TestHealth;

        #[tonic::async_trait]
        impl Health for TestHealth {
            async fn ping(
                &self,
                request: tonic::Request<pb::PingRequest>,
            ) -> Result<tonic::Response<pb::PingResponse>, tonic::Status> {
                let inner = request.into_inner();
                let mut features = std::collections::HashMap::new();
                features.insert("health".to_string(), "v1".to_string());
                Ok(tonic::Response::new(pb::PingResponse {
                    server: "test-server/0.0.1".to_string(),
                    api_version: "v1".to_string(),
                    received_unix_ms: inner.sent_unix_ms + 1,
                    features,
                }))
            }
        }

        // Bind an ephemeral port.
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr: SocketAddr = listener.local_addr()?;
        let incoming = TcpListenerStream::new(listener);

        // Run the server in the background.
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(HealthServer::new(TestHealth))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });

        // Connect and ping.
        let endpoint = format!("http://{addr}");
        let client = SidecarClient::connect(endpoint, Duration::from_secs(2)).await?;
        let resp = client.ping().await?;
        assert_eq!(resp.server, "test-server/0.0.1");
        assert_eq!(resp.api_version, "v1");
        assert!(resp.features.contains_key("health"));

        server.abort();
        Ok(())
    }
}
