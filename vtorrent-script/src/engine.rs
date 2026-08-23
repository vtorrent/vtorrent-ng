//! Stack-based script execution engine.
//!
//! Executes a scriptSig + scriptPubKey pair and returns whether the combined
//! script evaluates to true (i.e., the spending is valid).

use crate::{
    error::{Result, ScriptError},
    script::{Script, ScriptItem},
};
use ripemd::Ripemd160;
use secp256k1::{ecdsa::Signature, Message, PublicKey, Secp256k1};
use sha2::{Digest, Sha256};

/// Maximum stack depth.
const MAX_STACK_DEPTH: usize = 1000;
/// Maximum number of opcodes executed before the engine aborts (DoS protection).
const MAX_SCRIPT_EXEC_STEPS: usize = 200;

/// Execution environment passed to the engine for context-dependent opcodes.
#[derive(Debug, Clone, Default)]
pub struct ScriptEnv {
    /// The transaction hash being signed (for OP_CHECKSIG).
    pub tx_hash: [u8; 32],
    /// Current block height (for OP_CHECKLOCKTIMEVERIFY).
    pub block_height: u32,
    /// Current block timestamp (for OP_CHECKSEQUENCEVERIFY).
    pub block_time: u32,
    /// Transaction lock_time (for OP_CHECKLOCKTIMEVERIFY).
    pub tx_lock_time: u32,
    /// The input's nSequence (for OP_CHECKLOCKTIMEVERIFY finality check).
    pub input_sequence: u32,
}

/// The script execution engine.
pub struct Engine {
    /// Main stack.
    stack: Vec<Vec<u8>>,
    /// Alt stack (for OP_TOALTSTACK / OP_FROMALTSTACK).
    alt_stack: Vec<Vec<u8>>,
    /// Execution environment.
    env: ScriptEnv,
    /// secp256k1 context.
    secp: Secp256k1<secp256k1::All>,
}

impl Engine {
    /// Create a new engine with the given execution environment.
    pub fn new(env: ScriptEnv) -> Self {
        Self {
            stack: Vec::new(),
            alt_stack: Vec::new(),
            env,
            secp: Secp256k1::new(),
        }
    }

    /// Execute a scriptSig followed by a scriptPubKey.
    ///
    /// Returns `Ok(())` if the script pair is valid (top of stack is true).
    /// Returns an error if execution fails or the result is false.
    pub fn execute(&mut self, script_sig: &Script, script_pubkey: &Script) -> Result<()> {
        self.stack.clear();
        self.alt_stack.clear();

        // Execute scriptSig first (pushes signatures and pubkeys)
        self.run(script_sig)?;

        // For P2SH: save a copy of the stack after scriptSig
        let stack_after_sig = self.stack.clone();

        // Execute scriptPubKey
        self.run(script_pubkey)?;

        // Check top of stack is true
        let top = self.stack.last().ok_or(ScriptError::EmptyStack)?;
        if !is_true(top) {
            return Err(ScriptError::VerifyFailed);
        }

        // P2SH: if scriptPubKey is P2SH, execute the redeem script
        if is_p2sh(script_pubkey) {
            self.stack = stack_after_sig;
            let redeem_script_bytes = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
            let redeem_script = Script::from_bytes(redeem_script_bytes)?;
            self.run(&redeem_script)?;
            let top = self.stack.last().ok_or(ScriptError::EmptyStack)?;
            if !is_true(top) {
                return Err(ScriptError::VerifyFailed);
            }
        }

        Ok(())
    }

