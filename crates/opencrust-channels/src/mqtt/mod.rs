pub mod config;
pub mod detect;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, TlsConfiguration, Transport};
use tokio::sync::{Mutex, mpsc, watch};
use tracing::{info, warn};

use crate::traits::{ChannelLifecycle, ChannelResponse, ChannelSender, ChannelStatus};
use config::{MqttConfig, MqttMode};
use detect::{DetectedMessage, detect};
use opencrust_common::{Message, MessageContent, Result};

// ── Callback type ─────────────────────────────────────────────────────────────

/// Callback invoked when an MQTT message is received.
///
/// Arguments: `(user_id, session_id, text, delta_tx)`.
/// * `delta_tx` is always `None` — MQTT does not support message streaming edits.
///
/// Return `Err("__blocked__")` to silently drop the message.
pub type MqttOnMessageFn = Arc<
    dyn Fn(
            String,
            String,
            String,
            Option<mpsc::Sender<String>>,
        )
            -> Pin<Box<dyn Future<Output = std::result::Result<ChannelResponse, String>> + Send>>
        + Send
        + Sync,
>;

// ── MqttChannel ───────────────────────────────────────────────────────────────

/// MQTT channel — subscribes to a broker topic and publishes replies.
pub struct MqttChannel {
    config: MqttConfig,
    display: String,
    status: ChannelStatus,
    on_message: MqttOnMessageFn,
    /// Shared handle to the live `AsyncClient`.  `None` when disconnected.
    client_slot: Arc<Mutex<Option<AsyncClient>>>,
    shutdown_tx: Option<watch::Sender<bool>>,
}

impl MqttChannel {
    pub fn new(config: MqttConfig, on_message: MqttOnMessageFn) -> Self {
        let display = format!("MQTT({})", config.channel_name);
        Self {
            config,
            display,
            status: ChannelStatus::Disconnected,
            on_message,
            client_slot: Arc::new(Mutex::new(None)),
            shutdown_tx: None,
        }
    }
}

// ── MqttSender ────────────────────────────────────────────────────────────────

/// Lightweight send-only handle.  Shares the `AsyncClient` with the event loop.
pub struct MqttSender {
    channel_name: String,
    publish_topic: String,
    client_slot: Arc<Mutex<Option<AsyncClient>>>,
    qos: rumqttc::QoS,
}

#[async_trait]
impl ChannelSender for MqttSender {
    fn channel_type(&self) -> &str {
        "mqtt"
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        let topic = message
            .metadata
            .get("mqtt_reply_topic")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.publish_topic);

        let text = match &message.content {
            MessageContent::Text(t) => t.clone(),
            _ => {
                return Err(opencrust_common::Error::Channel(
                    "only text messages are supported for mqtt send".into(),
                ));
            }
        };

        let client = {
            let guard = self.client_slot.lock().await;
            guard.clone()
        };

        if let Some(client) = client {
            client
                .publish(topic, self.qos, false, text.as_bytes())
                .await
                .map_err(|e| {
                    opencrust_common::Error::Channel(format!("mqtt publish failed: {e}"))
                })?;
        } else {
            return Err(opencrust_common::Error::Channel(format!(
                "mqtt channel '{}' is not connected",
                self.channel_name
            )));
        }

        Ok(())
    }
}

// ── ChannelLifecycle ──────────────────────────────────────────────────────────

#[async_trait]
impl ChannelLifecycle for MqttChannel {
    fn display_name(&self) -> &str {
        &self.display
    }

    fn create_sender(&self) -> Box<dyn ChannelSender> {
        Box::new(MqttSender {
            channel_name: self.config.channel_name.clone(),
            publish_topic: self.config.publish_topic.clone(),
            client_slot: Arc::clone(&self.client_slot),
            qos: self.config.qos,
        })
    }

    async fn connect(&mut self) -> Result<()> {
        let config = self.config.clone();
        let on_message = Arc::clone(&self.on_message);
        let client_slot = Arc::clone(&self.client_slot);

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        self.shutdown_tx = Some(shutdown_tx);

        tokio::spawn(async move {
            run_mqtt_loop(config, on_message, client_slot, shutdown_rx).await;
        });

        self.status = ChannelStatus::Connecting;
        info!("mqtt channel '{}' connecting...", self.config.channel_name);
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        // Best-effort disconnect
        let client = {
            let guard = self.client_slot.lock().await;
            guard.clone()
        };
        if let Some(client) = client {
            let _ = client.disconnect().await;
        }
        self.status = ChannelStatus::Disconnected;
        info!("mqtt channel '{}' disconnected", self.config.channel_name);
        Ok(())
    }

    fn status(&self) -> ChannelStatus {
        self.status.clone()
    }
}

#[async_trait]
impl ChannelSender for MqttChannel {
    fn channel_type(&self) -> &str {
        "mqtt"
    }

