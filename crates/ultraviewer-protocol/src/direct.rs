//! The native P2P byte envelope and a bounded raw TCP LAN accepting end for
//! Windows' active P2PTryConnect. TCP success and successful CBC decryption
//! are transport facts only; the owner must complete ordinary partner Login.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::crypto::LegacyAes;
use crate::frame::{self, FrameDecoder};
use crate::relay::{DirectPeerEnvelope, RelayIPv4Address};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirectError {
    #[error("the LAN listener is already open")]
    AlreadyOpen,
    #[error("the local address is not a LAN address")]
    InvalidLanAddress,
    #[error("invalid LAN encryption key")]
    InvalidKey,
    #[error("the LAN connection is closed")]
    Closed,
    #[error("the LAN connection is not connected")]
    NotConnected,
    #[error("LAN I/O failed: {0}")]
    Io(String),
}

/// AES framing for one direct peer.
#[derive(Clone)]
pub struct DirectPeerCodec {
    cipher: LegacyAes,
}

impl DirectPeerCodec {
    pub fn new(encryption_key: &str) -> Result<Self, DirectError> {
        if encryption_key.is_empty() {
            return Err(DirectError::InvalidKey);
        }
        Ok(Self { cipher: LegacyAes::new(encryption_key).map_err(|_| DirectError::InvalidKey)? })
    }

    pub fn encode(&self, envelope: &DirectPeerEnvelope) -> Result<Vec<u8>, DirectError> {
        frame::encode(&self.cipher.encrypt(&envelope.encode())).map_err(|_| DirectError::Closed)
    }

    /// Deterministic independent-fixture entry point, never used by the listener.
    pub fn encode_with_iv(&self, envelope: &DirectPeerEnvelope, iv: &[u8]) -> Result<Vec<u8>, DirectError> {
        let encrypted = self.cipher.encrypt_with_iv(&envelope.encode(), iv).map_err(|_| DirectError::InvalidKey)?;
        frame::encode(&encrypted).map_err(|_| DirectError::Closed)
    }

    pub fn decode_frame(&self, encrypted_payload: &[u8]) -> Option<DirectPeerEnvelope> {
        let plain = self.cipher.decrypt(encrypted_payload).ok()?;
        DirectPeerEnvelope::decode(&plain).ok()
    }
}

pub type CandidateId = u64;

#[derive(Debug)]
pub enum DirectEvent {
    /// A LAN socket was accepted; the owner decides whether to attach it.
    Accepted { id: CandidateId, stream: TcpStream, address: RelayIPv4Address },
    Message { id: CandidateId, envelope: DirectPeerEnvelope },
    Disconnected { id: CandidateId },
    ListenerEnded,
}

pub struct Advertisement {
    pub local_address: RelayIPv4Address,
    pub port: u16,
}

struct Client {
    writer: OwnedWriteHalf,
    reader: JoinHandle<()>,
}

type EventMap<T> = Arc<dyn Fn(DirectEvent) -> T + Send + Sync>;

/// Owned by the session actor; all events arrive through its inbox.
pub struct DirectTransport<T: Send + 'static> {
    codec: DirectPeerCodec,
    clients: HashMap<CandidateId, Client>,
    accept: Option<JoinHandle<()>>,
    sink: mpsc::Sender<T>,
    map: EventMap<T>,
    maximum_payload_length: usize,
}