    /// Run a single script, modifying the stack.
    fn run(&mut self, script: &Script) -> Result<()> {
        let mut executing = true;
        let mut if_stack: Vec<bool> = Vec::new();
        let mut steps: usize = 0;

        for item in script.iter() {
            steps += 1;
            if steps > MAX_SCRIPT_EXEC_STEPS {
                return Err(ScriptError::StackOverflow);
            }
            match item {
                ScriptItem::PushData(data) => {
                    if executing {
                        if self.stack.len() >= MAX_STACK_DEPTH {
                            return Err(ScriptError::StackOverflow);
                        }
                        self.stack.push(data.to_vec());
                    }
                }
                ScriptItem::Opcode(op) => {
                    match op {
                        // ── Flow control ─────────────────────────────────────
                        0x63 => {
                            // OP_IF
                            let val = if executing {
                                let top = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                is_true(&top)
                            } else {
                                false
                            };
                            if_stack.push(executing);
                            executing = executing && val;
                        }
                        0x64 => {
                            // OP_NOTIF
                            let val = if executing {
                                let top = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                !is_true(&top)
                            } else {
                                false
                            };
                            if_stack.push(executing);
                            executing = executing && val;
                        }
                        0x67 => {
                            // OP_ELSE
                            let parent = if_stack.last().copied().unwrap_or(true);
                            executing = parent && !executing;
                        }
                        0x68 => {
                            // OP_ENDIF
                            executing = if_stack.pop().unwrap_or(true);
                        }
                        0x61 => {} // OP_NOP — do nothing
                        0x6a => {
                            // OP_RETURN
                            return Err(ScriptError::OpReturnUnspendable);
                        }
                        0x69 => {
                            // OP_VERIFY
                            if executing {
                                let top = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                if !is_true(&top) {
                                    return Err(ScriptError::VerifyFailed);
                                }
                            }
                        }

                        // ── Stack ops ────────────────────────────────────────
                        0x76 => {
                            // OP_DUP
                            if executing {
                                let top = self.stack.last().ok_or(ScriptError::EmptyStack)?.clone();
                                self.stack.push(top);
                            }
                        }
                        0x75 => {
                            // OP_DROP
                            if executing {
                                self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                            }
                        }
                        0x7c => {
                            // OP_SWAP
                            if executing {
                                let len = self.stack.len();
                                if len < 2 {
                                    return Err(ScriptError::EmptyStack);
                                }
                                self.stack.swap(len - 1, len - 2);
                            }
                        }
                        0x79 => {
                            // OP_OVER
                            if executing {
                                let len = self.stack.len();
                                if len < 2 {
                                    return Err(ScriptError::EmptyStack);
                                }
                                let item = self.stack[len - 2].clone();
                                self.stack.push(item);
                            }
                        }
                        0x7b => {
                            // OP_ROT
                            if executing {
                                let len = self.stack.len();
                                if len < 3 {
                                    return Err(ScriptError::EmptyStack);
                                }
                                let item = self.stack.remove(len - 3);
                                self.stack.push(item);
                            }
                        }
                        0x6b => {
                            // OP_TOALTSTACK
                            if executing {
                                let top = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                self.alt_stack.push(top);
                            }
                        }
                        0x6c => {
                            // OP_FROMALTSTACK
                            if executing {
                                let top = self.alt_stack.pop().ok_or(ScriptError::EmptyStack)?;
                                self.stack.push(top);
                            }
                        }
                        0x6e => {
                            // OP_2DUP
                            if executing {
                                let len = self.stack.len();
                                if len < 2 {
                                    return Err(ScriptError::EmptyStack);
                                }
                                let a = self.stack[len - 2].clone();
                                let b = self.stack[len - 1].clone();
                                self.stack.push(a);
                                self.stack.push(b);
                            }
                        }
                        0x6d => {
                            // OP_2DROP
                            if executing {
                                self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                            }
                        }
                        0x73 => {
                            // OP_IFDUP
                            if executing {
                                let top = self.stack.last().ok_or(ScriptError::EmptyStack)?.clone();
                                if is_true(&top) {
                                    self.stack.push(top);
                                }
                            }
                        }
                        0x6f => {
                            // OP_3DUP
                            if executing {
                                let len = self.stack.len();
                                if len < 3 {
                                    return Err(ScriptError::EmptyStack);
                                }
                                let a = self.stack[len - 3].clone();
                                let b = self.stack[len - 2].clone();
                                let c = self.stack[len - 1].clone();
                                self.stack.extend_from_slice(&[a, b, c]);
                            }
                        }
                        0x70 => {
                            // OP_2OVER
                            if executing {
                                let len = self.stack.len();
                                if len < 4 {
                                    return Err(ScriptError::EmptyStack);
                                }
                                let a = self.stack[len - 4].clone();
                                let b = self.stack[len - 3].clone();
                                self.stack.extend_from_slice(&[a, b]);
                            }
                        }
                        0x71 => {
                            // OP_2ROT
                            if executing {
                                let len = self.stack.len();
                                if len < 6 {
                                    return Err(ScriptError::EmptyStack);
                                }
                                let a = self.stack[len - 6].clone();
                                let b = self.stack[len - 5].clone();
                                self.stack.remove(len - 6);
                                self.stack.remove(len - 6);
                                self.stack.push(a);
                                self.stack.push(b);
                            }
                        }
                        0x72 => {
                            // OP_2SWAP
                            if executing {
                                let len = self.stack.len();
                                if len < 4 {
                                    return Err(ScriptError::EmptyStack);
                                }
                                self.stack.swap(len - 4, len - 2);
                                self.stack.swap(len - 3, len - 1);
                            }
                        }
                        0x74 => {
                            // OP_DEPTH
                            if executing {
                                let depth = self.stack.len() as i64;
                                self.stack.push(int_to_bytes(depth));
                            }
                        }
                        0x77 => {
                            // OP_NIP
                            if executing {
                                let len = self.stack.len();
                                if len < 2 {
                                    return Err(ScriptError::EmptyStack);
                                }
                                self.stack.remove(len - 2);
                            }
                        }
                        0x7a => {
                            // OP_ROLL
                            if executing {
                                let n =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                if n < 0 || n as usize >= self.stack.len() {
                                    return Err(ScriptError::InvalidScriptNumber);
                                }
                                let idx = self.stack.len() - 1 - n as usize;
                                let item = self.stack.remove(idx);
                                self.stack.push(item);
                            }
                        }
                        0x78 => {
                            // OP_PICK (copy nth item to top)
                            if executing {
                                let n =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                if n < 0 || n as usize >= self.stack.len() {
                                    return Err(ScriptError::InvalidScriptNumber);
                                }
                                let idx = self.stack.len() - 1 - n as usize;
                                let item = self.stack[idx].clone();
                                self.stack.push(item);
                            }
                        }
                        0x7d => {
                            // OP_TUCK
                            if executing {
                                let len = self.stack.len();
                                if len < 2 {
                                    return Err(ScriptError::EmptyStack);
                                }
                                let top = self.stack[len - 1].clone();
                                self.stack.insert(len - 2, top);
                            }
                        }

                        // ── Bitwise / equality ───────────────────────────────
                        0x87 => {
                            // OP_EQUAL
                            if executing {
                                let b = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                let a = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                self.stack.push(bool_to_bytes(a == b));
                            }
                        }
                        0x88 => {
                            // OP_EQUALVERIFY
                            if executing {
                                let b = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                let a = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                if a != b {
                                    return Err(ScriptError::VerifyFailed);
                                }
                            }
                        }

                        // ── Crypto ───────────────────────────────────────────
                        0xa7 => {
                            // OP_SHA1
                            if executing {
                                let top = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                let hash = sha1::Sha1::digest(&top).to_vec();
                                self.stack.push(hash);
                            }
                        }
                        0xa8 => {
                            // OP_SHA256
                            if executing {
                                let top = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                let hash = Sha256::digest(&top).to_vec();
                                self.stack.push(hash);
                            }
                        }
                        0xa9 => {
                            // OP_HASH160 (RIPEMD160(SHA256(x)))
                            if executing {
                                let top = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                let sha = Sha256::digest(&top);
                                let hash = Ripemd160::digest(sha).to_vec();
                                self.stack.push(hash);
                            }
                        }
                        0xaa => {
                            // OP_HASH256 (SHA256(SHA256(x)))
                            if executing {
                                let top = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                let h1 = Sha256::digest(&top);
                                let h2 = Sha256::digest(h1).to_vec();
                                self.stack.push(h2);
                            }
                        }
                        0xa6 => {
                            // OP_RIPEMD160
                            if executing {
                                let top = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                let hash = Ripemd160::digest(&top).to_vec();
                                self.stack.push(hash);
                            }
                        }

                        // ── Signature verification ───────────────────────────
                        0xab => {
                            // OP_CODESEPARATOR — currently a NOP.
                            // Full implementation requires lazy sighash recomputation
                            // at OP_CHECKSIG time with a truncated subscript (everything
                            // after the last CODESEPARATOR).  See Bitcoin Core:
                            // script/interpreter.cpp :: EvalScript for the reference.
                        }
                        0xac => {
                            // OP_CHECKSIG
                            if executing {
                                let pubkey_bytes =
                                    self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                let sig_bytes = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                let result = self.check_sig(&sig_bytes, &pubkey_bytes);
                                self.stack.push(bool_to_bytes(result.is_ok()));
                            }
                        }
                        0xad => {
                            // OP_CHECKSIGVERIFY
                            if executing {
                                let pubkey_bytes =
                                    self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                let sig_bytes = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                self.check_sig(&sig_bytes, &pubkey_bytes)?;
                            }
                        }
                        0xae => {
                            // OP_CHECKMULTISIG
                            if executing {
                                self.exec_checkmultisig(false)?;
                            }
                        }
                        0xaf => {
                            // OP_CHECKMULTISIGVERIFY
                            if executing {
                                self.exec_checkmultisig(true)?;
                            }
                        }

                        // ── Timelock ─────────────────────────────────────────
                        0xb1 => {
                            // OP_CHECKLOCKTIMEVERIFY (BIP-65)
                            if executing {
                                let top = self.stack.last().ok_or(ScriptError::EmptyStack)?;
                                let locktime = bytes_to_int(top);
                                if locktime < 0 {
                                    return Err(ScriptError::NegativeLocktime);
                                }
                                if locktime > u32::MAX as i64 {
                                    return Err(ScriptError::UnsatisfiedLocktime);
                                }
                                // Bitcoin Core rejects values where bit 31 is set
                                // (treated as negative by CScriptNum).
                                if locktime > 0x7FFF_FFFF {
                                    return Err(ScriptError::UnsatisfiedLocktime);
                                }
                                let locktime = locktime as u32;
                                // The input must not be final (nSequence != MAX).
                                if self.env.input_sequence == 0xffff_ffff {
                                    return Err(ScriptError::UnsatisfiedLocktime);
                                }
                                // The locktime and tx lock_time must be the same
                                // type (both height or both timestamp).
                                let locktime_is_time = locktime >= 500_000_000;
                                let tx_is_time = self.env.tx_lock_time >= 500_000_000;
                                if locktime_is_time != tx_is_time {
                                    return Err(ScriptError::UnsatisfiedLocktime);
                                }
                                if self.env.tx_lock_time < locktime {
                                    return Err(ScriptError::HtlcLocktimeNotExpired);
                                }
                            }
                        }
                        0xb2 => {
                            // OP_CHECKSEQUENCEVERIFY (BIP-112)
                            if executing {
                                let top = self.stack.last().ok_or(ScriptError::EmptyStack)?;
                                let seq = bytes_to_int(top);
                                if seq < 0 {
                                    return Err(ScriptError::NegativeSequence);
                                }
                                if seq > u32::MAX as i64 {
                                    return Err(ScriptError::UnsatisfiedSequence);
                                }
                                let seq = seq as u32;
                                // If the disable flag (bit 31) is set, CSV behaves
                                // as a NOP per BIP-112.
                                if seq & 0x8000_0000 != 0 {
                                    // NOP — leave stack unchanged.
                                } else if self.env.input_sequence == 0xffff_ffff {
                                    // The input must not be final.
                                    return Err(ScriptError::UnsatisfiedSequence);
                                } else {
                                    // Type flags must match (height vs time).
                                    let seq_is_time = seq & 0x0040_0000 != 0;
                                    let input_is_time = self.env.input_sequence & 0x0040_0000 != 0;
                                    if seq_is_time != input_is_time {
                                        return Err(ScriptError::UnsatisfiedSequence);
                                    }
                                    let mask = 0x0000_ffff;
                                    let arg = seq & mask;
                                    let input = self.env.input_sequence & mask;
                                    if input < arg {
                                        return Err(ScriptError::UnsatisfiedSequence);
                                    }
                                }
                            }
                        }

                        // ── Arithmetic ───────────────────────────────────────
                        0x93 => {
                            // OP_ADD
                            if executing {
                                let b =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(int_to_bytes(a.wrapping_add(b)));
                            }
                        }
                        0x94 => {
                            // OP_SUB
                            if executing {
                                let b =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(int_to_bytes(a.wrapping_sub(b)));
                            }
                        }
                        0x8b => {
                            // OP_1ADD
                            if executing {
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(int_to_bytes(a.wrapping_add(1)));
                            }
                        }
                        0x8c => {
                            // OP_1SUB
                            if executing {
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(int_to_bytes(a.wrapping_sub(1)));
                            }
                        }
                        0x8f => {
                            // OP_NEGATE
                            if executing {
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(int_to_bytes(a.wrapping_neg()));
                            }
                        }
                        0x90 => {
                            // OP_ABS
                            if executing {
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                // Bitcoin spec: OP_ABS of i64::MIN is invalid
                                // (wrapping_abs would return i64::MIN, still negative).
                                if a == i64::MIN {
                                    return Err(ScriptError::InvalidScriptNumber);
                                }
                                self.stack.push(int_to_bytes(a.wrapping_abs()));
                            }
                        }
                        0x91 => {
                            // OP_NOT
                            if executing {
                                let top = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                self.stack.push(bool_to_bytes(!is_true(&top)));
                            }
                        }
                        0x92 => {
                            // OP_0NOTEQUAL
                            if executing {
                                let top = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                self.stack.push(bool_to_bytes(bytes_to_int(&top) != 0));
                            }
                        }
                        0x9a => {
                            // OP_BOOLAND
                            if executing {
                                let b = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                let a = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                self.stack.push(bool_to_bytes(is_true(&a) && is_true(&b)));
                            }
                        }
                        0x9b => {
                            // OP_BOOLOR
                            if executing {
                                let b = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                let a = self.stack.pop().ok_or(ScriptError::EmptyStack)?;
                                self.stack.push(bool_to_bytes(is_true(&a) || is_true(&b)));
                            }
                        }
                        0x9c => {
                            // OP_NUMEQUAL
                            if executing {
                                let b =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(bool_to_bytes(a == b));
                            }
                        }
                        0x9d => {
                            // OP_NUMEQUALVERIFY
                            if executing {
                                let b =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                if a != b {
                                    return Err(ScriptError::VerifyFailed);
                                }
                            }
                        }
                        0x9e => {
                            // OP_NUMNOTEQUAL
                            if executing {
                                let b =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(bool_to_bytes(a != b));
                            }
                        }
                        0x9f => {
                            // OP_LESSTHAN
                            if executing {
                                let b =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(bool_to_bytes(a < b));
                            }
                        }
                        0xa0 => {
                            // OP_GREATERTHAN
                            if executing {
                                let b =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(bool_to_bytes(a > b));
                            }
                        }
                        0xa1 => {
                            // OP_LESSTHANOREQUAL
                            if executing {
                                let b =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(bool_to_bytes(a <= b));
                            }
                        }
                        0xa2 => {
                            // OP_GREATERTHANOREQUAL
                            if executing {
                                let b =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(bool_to_bytes(a >= b));
                            }
                        }
                        0xa3 => {
                            // OP_MIN
                            if executing {
                                let b =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(int_to_bytes(a.min(b)));
                            }
                        }
                        0xa4 => {
                            // OP_MAX
                            if executing {
                                let b =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                let a =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(int_to_bytes(a.max(b)));
                            }
                        }
                        0xa5 => {
                            // OP_WITHIN
                            if executing {
                                let max =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                let min =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                let x =
                                    bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
                                self.stack.push(bool_to_bytes(x >= min && x < max));
                            }
                        }

                        // ── Small integers ───────────────────────────────────
                        0x00 => {
                            if executing {
                                self.stack.push(vec![]);
                            } // OP_0 = false
                        }
                        0x51..=0x60 => {
                            // OP_1 through OP_16
                            if executing {
                                let n = op - 0x50;
                                self.stack.push(vec![n]);
                            }
                        }
                        0x4f => {
                            // OP_1NEGATE
                            if executing {
                                self.stack.push(vec![0x81]);
                            }
                        }

                        // ── Size ─────────────────────────────────────────────
                        0x82 => {
                            // OP_SIZE
                            if executing {
                                let len = self.stack.last().ok_or(ScriptError::EmptyStack)?.len();
                                self.stack.push(int_to_bytes(len as i64));
                            }
                        }

                        _ => {
                            if executing {
                                return Err(ScriptError::InvalidOpcode(op));
                            }
                        }
                    }
                }
            }
        }

        if !if_stack.is_empty() {
            return Err(ScriptError::UnbalancedConditional);
        }

        Ok(())
    }

