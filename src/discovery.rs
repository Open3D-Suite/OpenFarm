// SPDX-License-Identifier: GPL-3.0-or-later
//! Finding farm servers on the LAN: a UDP broadcast that servers answer with their name and port.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const QUESTION: &[u8] = b"OPENFARM?";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Found {
    /// `ip:port` to put in the agent's settings.
    pub address: String,
    pub name: String,
    pub version: String,
}

/// Asks the LAN (and this computer) for farm servers and collects the answers for `wait`.
pub async fn discover(wait: Duration) -> Vec<Found> {
    let Ok(sock) = tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await else { return vec![] };
    let _ = sock.set_broadcast(true);
    let port = crate::config::DISCOVERY_PORT;
    let _ = sock.send_to(QUESTION, (Ipv4Addr::BROADCAST, port)).await;
    let _ = sock.send_to(QUESTION, (Ipv4Addr::LOCALHOST, port)).await;
    let mut out: Vec<Found> = Vec::new();
    let mut buf = [0u8; 1024];
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Ok((n, from))) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await {
        if let Some(f) = parse_answer(&buf[..n], from) {
            // The server's own computer answers on 127.0.0.1 and on the LAN: keep the LAN address.
            if let Some(i) = out.iter().position(|o| o.name == f.name) {
                if out[i].address.starts_with("127.") {
                    out[i] = f;
                }
            } else {
                out.push(f);
            }
        }
    }
    out
}

fn parse_answer(bytes: &[u8], from: SocketAddr) -> Option<Found> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    Some(Found {
        address: format!("{}:{}", from.ip(), v.get("port")?.as_u64()?),
        name: v.get("name")?.as_str()?.to_string(),
        version: v.get("openfarm")?.as_str()?.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_answer() {
        let from: SocketAddr = "192.168.1.10:47101".parse().unwrap();
        let f = parse_answer(br#"{"openfarm":"0.1.0","name":"Studio","port":47100}"#, from).unwrap();
        assert_eq!(f.address, "192.168.1.10:47100");
        assert_eq!(f.name, "Studio");
        assert!(parse_answer(b"nonsense", from).is_none());
    }
}