impl<T: Send + 'static> DirectTransport<T> {
    const MAXIMUM_CLIENTS: usize = 4;

    /// Binds a listener on the LAN address and starts accepting candidates.
    pub async fn start<F>(local_address: RelayIPv4Address, encryption_key: &str, allow_loopback: bool,
                          sink: mpsc::Sender<T>, map: F) -> Result<(Self, Advertisement), DirectError>
    where
        F: Fn(DirectEvent) -> T + Send + Sync + 'static,
    {
        if !(local_address.is_lan() || (allow_loopback && local_address.octets()[0] == 127)) {
            return Err(DirectError::InvalidLanAddress);
        }
        let codec = DirectPeerCodec::new(encryption_key)?;
        let ip = std::net::Ipv4Addr::from(local_address.octets());
        let listener = tokio::time::timeout(Duration::from_secs(3), TcpListener::bind((ip, 0)))
            .await
            .map_err(|_| DirectError::Closed)?
            .map_err(|e| DirectError::Io(e.to_string()))?;
        let port = listener.local_addr().map_err(|e| DirectError::Io(e.to_string()))?.port();
        let map: EventMap<T> = Arc::new(map);
        let accept_sink = sink.clone();
        let accept_map = map.clone();
        let accept = tokio::spawn(async move {
            let mut next: CandidateId = 1;
            loop {
                let Ok((stream, peer)) = listener.accept().await else { break };
                let SocketAddr::V4(v4) = peer else { continue };
                let address = RelayIPv4Address::from_octets(v4.ip().octets());
                if !(address.is_lan() || (allow_loopback && address.octets()[0] == 127)) {
                    continue;
                }
                let _ = stream.set_nodelay(true);
                if accept_sink.send(accept_map(DirectEvent::Accepted { id: next, stream, address })).await.is_err() {
                    return;
                }
                next += 1;
            }
            let _ = accept_sink.send(accept_map(DirectEvent::ListenerEnded)).await;
        });
        Ok((
            Self { codec, clients: HashMap::new(), accept: Some(accept), sink, map, maximum_payload_length: frame::DEFAULT_MAXIMUM_PAYLOAD },
            Advertisement { local_address, port },
        ))
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// Registers an accepted socket and starts delivering its messages.
    pub fn attach(&mut self, id: CandidateId, stream: TcpStream) -> bool {
        if self.clients.len() >= Self::MAXIMUM_CLIENTS {
            return false;
        }
        let (mut read, writer) = stream.into_split();
        let codec = self.codec.clone();
        let maximum = self.maximum_payload_length;
        let sink = self.sink.clone();
        let map = self.map.clone();
        let reader = tokio::spawn(async move {
            let mut decoder = FrameDecoder::new(maximum);
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let count = match read.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                let Ok(frames) = decoder.append(&buffer[..count]) else { break };
                for encrypted in frames {
                    // A decryption failure closes this candidate only.
                    let Some(envelope) = codec.decode_frame(&encrypted) else {
                        let _ = sink.send(map(DirectEvent::Disconnected { id })).await;
                        return;
                    };
                    if sink.send(map(DirectEvent::Message { id, envelope })).await.is_err() {
                        return;
                    }
                }
            }
            let _ = sink.send(map(DirectEvent::Disconnected { id })).await;
        });
        self.clients.insert(id, Client { writer, reader });
        true
    }

    pub async fn send(&mut self, payload: &[u8], format_flag: bool, to: CandidateId) -> Result<(), DirectError> {
        let wire = self.codec.encode(&DirectPeerEnvelope { format_flag, payload: payload.to_vec() })?;
        let client = self.clients.get_mut(&to).ok_or(DirectError::NotConnected)?;
        match tokio::time::timeout(Duration::from_secs(20), client.writer.write_all(&wire)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(DirectError::Io(error.to_string())),
            Err(_) => Err(DirectError::Closed),
        }
    }

    pub fn close_candidate(&mut self, id: CandidateId) {
        if let Some(client) = self.clients.remove(&id) {
            client.reader.abort();
        }
    }

    /// Keep only the selected connection. The listener is no longer advertised
    /// after one direct channel passes application initialization.
    pub fn select_candidate(&mut self, id: CandidateId) {
        if !self.clients.contains_key(&id) {
            return;
        }
        self.stop_listening();
        let others: Vec<CandidateId> = self.clients.keys().copied().filter(|&c| c != id).collect();
        for other in others {
            self.close_candidate(other);
        }
    }

    /// Stops accepting new candidates without touching existing ones.
    pub fn stop_listening(&mut self) {
        if let Some(accept) = self.accept.take() {
            accept.abort();
        }
    }

    pub fn close(&mut self) {
        self.stop_listening();
        let ids: Vec<CandidateId> = self.clients.keys().copied().collect();
        for id in ids {
            self.close_candidate(id);
        }
    }
}