    /// Verify a DER-encoded signature against a public key.
    fn check_sig(&self, sig_bytes: &[u8], pubkey_bytes: &[u8]) -> Result<()> {
        if sig_bytes.is_empty() {
            return Err(ScriptError::InvalidSignature);
        }

        // Strip the sighash type byte (last byte of the DER signature). The
        // valid sighash types are 0x01 (ALL), 0x02 (NONE), 0x03 (SINGLE), and
        // their ANYONECANPAY variants 0x81/0x82/0x83. Strip any of them, not
        // just 0x01/0x83, so SIGHASH_NONE/SINGLE/ANYONECANPAY signatures verify.
        let last = *sig_bytes.last().unwrap();
        let is_sighash = matches!(last, 0x01 | 0x02 | 0x03 | 0x81 | 0x82 | 0x83);
        let sig_der = if is_sighash {
            &sig_bytes[..sig_bytes.len() - 1]
        } else {
            sig_bytes
        };

        let sig = Signature::from_der(sig_der).map_err(|_| ScriptError::InvalidSignature)?;

        let pubkey =
            PublicKey::from_slice(pubkey_bytes).map_err(|_| ScriptError::InvalidPublicKey)?;

        let msg = Message::from_digest(self.env.tx_hash);

        self.secp
            .verify_ecdsa(&msg, &sig, &pubkey)
            .map_err(|_| ScriptError::SignatureVerification)
    }