    async fn send_message(&self, message: &Message) -> Result<()> {
        let sender = MqttSender {
            channel_name: self.config.channel_name.clone(),
            publish_topic: self.config.publish_topic.clone(),
            client_slot: Arc::clone(&self.client_slot),
            qos: self.config.qos,
        };
        sender.send_message(message).await
    }
}

// ── Exponential backoff ───────────────────────────────────────────────────────

struct Backoff {
    current: Duration,
    max: Duration,
}

impl Backoff {
    fn new() -> Self {
        Self {
            current: Duration::from_secs(1),
            max: Duration::from_secs(120),
        }
    }

    fn reset(&mut self) {
        self.current = Duration::from_secs(1);
    }

    fn next(&mut self) -> Duration {
        let d = self.current;
        self.current = (self.current * 2).min(self.max);
        d
    }

    async fn wait(&mut self, shutdown_rx: &mut watch::Receiver<bool>) {
        let delay = self.next();
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown_rx.changed() => {}
        }
    }
}

// ── Event loop ────────────────────────────────────────────────────────────────

async fn run_mqtt_loop(
    config: MqttConfig,
    on_message: MqttOnMessageFn,
    client_slot: Arc<Mutex<Option<AsyncClient>>>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut backoff = Backoff::new();

    loop {
        if *shutdown_rx.borrow() {
            return;
        }

        // Build connection options
        let host = config.host().to_string();
        let mut opts = MqttOptions::new(&config.client_id, &host, config.port);
        opts.set_keep_alive(Duration::from_secs(30));
        opts.set_clean_session(true);

        if let (Some(u), Some(p)) = (&config.username, &config.password) {
            opts.set_credentials(u, p);
        }

        if config.use_tls() {
            opts.set_transport(Transport::tls_with_config(TlsConfiguration::Native));
        }

        let (client, mut event_loop) = AsyncClient::new(opts, 64);

        // Store client so MqttSender can publish
        {
            let mut guard = client_slot.lock().await;
            *guard = Some(client.clone());
        }

        // Subscribe
        if let Err(e) = client.subscribe(&config.subscribe_topic, config.qos).await {
            warn!(
                "mqtt '{}': subscribe failed: {e}, retrying...",
                config.channel_name
            );
            {
                let mut guard = client_slot.lock().await;
                *guard = None;
            }
            backoff.wait(&mut shutdown_rx).await;
            continue;
        }

        info!(
            "mqtt '{}': connected to {} — subscribed to '{}'",
            config.channel_name, config.broker_url, config.subscribe_topic
        );

        // Drive the event loop
        let reconnect = drive_event_loop(
            &config,
            &on_message,
            &client,
            &mut event_loop,
            &mut shutdown_rx,
        )
        .await;

        // Clear client slot
        {
            let mut guard = client_slot.lock().await;
            *guard = None;
        }

        if !reconnect {
            return; // shutdown requested
        }

        warn!(
            "mqtt '{}': disconnected, reconnecting in {:?}...",
            config.channel_name, backoff.current
        );
        backoff.wait(&mut shutdown_rx).await;
        backoff.reset();
    }
}

