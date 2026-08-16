//! The `Script` type — a sequence of opcodes and push data.

use crate::error::{Result, ScriptError};
use serde::{Deserialize, Serialize};

/// Maximum script size in bytes (Bitcoin Script limit).
pub const MAX_SCRIPT_SIZE: usize = 10_000;

/// Maximum push data item size in bytes.
pub const MAX_PUSH_SIZE: usize = 520;

/// A script — a sequence of opcodes and push-data items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Script(pub Vec<u8>);

impl Script {
    /// Create a new empty script.
    pub fn new() -> Self {
        Self(vec![])
    }

    /// Create a script from raw bytes.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() > MAX_SCRIPT_SIZE {
            return Err(ScriptError::ScriptTooLarge(bytes.len()));
        }
        Ok(Self(bytes))
    }

    /// Returns the raw bytes of the script.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns the length of the script in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Push a raw byte slice onto the script (with the appropriate push opcode).
    pub fn push_data(&mut self, data: &[u8]) -> Result<()> {
        if data.len() > MAX_PUSH_SIZE {
            return Err(ScriptError::PushTooLarge(data.len()));
        }
        match data.len() {
            0 => self.0.push(0x00), // OP_0
            1..=75 => {
                self.0.push(data.len() as u8);
                self.0.extend_from_slice(data);
            }
            76..=255 => {
                self.0.push(0x4c); // OP_PUSHDATA1
                self.0.push(data.len() as u8);
                self.0.extend_from_slice(data);
            }
            256..=65535 => {
                self.0.push(0x4d); // OP_PUSHDATA2
                self.0.extend_from_slice(&(data.len() as u16).to_le_bytes());
                self.0.extend_from_slice(data);
            }
            _ => {
                self.0.push(0x4e); // OP_PUSHDATA4
                self.0.extend_from_slice(&(data.len() as u32).to_le_bytes());
                self.0.extend_from_slice(data);
            }
        }
        Ok(())
    }

    /// Push a single opcode byte.
    pub fn push_opcode(&mut self, opcode: u8) {
        self.0.push(opcode);
    }

    /// Push a small integer (0–16) as the corresponding OP_N opcode.
    pub fn push_int(&mut self, n: u8) {
        match n {
            0 => self.0.push(0x00),
            1..=16 => self.0.push(0x50 + n),
            _ => {
                // Encode as minimally-encoded script integer
                self.0.push(1);
                self.0.push(n);
            }
        }
    }

    /// Iterate over the script's items (opcodes and push data).
    pub fn iter(&self) -> ScriptIter<'_> {
        ScriptIter {
            data: &self.0,
            pos: 0,
        }
    }
}

impl From<Vec<u8>> for Script {
    fn from(v: Vec<u8>) -> Self {
        Self(v)
    }
}

impl From<Script> for Vec<u8> {
    fn from(s: Script) -> Self {
        s.0
    }
}

/// An item yielded by the script iterator.
#[derive(Debug, Clone)]
pub enum ScriptItem<'a> {
    /// An opcode.
    Opcode(u8),
    /// A push data item.
    PushData(&'a [u8]),
}

/// Iterator over script items.
pub struct ScriptIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for ScriptIter<'a> {
    type Item = ScriptItem<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.data.len() {
            return None;
        }

        let byte = self.data[self.pos];
        self.pos += 1;

        match byte {
            0x00 => Some(ScriptItem::PushData(&[])),
            1..=75 => {
                let len = byte as usize;
                if self.pos + len > self.data.len() {
                    return None;
                }
                let data = &self.data[self.pos..self.pos + len];
                self.pos += len;
                Some(ScriptItem::PushData(data))
            }
            0x4c => {
                // OP_PUSHDATA1
                if self.pos >= self.data.len() {
                    return None;
                }
                let len = self.data[self.pos] as usize;
                self.pos += 1;
                if self.pos + len > self.data.len() {
                    return None;
                }
                let data = &self.data[self.pos..self.pos + len];
                self.pos += len;
                Some(ScriptItem::PushData(data))
            }
            0x4d => {
                // OP_PUSHDATA2
                if self.pos + 2 > self.data.len() {
                    return None;
                }
                let len =
                    u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]) as usize;
                self.pos += 2;
                if self.pos + len > self.data.len() {
                    return None;
                }
                let data = &self.data[self.pos..self.pos + len];
                self.pos += len;
                Some(ScriptItem::PushData(data))
            }
            0x4e => {
                // OP_PUSHDATA4
                if self.pos + 4 > self.data.len() {
                    return None;
                }
                let len = u32::from_le_bytes([
                    self.data[self.pos],
                    self.data[self.pos + 1],
                    self.data[self.pos + 2],
                    self.data[self.pos + 3],
                ]) as usize;
                self.pos += 4;
                if self.pos + len > self.data.len() {
                    return None;
                }
                let data = &self.data[self.pos..self.pos + len];
                self.pos += len;
                Some(ScriptItem::PushData(data))
            }
            _ => Some(ScriptItem::Opcode(byte)),
        }
    }
}