    /// Execute OP_CHECKMULTISIG / OP_CHECKMULTISIGVERIFY.
    fn exec_checkmultisig(&mut self, verify: bool) -> Result<()> {
        // Stack: <OP_0> <sig1> ... <sigM> <M> <key1> ... <keyN> <N>
        let n_keys = bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
        // Bound the key count to the remaining stack depth to avoid an
        // attacker-controlled multi-GB allocation.
        if n_keys < 0 || n_keys as usize > self.stack.len() {
            return Err(ScriptError::InvalidScriptNumber);
        }
        let n_keys = n_keys as usize;
        let mut keys = Vec::with_capacity(n_keys);
        for _ in 0..n_keys {
            keys.push(self.stack.pop().ok_or(ScriptError::EmptyStack)?);
        }

        let n_sigs = bytes_to_int(&self.stack.pop().ok_or(ScriptError::EmptyStack)?);
        if n_sigs < 0 || n_sigs as usize > self.stack.len() {
            return Err(ScriptError::InvalidScriptNumber);
        }
        let n_sigs = n_sigs as usize;
        let mut sigs = Vec::with_capacity(n_sigs);
        for _ in 0..n_sigs {
            sigs.push(self.stack.pop().ok_or(ScriptError::EmptyStack)?);
        }

        // Bitcoin bug: consume one extra stack item (OP_0 dummy)
        self.stack.pop();

        if n_sigs > n_keys {
            return Err(ScriptError::MultisigKeyCount(n_sigs, n_keys));
        }

        // Verify M-of-N: each sig must match at least one key (in order)
        let mut key_idx = 0;
        let mut valid_sigs = 0;

        'sig_loop: for sig in &sigs {
            while key_idx < keys.len() {
                if self.check_sig(sig, &keys[key_idx]).is_ok() {
                    valid_sigs += 1;
                    key_idx += 1;
                    continue 'sig_loop;
                }
                key_idx += 1;
            }
        }