impl<T: Send + 'static> Drop for DirectTransport<T> {
    fn drop(&mut self) {
        self.close();
    }
}

/// Pure selection gate shared by the session and tests. A pending socket and
/// its decryptable packets cannot replace a healthy relay before Login+init0.
#[derive(Debug, Default, Clone)]
pub struct LanDirectUpgradeState {
    pub expected_address: Option<RelayIPv4Address>,
    pub candidates: HashMap<CandidateId, RelayIPv4Address>,
    pub login_candidate: Option<CandidateId>,
    pub login_sent: bool,
    pub selected: Option<CandidateId>,
}

impl LanDirectUpgradeState {
    pub fn peer_lan_address(public_address: &str, own_public_address: RelayIPv4Address, details: Option<&str>,
                            fallback_address: &str) -> Option<RelayIPv4Address> {
        if RelayIPv4Address::parse(public_address).ok()? != own_public_address {
            return None;
        }
        let address = match details {
            Some(details) if !details.is_empty() => {
                if details.len() > 64 {
                    return None;
                }
                let fields: Vec<&str> = details.split('|').collect();
                if fields.len() != 2 || fields[1].parse::<u16>().map_or(true, |p| p == 0) {
                    return None;
                }
                RelayIPv4Address::parse(fields[0]).ok()
            }
            _ => RelayIPv4Address::parse(fallback_address).ok(),
        }?;
        address.is_lan().then_some(address)
    }

    pub fn connected(&mut self, id: CandidateId, address: RelayIPv4Address) -> bool {
        if self.selected.is_some() || self.candidates.len() >= 4 || self.expected_address.is_some_and(|e| e != address) {
            return false;
        }
        self.candidates.insert(id, address);
        true
    }

    pub fn expect(&mut self, address: RelayIPv4Address) -> Vec<CandidateId> {
        self.expected_address = Some(address);
        let rejected: Vec<CandidateId> = self.candidates.iter().filter(|(_, a)| **a != address).map(|(id, _)| *id).collect();
        for id in &rejected {
            self.remove(*id);
        }
        rejected
    }

    pub fn begin_login(&mut self) -> Option<CandidateId> {
        if self.selected.is_some() || self.login_candidate.is_some() {
            return None;
        }
        let expected = self.expected_address?;
        let mut ids: Vec<CandidateId> = self.candidates.iter().filter(|(_, a)| **a == expected).map(|(id, _)| *id).collect();
        ids.sort_unstable();
        let candidate = *ids.first()?;
        self.login_candidate = Some(candidate);
        self.login_sent = false;
        Some(candidate)
    }

    pub fn mark_login_sent(&mut self, id: CandidateId) -> bool {
        if self.login_candidate != Some(id) || self.expected_address.is_none() || self.candidates.get(&id) != self.expected_address.as_ref() {
            return false;
        }
        self.login_sent = true;
        true
    }

    pub fn permits_initialization(&self, id: CandidateId) -> bool {
        self.login_sent
            && self.login_candidate == Some(id)
            && self.expected_address.is_some()
            && self.candidates.get(&id) == self.expected_address.as_ref()
    }

    pub fn select_initialized(&mut self, id: CandidateId) -> bool {
        if !self.permits_initialization(id) {
            return false;
        }
        self.selected = Some(id);
        let address = self.candidates[&id];
        self.candidates = HashMap::from([(id, address)]);
        true
    }

    pub fn remove(&mut self, id: CandidateId) -> bool {
        self.candidates.remove(&id);
        if self.login_candidate == Some(id) {
            self.login_candidate = None;
            self.login_sent = false;
        }
        let was_selected = self.selected == Some(id);
        if was_selected {
            self.selected = None;
        }
        was_selected
    }
}
