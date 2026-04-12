use opencrust_common::{Error, Result};
use serde::Deserialize;
use std::collections::HashMap;

/// Payload interpretation mode for the MQTT channel.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum MqttMode {
    /// Every payload is treated as plain-text.  One session per channel.
    Simple,
    /// Payload must be JSON `{"user_id":"…","text":"…"}`.  One session per user_id.
    Multi,
    /// Auto-detect: JSON with both `user_id` and `text` → Multi, otherwise → Simple.
    #[default]
    Auto,
}

/// Typed configuration for one MQTT channel, extracted from the generic
/// `ChannelConfig::settings` map.
#[derive(Debug, Clone)]
pub struct MqttConfig {
    /// Config key name (e.g. `"mqtt-home"`).  Used as fallback `client_id` and session prefix.
    pub channel_name: String,
    /// e.g. `"mqtt://localhost"` or `"mqtts://broker.example.com"`.
    pub broker_url: String,
    /// TCP port of the broker (e.g. `1883`).
    pub port: u16,
    /// Topic this channel subscribes to for incoming messages.
    pub subscribe_topic: String,
    /// Base topic for publishing replies.
    pub publish_topic: String,
    /// MQTT client identifier.  Defaults to `channel_name`.
    pub client_id: String,
    /// MQTT QoS level (0 / 1 / 2).  Defaults to `1`.
    pub qos: rumqttc::QoS,
    /// Optional broker username.
    pub username: Option<String>,
    /// Optional broker password.
    pub password: Option<String>,
    /// Payload mode.  Defaults to `Auto`.
    pub mode: MqttMode,
}

// ── Raw serde helper ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RawMqttConfig {
    broker_url: Option<String>,
    port: Option<u16>,
    subscribe_topic: Option<String>,
    publish_topic: Option<String>,
    client_id: Option<String>,
    #[serde(default = "default_qos")]
    qos: u8,
    username: Option<String>,
    password: Option<String>,
    #[serde(default)]
    mode: String,
}

fn default_qos() -> u8 {
    1
}

// ── MqttConfig impl ───────────────────────────────────────────────────────────

impl MqttConfig {
    /// Build a validated `MqttConfig` from the flat settings map stored in
    /// `opencrust_config::ChannelConfig::settings`.
    pub fn from_settings(
        channel_name: &str,
        settings: &HashMap<String, serde_json::Value>,
    ) -> Result<Self> {
        let value = serde_json::Value::Object(
            settings
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );

        let raw: RawMqttConfig = serde_json::from_value(value)
            .map_err(|e| Error::Config(format!("invalid mqtt config: {e}")))?;

        let broker_url = raw
            .broker_url
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| Error::Config("mqtt broker_url is required".into()))?;

        let port = raw
            .port
            .ok_or_else(|| Error::Config("mqtt port is required".into()))?;

        let subscribe_topic = raw
            .subscribe_topic
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| Error::Config("mqtt subscribe_topic is required".into()))?;

        let publish_topic = raw
            .publish_topic
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| Error::Config("mqtt publish_topic is required".into()))?;

        let qos = match raw.qos {
            0 => rumqttc::QoS::AtMostOnce,
            1 => rumqttc::QoS::AtLeastOnce,
            2 => rumqttc::QoS::ExactlyOnce,
            n => {
                return Err(Error::Config(format!(
                    "mqtt qos must be 0, 1, or 2 — got {n}"
                )));
            }
        };

        let mode = match raw.mode.to_lowercase().as_str() {
            "simple" => MqttMode::Simple,
            "multi" => MqttMode::Multi,
            _ => MqttMode::Auto,
        };

        let client_id = raw
            .client_id
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| channel_name.to_string());

        Ok(Self {
            channel_name: channel_name.to_string(),
            broker_url,
            port,
            subscribe_topic,
            publish_topic,
            client_id,
            qos,
            username: raw.username.filter(|s| !s.is_empty()),
            password: raw.password.filter(|s| !s.is_empty()),
            mode,
        })
    }

    /// Parse the host from `broker_url` (strips `mqtt://` / `mqtts://` scheme).
    pub fn host(&self) -> &str {
        self.broker_url
            .trim_start_matches("mqtts://")
            .trim_start_matches("mqtt://")
            .split(':')
            .next()
            .unwrap_or("localhost")
    }

    /// Returns `true` when TLS should be used (`mqtts://` scheme).
    pub fn use_tls(&self) -> bool {
        self.broker_url.starts_with("mqtts://")
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn minimal() -> HashMap<String, serde_json::Value> {
        settings(&[
            ("broker_url", serde_json::json!("mqtt://localhost")),
            ("port", serde_json::json!(1883_u16)),
            ("subscribe_topic", serde_json::json!("in")),
            ("publish_topic", serde_json::json!("out")),
        ])
    }

    #[test]
    fn minimal_config_parses() {
        let cfg = MqttConfig::from_settings("ch", &minimal()).unwrap();
        assert_eq!(cfg.broker_url, "mqtt://localhost");
        assert_eq!(cfg.port, 1883);
        assert_eq!(cfg.subscribe_topic, "in");
        assert_eq!(cfg.publish_topic, "out");
        assert_eq!(cfg.client_id, "ch"); // fallback to channel name
        assert_eq!(cfg.qos, rumqttc::QoS::AtLeastOnce); // default 1
        assert_eq!(cfg.mode, MqttMode::Auto); // default
    }

    #[test]
    fn missing_broker_url_fails() {
        let mut s = minimal();
        s.remove("broker_url");
        let err = MqttConfig::from_settings("ch", &s).unwrap_err();
        assert!(err.to_string().contains("broker_url"));
    }

    #[test]
    fn missing_port_fails() {
        let mut s = minimal();
        s.remove("port");
        let err = MqttConfig::from_settings("ch", &s).unwrap_err();
        assert!(err.to_string().contains("port"));
    }

    #[test]
    fn invalid_qos_fails() {
        let mut s = minimal();
        s.insert("qos".into(), serde_json::json!(5_u8));
        let err = MqttConfig::from_settings("ch", &s).unwrap_err();
        assert!(err.to_string().contains("qos"));
    }

    #[test]
    fn mode_simple_parses() {
        let mut s = minimal();
        s.insert("mode".into(), serde_json::json!("simple"));
        let cfg = MqttConfig::from_settings("ch", &s).unwrap();
        assert_eq!(cfg.mode, MqttMode::Simple);
    }

    #[test]
    fn mode_multi_parses() {
        let mut s = minimal();
        s.insert("mode".into(), serde_json::json!("multi"));
        let cfg = MqttConfig::from_settings("ch", &s).unwrap();
        assert_eq!(cfg.mode, MqttMode::Multi);
    }

    #[test]
    fn unknown_mode_defaults_to_auto() {
        let mut s = minimal();
        s.insert("mode".into(), serde_json::json!("unknown"));
        let cfg = MqttConfig::from_settings("ch", &s).unwrap();
        assert_eq!(cfg.mode, MqttMode::Auto);
    }

    #[test]
    fn host_strips_scheme() {
        let mut s = minimal();
        s.insert(
            "broker_url".into(),
            serde_json::json!("mqtts://broker.example.com"),
        );
        let cfg = MqttConfig::from_settings("ch", &s).unwrap();
        assert_eq!(cfg.host(), "broker.example.com");
        assert!(cfg.use_tls());
    }
}