        let success = valid_sigs >= n_sigs;

        if verify && !success {
            return Err(ScriptError::SignatureVerification);
        }

        self.stack.push(bool_to_bytes(success));
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns true if the byte slice represents a truthy Script value.
fn is_true(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    // False if all bytes are zero, or if it's negative zero (0x80)
    for (i, &b) in bytes.iter().enumerate() {
        if i == bytes.len() - 1 {
            if b & 0x7f != 0 {
                return true;
            }
        } else if b != 0 {
            return true;
        }
    }
    false
}

fn bool_to_bytes(b: bool) -> Vec<u8> {
    if b {
        vec![1]
    } else {
        vec![]
    }
}

fn bytes_to_int(bytes: &[u8]) -> i64 {
    if bytes.is_empty() {
        return 0;
    }
    // Bitcoin script uses sign-magnitude encoding: the high bit of the last
    // byte is the sign bit, the remaining bits are the absolute value.
    let n = bytes.len().min(8);
    let mut result = 0i64;
    for (i, &b) in bytes[..n].iter().enumerate() {
        result |= (b as i64) << (8 * i);
    }
    let sign_mask = 1i64 << (8 * n - 1);
    if result & sign_mask != 0 {
        result &= !sign_mask;
        -result
    } else {
        result
    }
}

fn int_to_bytes(n: i64) -> Vec<u8> {
    if n == 0 {
        return vec![];
    }
    let mut abs = n.unsigned_abs();
    let negative = n < 0;
    let mut result = Vec::new();
    while abs > 0 {
        result.push((abs & 0xff) as u8);
        abs >>= 8;
    }
    if result.last().map(|&b| b & 0x80 != 0).unwrap_or(false) {
        result.push(if negative { 0x80 } else { 0x00 });
    } else if negative {
        *result.last_mut().unwrap() |= 0x80;
    }
    result
}

/// Returns true if the script is a P2SH scriptPubKey.
fn is_p2sh(script: &Script) -> bool {
    let b = script.as_bytes();
    b.len() == 23 && b[0] == 0xa9 && b[1] == 0x14 && b[22] == 0x87
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::standard::build_p2pkh;
    use secp256k1::{Message, Secp256k1, SecretKey};
    use sha2::{Digest, Sha256};

    fn make_keypair() -> (SecretKey, secp256k1::PublicKey) {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[1u8; 32]).unwrap();
        let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
        (sk, pk)
    }

