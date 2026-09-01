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
use std::sync::LazyLock;

/// Maximum stack depth.
const MAX_STACK_DEPTH: usize = 1000;

/// Maximum operand size for arithmetic opcodes (CScriptNum rule).
const MAX_ARITH_NUM_LEN: usize = 4;

/// Push an arithmetic result, rejecting values that do not fit a 4-byte
/// sign-magnitude script number (Bitcoin requires arithmetic outputs to be
/// representable in CScriptNum serialization).
fn push_arith_result(stack: &mut Vec<Vec<u8>>, value: Option<i64>) -> Result<()> {
    let value = value.ok_or(ScriptError::InvalidScriptNumber)?;
    let bytes = int_to_bytes(value);
    if bytes.len() > MAX_ARITH_NUM_LEN {
        return Err(ScriptError::InvalidScriptNumber);
    }
    stack.push(bytes);
    Ok(())
}
/// Maximum number of opcodes executed before the engine aborts (DoS protection).
const MAX_SCRIPT_EXEC_STEPS: usize = 200;

/// Static secp256k1 context — avoids re-seeding RNG on every engine creation.
static SECP: LazyLock<Secp256k1<secp256k1::All>> = LazyLock::new(Secp256k1::new);

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
    /// Height at which the UTXO being spent was created (for OP_CHECKSEQUENCEVERIFY).
    pub utxo_height: u32,
    /// Timestamp at which the UTXO being spent was created (for OP_CHECKSEQUENCEVERIFY).
    pub utxo_timestamp: u32,
}

/// The script execution engine.
pub struct Engine {
    /// Main stack.
    stack: Vec<Vec<u8>>,
    /// Alt stack (for OP_TOALTSTACK / OP_FROMALTSTACK).
    alt_stack: Vec<Vec<u8>>,
    /// Execution environment.
    env: ScriptEnv,
}

