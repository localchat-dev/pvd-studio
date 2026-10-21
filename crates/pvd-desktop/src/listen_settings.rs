use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Deserializer, Serialize};

use crate::i18n::Message;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListenSettings {
    #[serde(rename = "listen_host")]
    pub host: String,
    #[serde(rename = "listen_port")]
    pub port: u16,
}

impl Default for ListenSettings {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".into(),
            port: 5425,
        }
    }
}

impl<'de> Deserialize<'de> for ListenSettings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Stored {
            listen_host: Option<String>,
            listen_port: Option<u16>,
            listen_address: Option<String>,
        }
        let stored = Stored::deserialize(deserializer)?;
        if let Some(host) = stored.listen_host {
            return Ok(Self {
                host,
                port: stored.listen_port.unwrap_or(5425),
            });
        }
        if let Some(legacy) = stored.listen_address {
            if legacy == "127.0.0.1:5425" {
                return Ok(Self::default());
            }
            return Ok(match legacy.parse::<SocketAddr>() {
                Ok(address) => Self {
                    host: match address {
                        SocketAddr::V6(address) if address.scope_id() != 0 => {
                            format!("{}%{}", address.ip(), address.scope_id())
                        }
                        _ => address.ip().to_string(),
                    },
                    port: address.port(),
                },
                Err(_) => Self {
                    host: legacy,
                    port: stored.listen_port.unwrap_or(5425),
                },
            });
        }
        Ok(Self {
            port: stored.listen_port.unwrap_or(5425),
            ..Self::default()
        })
    }
}

impl ListenSettings {
    pub fn address(&self) -> Result<SocketAddr, Message> {
        if self.port == 0 {
            return Err(Message::new(
                "Listen port must be between 1 and 65535.",
                "监听端口必须在 1 至 65535 之间。",
            ));
        }
        let host = self.host.trim();
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        if host.contains(':')
            && let Ok(address) = format!("[{host}]:{}", self.port).parse::<SocketAddr>()
        {
            return Ok(address);
        }
        let ip = if host == "*" {
            Ok(IpAddr::from([0, 0, 0, 0]))
        } else {
            host.parse::<IpAddr>()
        };
        let ip = ip.map_err(|_| Message::new("Enter a local IPv4 or IPv6 address; use 0.0.0.0 for all IPv4 interfaces. Enter the port separately.", "请输入本机 IPv4 或 IPv6 地址；0.0.0.0 表示所有 IPv4 网卡。端口请单独填写。"))?;
        Ok(SocketAddr::new(ip, self.port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_defaults_migrate_while_custom_bindings_round_trip() {
        assert_eq!(
            serde_json::from_str::<ListenSettings>("{}").unwrap(),
            ListenSettings::default()
        );
        assert_eq!(
            serde_json::from_str::<ListenSettings>(r#"{"listen_address":"127.0.0.1:5425"}"#)
                .unwrap(),
            ListenSettings::default()
        );
        for legacy in [
            "127.0.0.1:15425",
            "192.168.96.147:5435",
            "[::1]:5425",
            "[::]:5435",
            "[fe80::1234%12]:5435",
        ] {
            let json = serde_json::json!({"listen_address":legacy});
            let settings: ListenSettings = serde_json::from_value(json).unwrap();
            assert_eq!(
                settings.address().unwrap(),
                legacy.parse::<SocketAddr>().unwrap()
            );
            let saved = serde_json::to_string(&settings).unwrap();
            assert!(!saved.contains("listen_address"));
            assert_eq!(
                serde_json::from_str::<ListenSettings>(&saved).unwrap(),
                settings
            );
        }
        assert_eq!(
            serde_json::from_str::<ListenSettings>(
                r#"{"listen_host":"127.0.0.1","listen_port":5425}"#
            )
            .unwrap()
            .host,
            "127.0.0.1"
        );
    }

    #[test]
    fn address_and_port_validation_never_silently_binds_a_different_interface() {
        for host in ["", "192.168.1.1:5425", "server.example", "256.1.2.3"] {
            assert!(
                ListenSettings {
                    host: host.into(),
                    port: 5435
                }
                .address()
                .is_err()
            );
        }
        assert_eq!(
            ListenSettings {
                host: " [::] ".into(),
                port: 5435
            }
            .address()
            .unwrap(),
            "[::]:5435".parse().unwrap()
        );
        assert_eq!(
            ListenSettings {
                host: "*".into(),
                port: 5435
            }
            .address()
            .unwrap(),
            "0.0.0.0:5435".parse().unwrap()
        );
        assert_eq!(
            ListenSettings {
                host: "fe80::1234%12".into(),
                port: 5435
            }
            .address()
            .unwrap(),
            "[fe80::1234%12]:5435".parse().unwrap()
        );
        assert!(
            ListenSettings {
                host: "0.0.0.0".into(),
                port: 0
            }
            .address()
            .is_err()
        );
    }
}