    fn sign_tx(sk: &SecretKey, tx_hash: &[u8; 32]) -> Vec<u8> {
        let secp = Secp256k1::new();
        let msg = Message::from_digest(*tx_hash);
        let sig = secp.sign_ecdsa(&msg, sk);
        let mut sig_bytes = sig.serialize_der().to_vec();
        sig_bytes.push(0x01); // SIGHASH_ALL
        sig_bytes
    }

    #[test]
    fn test_p2pkh_valid() {
        let (sk, pk) = make_keypair();
        let pk_bytes = pk.serialize().to_vec();

        let tx_hash = [0xabu8; 32];
        let sig = sign_tx(&sk, &tx_hash);

        // Build P2PKH scriptPubKey from pubkey hash
        let pubkey_hash: [u8; 20] = {
            let sha = Sha256::digest(&pk_bytes);
            let ripe = ripemd::Ripemd160::digest(sha);
            ripe.into()
        };
        let script_pubkey = build_p2pkh(&pubkey_hash).unwrap();

        // Build scriptSig: <sig> <pubkey>
        let mut script_sig = crate::script::Script::new();
        script_sig.push_data(&sig).unwrap();
        script_sig.push_data(&pk_bytes).unwrap();

        let env = ScriptEnv {
            tx_hash,
            ..Default::default()
        };
        let mut engine = Engine::new(env);
        engine
            .execute(&script_sig, &script_pubkey)
            .expect("P2PKH should be valid");
    }

    #[test]
    fn test_p2pkh_wrong_signature_fails() {
        let (_, pk) = make_keypair();
        let pk_bytes = pk.serialize().to_vec();

        let tx_hash = [0xabu8; 32];
        // Sign with wrong key
        let (wrong_sk, _) = {
            let secp = Secp256k1::new();
            let sk = SecretKey::from_slice(&[2u8; 32]).unwrap();
            let pk2 = secp256k1::PublicKey::from_secret_key(&secp, &sk);
            (sk, pk2)
        };
        let sig = sign_tx(&wrong_sk, &tx_hash);

        let pubkey_hash: [u8; 20] = {
            let sha = Sha256::digest(&pk_bytes);
            ripemd::Ripemd160::digest(sha).into()
        };
        let script_pubkey = build_p2pkh(&pubkey_hash).unwrap();

        let mut script_sig = crate::script::Script::new();
        script_sig.push_data(&sig).unwrap();
        script_sig.push_data(&pk_bytes).unwrap();

        let env = ScriptEnv {
            tx_hash,
            ..Default::default()
        };
        let mut engine = Engine::new(env);
        assert!(engine.execute(&script_sig, &script_pubkey).is_err());
    }

    #[test]
    fn test_op_return_unspendable() {
        let script_sig = Script::new();
        let mut script_pubkey = Script::new();
        script_pubkey.push_opcode(0x6a); // OP_RETURN
        script_pubkey.push_data(b"hello").unwrap();

        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        let result = engine.execute(&script_sig, &script_pubkey);
        assert!(matches!(result, Err(ScriptError::OpReturnUnspendable)));
    }

    #[test]
    fn test_op_equal_true() {
        let mut script_sig = Script::new();
        script_sig.push_data(b"hello").unwrap();
        script_sig.push_data(b"hello").unwrap();

        let mut script_pubkey = Script::new();
        script_pubkey.push_opcode(0x87); // OP_EQUAL

        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine
            .execute(&script_sig, &script_pubkey)
            .expect("equal items should succeed");
    }

    #[test]
    fn test_op_equal_false() {
        let mut script_sig = Script::new();
        script_sig.push_data(b"hello").unwrap();
        script_sig.push_data(b"world").unwrap();

        let mut script_pubkey = Script::new();
        script_pubkey.push_opcode(0x87); // OP_EQUAL

        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        assert!(engine.execute(&script_sig, &script_pubkey).is_err());
    }

    #[test]
    fn test_op_dup_and_hash160() {
        // OP_DUP OP_HASH160 <hash> OP_EQUALVERIFY — without the final CHECKSIG
        let (_, pk) = make_keypair();
        let pk_bytes = pk.serialize().to_vec();
        let pubkey_hash: Vec<u8> = {
            let sha = Sha256::digest(&pk_bytes);
            ripemd::Ripemd160::digest(sha).to_vec()
        };

        let mut script_sig = Script::new();
        script_sig.push_data(&pk_bytes).unwrap();

        let mut script_pubkey = Script::new();
        script_pubkey.push_opcode(0x76); // OP_DUP
        script_pubkey.push_opcode(0xa9); // OP_HASH160
        script_pubkey.push_data(&pubkey_hash).unwrap();
        script_pubkey.push_opcode(0x88); // OP_EQUALVERIFY
                                         // Push true to leave stack non-empty
        script_pubkey.push_opcode(0x51); // OP_1

        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine
            .execute(&script_sig, &script_pubkey)
            .expect("hash160 should match");
    }