/// Returns `true` if the loop exited due to a connection error (should reconnect),
/// `false` if a shutdown was requested.
async fn drive_event_loop(
    config: &MqttConfig,
    on_message: &MqttOnMessageFn,
    client: &AsyncClient,
    event_loop: &mut EventLoop,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> bool {
    loop {
        tokio::select! {
            event = event_loop.poll() => {
                match event {
                    Ok(Event::Incoming(Packet::Publish(publish))) => {
                        handle_publish(config, on_message, client, publish).await;
                    }
                    Ok(Event::Incoming(Packet::ConnAck(_))) => {
                        info!("mqtt '{}': broker connection acknowledged", config.channel_name);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("mqtt '{}': connection error: {e}", config.channel_name);
                        return true; // reconnect
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    let _ = client.disconnect().await;
                    return false; // shutdown
                }
            }
        }
    }
}

/// Dispatch a single incoming PUBLISH packet to the on_message callback.
async fn handle_publish(
    config: &MqttConfig,
    on_message: &MqttOnMessageFn,
    client: &AsyncClient,
    publish: rumqttc::Publish,
) {
    let detected = detect(&publish.payload);

    let (user_id, session_id, text, reply_topic) = match &config.mode {
        MqttMode::Simple => {
            let text = match detected {
                DetectedMessage::Simple { text } => text,
                DetectedMessage::Multi(m) => m.text,
            };
            (
                config.channel_name.clone(),
                format!("mqtt-{}", config.channel_name),
                text,
                config.publish_topic.clone(),
            )
        }
        MqttMode::Multi => match detected {
            DetectedMessage::Multi(m) => {
                let session = m
                    .session_id
                    .unwrap_or_else(|| format!("mqtt-{}-{}", config.channel_name, m.user_id));
                let reply = format!("{}/{}", config.publish_topic, m.user_id);
                (m.user_id, session, m.text, reply)
            }
            DetectedMessage::Simple { text } => {
                warn!(
                    "mqtt '{}': multi mode but payload is not JSON with user_id+text, dropping",
                    config.channel_name
                );
                let _ = text;
                return;
            }
        },
        MqttMode::Auto => match detected {
            DetectedMessage::Simple { text } => (
                config.channel_name.clone(),
                format!("mqtt-{}", config.channel_name),
                text,
                config.publish_topic.clone(),
            ),
            DetectedMessage::Multi(m) => {
                let session = m
                    .session_id
                    .unwrap_or_else(|| format!("mqtt-{}-{}", config.channel_name, m.user_id));
                let reply = format!("{}/{}", config.publish_topic, m.user_id);
                (m.user_id, session, m.text, reply)
            }
        },
    };

    if text.trim().is_empty() {
        return;
    }

    // Spawn so the event loop is not blocked during processing
    let on_message = Arc::clone(on_message);
    let client = client.clone();
    let qos = config.qos;
    let channel_name = config.channel_name.clone();

    tokio::spawn(async move {
        match on_message(user_id, session_id, text, None).await {
            Ok(response) => {
                let reply_text = response.text().to_string();
                if !reply_text.is_empty()
                    && let Err(e) = client
                        .publish(&reply_topic, qos, false, reply_text.as_bytes())
                        .await
                {
                    warn!("mqtt '{channel_name}': publish reply failed: {e}");
                }
            }
            Err(e) if e == "__blocked__" => {
                // Silently dropped — unauthorized
            }
            Err(e) => {
                warn!("mqtt '{channel_name}': on_message error: {e}");
            }
        }
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use config::MqttMode;

    fn make_config(mode: MqttMode) -> MqttConfig {
        MqttConfig {
            channel_name: "test".into(),
            broker_url: "mqtt://localhost".into(),
            port: 1883,
            subscribe_topic: "in".into(),
            publish_topic: "out".into(),
            client_id: "test".into(),
            qos: rumqttc::QoS::AtLeastOnce,
            username: None,
            password: None,
            mode,
        }
    }

    fn noop_callback() -> MqttOnMessageFn {
        Arc::new(|_u, _s, _t, _d| Box::pin(async { Ok(ChannelResponse::Text("ok".into())) }))
    }

    #[test]
    fn initial_status_is_disconnected() {
        let ch = MqttChannel::new(make_config(MqttMode::Auto), noop_callback());
        assert!(matches!(ch.status(), ChannelStatus::Disconnected));
    }

    #[test]
    fn channel_type_is_mqtt() {
        let ch = MqttChannel::new(make_config(MqttMode::Auto), noop_callback());
        assert_eq!(ch.create_sender().channel_type(), "mqtt");
    }

    #[test]
    fn display_name_contains_channel_name() {
        let ch = MqttChannel::new(make_config(MqttMode::Auto), noop_callback());
        assert!(ch.display_name().contains("test"));
    }

    #[tokio::test]
    async fn disconnect_when_not_connected_is_noop() {
        let mut ch = MqttChannel::new(make_config(MqttMode::Auto), noop_callback());
        // Should not panic
        ch.disconnect().await.unwrap();
        assert!(matches!(ch.status(), ChannelStatus::Disconnected));
    }

    #[tokio::test]
    async fn on_message_receives_correct_ids_in_simple_mode() {
        let received = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let received_clone = Arc::clone(&received);

        let on_message: MqttOnMessageFn = Arc::new(move |user_id, session_id, _text, _d| {
            let rec = Arc::clone(&received_clone);
            Box::pin(async move {
                rec.lock().await.push((user_id, session_id));
                Ok(ChannelResponse::Text("ok".into()))
            })
        });

        let config = make_config(MqttMode::Simple);
        let publish = rumqttc::Publish::new("in", rumqttc::QoS::AtLeastOnce, "hello");
        let (client, _el) = AsyncClient::new(MqttOptions::new("t", "localhost", 1883), 10);

        handle_publish(&config, &on_message, &client, publish).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let rec = received.lock().await;
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].0, "test"); // user_id = channel_name
        assert_eq!(rec[0].1, "mqtt-test"); // session_id
    }

    #[tokio::test]
    async fn on_message_receives_correct_ids_in_multi_mode() {
        let received = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let received_clone = Arc::clone(&received);

        let on_message: MqttOnMessageFn = Arc::new(move |user_id, session_id, _text, _d| {
            let rec = Arc::clone(&received_clone);
            Box::pin(async move {
                rec.lock().await.push((user_id, session_id));
                Ok(ChannelResponse::Text("ok".into()))
            })
        });

        let config = make_config(MqttMode::Multi);
        let payload = br#"{"user_id":"pi-01","text":"temperature?"}"#;
        let publish = rumqttc::Publish::new("in", rumqttc::QoS::AtLeastOnce, payload.as_ref());
        let (client, _el) = AsyncClient::new(MqttOptions::new("t", "localhost", 1883), 10);

        handle_publish(&config, &on_message, &client, publish).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let rec = received.lock().await;
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].0, "pi-01");
        assert_eq!(rec[0].1, "mqtt-test-pi-01");
    }
}