impl Engine {
    /// Create a new engine with the given execution environment.
    pub fn new(env: ScriptEnv) -> Self {
        Self {
            stack: Vec::new(),
            alt_stack: Vec::new(),
            env,
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
        // Reject truncated pushes up front: the iterator would otherwise stop
        // silently mid-script and let whatever is on the stack stand as the
        // result (consensus-incorrect — a truncated push must fail the script).
        script.validate()?;

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
                                let n = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
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
                                let n = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
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
                                let locktime = decode_script_num(top, 5)?;
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
                                // Chain-state check (BIP-65): the spend is only
                                // valid once the blockchain itself has reached
                                // the locktime. Without this, a spender could
                                // self-declare tx.lock_time >= expiry and spend
                                // a time-locked output before it expires.
                                let chain_reached = if locktime_is_time {
                                    self.env.block_time >= locktime
                                } else {
                                    self.env.block_height >= locktime
                                };
                                if !chain_reached {
                                    return Err(ScriptError::HtlcLocktimeNotExpired);
                                }
                            }
                        }
                        0xb2 => {
                            // OP_CHECKSEQUENCEVERIFY (BIP-112)
                            if executing {
                                let top = self.stack.last().ok_or(ScriptError::EmptyStack)?;
                                let seq = decode_script_num(top, 5)?;
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
                                    // Chain-state check (BIP-68): the spent
                                    // output must actually be `arg` blocks or
                                    // 512-second units old. Without this a
                                    // spender could self-declare nSequence and
                                    // spend a relative-locked output early.
                                    let age_satisfied = if seq_is_time {
                                        let arg_secs = u64::from(arg) * 512;
                                        let age_secs = self
                                            .env
                                            .block_time
                                            .saturating_sub(self.env.utxo_timestamp)
                                            as u64;
                                        u64::from(input) * 512 >= arg_secs && age_secs >= arg_secs
                                    } else {
                                        let age_blocks = self
                                            .env
                                            .block_height
                                            .saturating_sub(self.env.utxo_height);
                                        age_blocks >= arg
                                    };
                                    if !age_satisfied {
                                        return Err(ScriptError::UnsatisfiedSequence);
                                    }
                                }
                            }
                        }

                        // ── Arithmetic ───────────────────────────────────────
                        0x93 => {
                            // OP_ADD
                            if executing {
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                push_arith_result(&mut self.stack, a.checked_add(b))?;
                            }
                        }
                        0x94 => {
                            // OP_SUB
                            if executing {
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                push_arith_result(&mut self.stack, a.checked_sub(b))?;
                            }
                        }
                        0x95 => {
                            // OP_MUL
                            if executing {
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                push_arith_result(&mut self.stack, a.checked_mul(b))?;
                            }
                        }
                        0x96 => {
                            // OP_DIV
                            if executing {
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                push_arith_result(&mut self.stack, a.checked_div(b))?;
                            }
                        }
                        0x97 => {
                            // OP_MOD
                            if executing {
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                push_arith_result(&mut self.stack, a.checked_rem(b))?;
                            }
                        }
                        0x8b => {
                            // OP_1ADD
                            if executing {
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                push_arith_result(&mut self.stack, a.checked_add(1))?;
                            }
                        }
                        0x8c => {
                            // OP_1SUB
                            if executing {
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                push_arith_result(&mut self.stack, a.checked_sub(1))?;
                            }
                        }
                        0x8f => {
                            // OP_NEGATE
                            if executing {
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                push_arith_result(&mut self.stack, a.checked_neg())?;
                            }
                        }
                        0x90 => {
                            // OP_ABS
                            if executing {
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                push_arith_result(&mut self.stack, a.checked_abs())?;
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
                                self.stack.push(bool_to_bytes(
                                    decode_script_num(&top, MAX_ARITH_NUM_LEN)? != 0,
                                ));
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
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                self.stack.push(bool_to_bytes(a == b));
                            }
                        }
                        0x9d => {
                            // OP_NUMEQUALVERIFY
                            if executing {
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                if a != b {
                                    return Err(ScriptError::VerifyFailed);
                                }
                            }
                        }
                        0x9e => {
                            // OP_NUMNOTEQUAL
                            if executing {
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                self.stack.push(bool_to_bytes(a != b));
                            }
                        }
                        0x9f => {
                            // OP_LESSTHAN
                            if executing {
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                self.stack.push(bool_to_bytes(a < b));
                            }
                        }
                        0xa0 => {
                            // OP_GREATERTHAN
                            if executing {
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                self.stack.push(bool_to_bytes(a > b));
                            }
                        }
                        0xa1 => {
                            // OP_LESSTHANOREQUAL
                            if executing {
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                self.stack.push(bool_to_bytes(a <= b));
                            }
                        }
                        0xa2 => {
                            // OP_GREATERTHANOREQUAL
                            if executing {
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                self.stack.push(bool_to_bytes(a >= b));
                            }
                        }
                        0xa3 => {
                            // OP_MIN
                            if executing {
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                self.stack.push(int_to_bytes(a.min(b)));
                            }
                        }
                        0xa4 => {
                            // OP_MAX
                            if executing {
                                let b = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let a = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                self.stack.push(int_to_bytes(a.max(b)));
                            }
                        }
                        0xa5 => {
                            // OP_WITHIN
                            if executing {
                                let max = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let min = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
                                let x = decode_script_num(
                                    &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
                                    MAX_ARITH_NUM_LEN,
                                )?;
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

        let last = *sig_bytes.last().unwrap();
        if last != 0x01 {
            return Err(ScriptError::InvalidSignature);
        }
        let sig_der = &sig_bytes[..sig_bytes.len() - 1];

        let sig = Signature::from_der(sig_der).map_err(|_| ScriptError::InvalidSignature)?;

        let pubkey =
            PublicKey::from_slice(pubkey_bytes).map_err(|_| ScriptError::InvalidPublicKey)?;

        let msg = Message::from_digest(self.env.tx_hash);

        SECP.verify_ecdsa(&msg, &sig, &pubkey)
            .map_err(|_| ScriptError::SignatureVerification)
    }

    /// Execute OP_CHECKMULTISIG / OP_CHECKMULTISIGVERIFY.
    fn exec_checkmultisig(&mut self, verify: bool) -> Result<()> {
        // Stack: <OP_0> <sig1> ... <sigM> <M> <key1> ... <keyN> <N>
        let n_keys = decode_script_num(
            &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
            MAX_ARITH_NUM_LEN,
        )?;
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

        let n_sigs = decode_script_num(
            &self.stack.pop().ok_or(ScriptError::EmptyStack)?,
            MAX_ARITH_NUM_LEN,
        )?;
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

/// Decode a sign-magnitude script number, rejecting encodings longer than
/// `max_len` bytes. Arithmetic operands allow 4 bytes (CScriptNum rule);
/// CLTV/CSV operands allow 5 (BIP-65/112).
fn decode_script_num(bytes: &[u8], max_len: usize) -> Result<i64> {
    if bytes.is_empty() {
        return Ok(0);
    }
    if bytes.len() > max_len {
        return Err(ScriptError::InvalidScriptNumber);
    }
    let n = bytes.len();
    let mut magnitude = 0u64;
    for (i, &b) in bytes[..n].iter().enumerate() {
        if i == n - 1 {
            magnitude |= u64::from(b & 0x7f) << (8 * i);
        } else {
            magnitude |= u64::from(b) << (8 * i);
        }
    }
    let negative = bytes[n - 1] & 0x80 != 0;
    if magnitude > i64::MAX as u64 {
        return Err(ScriptError::InvalidScriptNumber);
    }
    Ok(if negative {
        -(magnitude as i64)
    } else {
        magnitude as i64
    })
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
mod tests;