    #[test]
    fn test_if_else_endif() {
        // Push 1, OP_IF push "yes" OP_ELSE push "no" OP_ENDIF
        let mut script = Script::new();
        script.push_opcode(0x51); // OP_1 (true)
        script.push_opcode(0x63); // OP_IF
        script.push_data(b"yes").unwrap();
        script.push_opcode(0x67); // OP_ELSE
        script.push_data(b"no").unwrap();
        script.push_opcode(0x68); // OP_ENDIF

        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.last().unwrap(), b"yes");
    }

    #[test]
    fn test_is_true_empty_is_false() {
        assert!(!is_true(&[]));
        assert!(!is_true(&[0x00]));
        assert!(is_true(&[0x01]));
        assert!(is_true(&[0x80, 0x01]));
    }

    // ── New opcode tests ───────────────────────────────────────────────────────

    #[test]
    fn test_depth_op() {
        let mut script = Script::new();
        script.push_opcode(0x51);
        script.push_opcode(0x52);
        script.push_opcode(0x53);
        script.push_opcode(0x74);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.last().unwrap(), &vec![3]);
    }

    #[test]
    fn test_3dup_op() {
        let mut script = Script::new();
        script.push_opcode(0x51);
        script.push_opcode(0x52);
        script.push_opcode(0x53);
        script.push_opcode(0x6f);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.len(), 6);
        assert_eq!(engine.stack.last().unwrap(), &vec![3]);
    }

    #[test]
    fn test_nip_op() {
        let mut script = Script::new();
        script.push_opcode(0x51);
        script.push_opcode(0x52);
        script.push_opcode(0x53);
        script.push_opcode(0x77);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.len(), 2);
        assert_eq!(engine.stack[0], vec![1]);
        assert_eq!(engine.stack[1], vec![3]);
    }

    #[test]
    fn test_roll_op() {
        let mut script = Script::new();
        script.push_opcode(0x51); // 1
        script.push_opcode(0x52); // 2
        script.push_opcode(0x53); // 3
        script.push_opcode(0x51); // index 1
        script.push_opcode(0x7a); // OP_ROLL
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        // Pop index 1, stack=[1,2,3]. idx=3-1-1=1, remove stack[1]=2 → [1,3,2]
        assert_eq!(engine.stack.last().unwrap(), &vec![2]);
    }

    #[test]
    fn test_pick_op() {
        let mut script = Script::new();
        script.push_opcode(0x51); // 1
        script.push_opcode(0x52); // 2
        script.push_opcode(0x53); // 3
        script.push_opcode(0x51); // index 1
        script.push_opcode(0x78); // OP_PICK (copy, don't remove)
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        // Pop index 1, stack=[1,2,3]. idx=3-1-1=1, clone stack[1]=2 → [1,2,3,2]
        assert_eq!(engine.stack.len(), 4);
        assert_eq!(engine.stack.last().unwrap(), &vec![2]);
        // Original item still in place
        assert_eq!(engine.stack[1], vec![2]);
    }

    #[test]
    fn test_tuck_op() {
        let mut script = Script::new();
        script.push_opcode(0x51);
        script.push_opcode(0x52);
        script.push_opcode(0x7d);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        // [1,2], top=2, insert at len-2=0 → [2,1,2]
        assert_eq!(engine.stack.len(), 3);
        assert_eq!(engine.stack[0], vec![2]);
        assert_eq!(engine.stack[1], vec![1]);
        assert_eq!(engine.stack[2], vec![2]);
    }

    #[test]
    fn test_2swap_op() {
        let mut script = Script::new();
        script.push_opcode(0x51);
        script.push_opcode(0x52);
        script.push_opcode(0x53);
        script.push_opcode(0x54);
        script.push_opcode(0x72);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack[0], vec![3]);
        assert_eq!(engine.stack[1], vec![4]);
        assert_eq!(engine.stack[2], vec![1]);
        assert_eq!(engine.stack[3], vec![2]);
    }

    #[test]
    fn test_2over_op() {
        let mut script = Script::new();
        script.push_opcode(0x51);
        script.push_opcode(0x52);
        script.push_opcode(0x53);
        script.push_opcode(0x54);
        script.push_opcode(0x70);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.len(), 6);
        assert_eq!(engine.stack[4], vec![1]);
        assert_eq!(engine.stack[5], vec![2]);
    }

    #[test]
    fn test_2rot_op() {
        let mut script = Script::new();
        script.push_opcode(0x51);
        script.push_opcode(0x52);
        script.push_opcode(0x53);
        script.push_opcode(0x54);
        script.push_opcode(0x55);
        script.push_opcode(0x56);
        script.push_opcode(0x71);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.len(), 6);
        assert_eq!(engine.stack[0], vec![3]);
        assert_eq!(engine.stack[1], vec![4]);
        assert_eq!(engine.stack[2], vec![5]);
        assert_eq!(engine.stack[3], vec![6]);
        assert_eq!(engine.stack[4], vec![1]);
        assert_eq!(engine.stack[5], vec![2]);
    }

    #[test]
    fn test_csv_nop_when_disable_flag_set() {
        let mut script = Script::new();
        // Push 0x80000001 as a 5-byte value so it's positive in sign-magnitude
        // (bit 31 set for disable flag, but sign bit in byte[4] is clear)
        script.push_data(&[0x01, 0x00, 0x00, 0x80, 0x00]).unwrap();
        script.push_opcode(0xb2);
        script.push_opcode(0x75);
        script.push_opcode(0x51);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.last().unwrap(), &vec![1]);
    }

    #[test]
    fn test_0notequal() {
        let mut script = Script::new();
        script.push_opcode(0x00);
        script.push_opcode(0x92);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.last().unwrap(), &vec![]);
    }

    #[test]
    fn test_booland_boolor() {
        let mut script = Script::new();
        script.push_opcode(0x51);
        script.push_opcode(0x52);
        script.push_opcode(0x9a);
        script.push_opcode(0x51);
        script.push_opcode(0x00);
        script.push_opcode(0x9b);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.len(), 2);
        assert_eq!(engine.stack[0], vec![1]);
        assert_eq!(engine.stack[1], vec![1]);
    }

    #[test]
    fn test_numequal_numnotequal() {
        let mut script = Script::new();
        script.push_opcode(0x55);
        script.push_opcode(0x55);
        script.push_opcode(0x9c);
        script.push_opcode(0x55);
        script.push_opcode(0x54);
        script.push_opcode(0x9e);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.len(), 2);
        assert_eq!(engine.stack[0], vec![1]);
        assert_eq!(engine.stack[1], vec![1]);
    }

    #[test]
    fn test_numequalverify_fails_on_mismatch() {
        let mut script = Script::new();
        script.push_opcode(0x55);
        script.push_opcode(0x54);
        script.push_opcode(0x9d);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        assert!(engine.run(&script).is_err());
    }

    #[test]
    fn test_compare_ops() {
        let mut script = Script::new();
        script.push_opcode(0x51);
        script.push_opcode(0x52);
        script.push_opcode(0x9f);
        script.push_opcode(0x52);
        script.push_opcode(0x51);
        script.push_opcode(0xa0);
        script.push_opcode(0x53);
        script.push_opcode(0x53);
        script.push_opcode(0xa1);
        script.push_opcode(0x54);
        script.push_opcode(0x54);
        script.push_opcode(0xa2);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.len(), 4);
        for item in &engine.stack {
            assert_eq!(item, &vec![1u8]);
        }
    }

    #[test]
    fn test_min_max_within() {
        let mut script = Script::new();
        script.push_opcode(0x53);
        script.push_opcode(0x55);
        script.push_opcode(0xa3);
        script.push_opcode(0x53);
        script.push_opcode(0x55);
        script.push_opcode(0xa4);
        script.push_opcode(0x54);
        script.push_opcode(0x53);
        script.push_opcode(0x57);
        script.push_opcode(0xa5);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.len(), 3);
        assert_eq!(engine.stack[0], vec![3]);
        assert_eq!(engine.stack[1], vec![5]);
        assert_eq!(engine.stack[2], vec![1]);
    }

    #[test]
    fn test_csv_succeeds_when_sequence_sufficient() {
        let mut script = Script::new();
        script.push_opcode(0x52);
        script.push_opcode(0xb2);
        script.push_opcode(0x75);
        script.push_opcode(0x51);
        let env = ScriptEnv {
            input_sequence: 0x0002_0002,
            ..Default::default()
        };
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.last().unwrap(), &vec![1]);
    }

    #[test]
    fn test_csv_fails_when_sequence_insufficient() {
        let mut script = Script::new();
        script.push_opcode(0x52);
        script.push_opcode(0xb2);
        let env = ScriptEnv {
            input_sequence: 0x0001_0001,
            ..Default::default()
        };
        let mut engine = Engine::new(env);
        assert!(matches!(
            engine.run(&script),
            Err(ScriptError::UnsatisfiedSequence)
        ));
    }

    #[test]
    fn test_csv_fails_when_final_input() {
        let mut script = Script::new();
        script.push_opcode(0x51);
        script.push_opcode(0xb2);
        let env = ScriptEnv {
            input_sequence: 0xffff_ffff,
            ..Default::default()
        };
        let mut engine = Engine::new(env);
        assert!(matches!(
            engine.run(&script),
            Err(ScriptError::UnsatisfiedSequence)
        ));
    }

    #[test]
    fn test_csv_fails_on_negative() {
        let mut script = Script::new();
        script.push_data(&[0x81]).unwrap();
        script.push_opcode(0xb2);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        assert!(matches!(
            engine.run(&script),
            Err(ScriptError::NegativeSequence)
        ));
    }

    #[test]
    fn test_csv_fails_on_type_mismatch() {
        let mut script = Script::new();
        script
            .push_data(&0x0040_0001u32.to_le_bytes()[..4])
            .unwrap();
        script.push_opcode(0xb2);
        let env = ScriptEnv {
            input_sequence: 0x0000_0002,
            ..Default::default()
        };
        let mut engine = Engine::new(env);
        assert!(matches!(
            engine.run(&script),
            Err(ScriptError::UnsatisfiedSequence)
        ));
    }

    #[test]
    fn test_op_codeseparator_is_nop() {
        let mut script = Script::new();
        script.push_data(&[42]).unwrap();
        script.push_opcode(0xab); // OP_CODESEPARATOR
        script.push_data(&[99]).unwrap();
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.run(&script).unwrap();
        assert_eq!(engine.stack.len(), 2);
        assert_eq!(engine.stack[0], vec![42]);
        assert_eq!(engine.stack[1], vec![99]);
    }
}
