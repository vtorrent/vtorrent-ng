use vtorrent_p2p::message::{NetMessage, MAX_PAYLOAD_SIZE};

use crate::error::{NodeError, Result};

pub(crate) fn encode_overlay_message(msg: &NetMessage) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16 + msg.payload.len());
    bytes.extend_from_slice(&msg.command);
    bytes.extend_from_slice(&(msg.payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&msg.payload);
    bytes
}

pub(crate) fn decode_overlay_message(bytes: &[u8]) -> Result<NetMessage> {
    if bytes.len() < 16 {
        return Err(NodeError::Chain(
            "Overlay message is shorter than its envelope".into(),
        ));
    }

    let mut command = [0u8; 12];
    command.copy_from_slice(&bytes[..12]);
    let command_len = command.iter().position(|byte| *byte == 0).unwrap_or(12);
    std::str::from_utf8(&command[..command_len])
        .map_err(|_| NodeError::Chain("Overlay message command is not UTF-8".into()))?;
    if command_len == 0 || command[command_len..].iter().any(|byte| *byte != 0) {
        return Err(NodeError::Chain(
            "Overlay message command is malformed".into(),
        ));
    }

    let payload_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    if payload_len > MAX_PAYLOAD_SIZE as usize || bytes.len() != 16 + payload_len {
        return Err(NodeError::Chain(
            "Overlay message payload length is invalid".into(),
        ));
    }

    Ok(NetMessage {
        command,
        payload: bytes[16..].to_vec(),
    })
}

pub(crate) fn overlay_peer_addr(node_id: &str) -> Result<std::net::SocketAddr> {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    let bytes = hex::decode(node_id)
        .map_err(|_| NodeError::Chain("Overlay node ID is not hexadecimal".into()))?;
    if bytes.len() != 32 {
        return Err(NodeError::Chain("Overlay node ID must be 32 bytes".into()));
    }
    let port = 1_024 + (u16::from_le_bytes([bytes[2], bytes[3]]) % (u16::MAX - 1_024));
    Ok(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(198, 18 + (bytes[0] & 1), bytes[1], bytes[4])),
        port,
    ))
}
