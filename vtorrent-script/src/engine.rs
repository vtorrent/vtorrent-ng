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
mod tests {
    use super::*;
    use crate::standard::{build_p2ms, build_p2pkh};
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
    fn test_truncated_push_fails_script() {
        // A redeem script of `OP_TRUE` followed by a PUSHDATA1 claiming 255
        // bytes with no data must FAIL, not silently evaluate to true.
        let script_sig = Script::new();
        let mut script_pubkey = Script::new();
        script_pubkey.push_opcode(0x51); // OP_TRUE
        script_pubkey.push_opcode(0x4c); // OP_PUSHDATA1
        script_pubkey.push_opcode(0xff); // declared length 255, no data follows

        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        let result = engine.execute(&script_sig, &script_pubkey);
        assert!(result.is_err(), "truncated push must fail the script");
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

    // ── Differential / coverage tests for previously-untested opcodes ──────

    fn run_op(opcode: u8) -> Result<()> {
        let script_sig = Script::new();
        let mut script_pubkey = Script::new();
        script_pubkey.push_opcode(opcode);
        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine.execute(&script_sig, &script_pubkey)
    }

    #[test]
    fn test_op_1negate() {
        // OP_1NEGATE pushes -1 (truthy)
        run_op(0x4f).expect("OP_1NEGATE should succeed");
    }

    #[test]
    fn test_op_verify_succeeds() {
        let mut sig = Script::new();
        sig.push_opcode(0x51); // OP_1
        let mut sp = Script::new();
        sp.push_opcode(0x51); // OP_1
        sp.push_opcode(0x69); // OP_VERIFY (consumes 1, passes)
        sp.push_opcode(0x51); // OP_1 (leaves 1 for execute's final check)
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_VERIFY on true should pass");
    }

    #[test]
    fn test_op_verify_fails_on_false() {
        let sig = Script::new();
        let mut sp = Script::new();
        sp.push_opcode(0x00); // OP_0
        sp.push_opcode(0x69); // OP_VERIFY (consumes 0, fails)
        sp.push_opcode(0x51); // OP_1 (never reached)
        let mut e = Engine::new(ScriptEnv::default());
        assert!(
            e.execute(&sig, &sp).is_err(),
            "OP_VERIFY on false must fail"
        );
    }

    #[test]
    fn test_op_toalt_fromalt() {
        let mut sig = Script::new();
        sig.push_opcode(0x51); // OP_1
        let mut sp = Script::new();
        sp.push_opcode(0x6b); // TOALTSTACK
        sp.push_opcode(0x6c); // FROMALTSTACK
                              // After: stack has [1], execute checks top → true
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp)
            .expect("TOALTSTACK/FROMALTSTACK roundtrip should succeed");
    }

    #[test]
    fn test_op_2drop() {
        let mut sig = Script::new();
        sig.push_opcode(0x51); // OP_1
        sig.push_opcode(0x52); // OP_2
        let mut sp = Script::new();
        sp.push_opcode(0x6d); // OP_2DROP (drops both)
        sp.push_opcode(0x51); // OP_1 (leaves 1 for execute)
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_2DROP should succeed");
    }

    #[test]
    fn test_op_2dup() {
        let mut sig = Script::new();
        sig.push_opcode(0x51); // OP_1
        sig.push_opcode(0x52); // OP_2
        let mut sp = Script::new();
        sp.push_opcode(0x6e); // OP_2DUP → [1,2,1,2]
                              // execute checks top → 2 → true
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_2DUP should succeed");
    }

    #[test]
    fn test_op_ifdup_true() {
        let mut sig = Script::new();
        sig.push_opcode(0x51); // OP_1
        let mut sp = Script::new();
        sp.push_opcode(0x73); // OP_IFDUP → [1,1]
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp)
            .expect("OP_IFDUP on true should succeed");
    }

    #[test]
    fn test_op_ifdup_false() {
        let mut sig = Script::new();
        sig.push_opcode(0x00); // OP_0
        let mut sp = Script::new();
        sp.push_opcode(0x73); // OP_IFDUP → [0] (no dup)
        sp.push_opcode(0x51); // OP_1 (for execute final check)
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp)
            .expect("OP_IFDUP on false should succeed");
    }

    #[test]
    fn test_op_over() {
        let mut sig = Script::new();
        sig.push_opcode(0x51); // OP_1
        sig.push_opcode(0x52); // OP_2
        let mut sp = Script::new();
        sp.push_opcode(0x79); // OP_OVER → [1,2,1]
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_OVER should succeed");
    }

    #[test]
    fn test_op_rot() {
        let mut sig = Script::new();
        sig.push_opcode(0x51); // 1
        sig.push_opcode(0x52); // 2
        sig.push_opcode(0x53); // 3
        let mut sp = Script::new();
        sp.push_opcode(0x7b); // OP_ROT → [2,3,1]
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_ROT should succeed");
    }

    #[test]
    fn test_op_swap() {
        let mut sig = Script::new();
        sig.push_opcode(0x51); // 1
        sig.push_opcode(0x52); // 2
        let mut sp = Script::new();
        sp.push_opcode(0x7c); // OP_SWAP → [2,1]
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_SWAP should succeed");
    }

    #[test]
    fn test_op_size() {
        let mut sig = Script::new();
        sig.push_data(b"hello").unwrap(); // 5 bytes
        let mut sp = Script::new();
        sp.push_opcode(0x82); // OP_SIZE → pushes 5
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_SIZE should succeed");
    }

    #[test]
    fn test_op_1add() {
        let mut sig = Script::new();
        sig.push_opcode(0x51); // OP_1
        let mut sp = Script::new();
        sp.push_opcode(0x8b); // OP_1ADD → 2
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_1ADD should succeed");
    }

    #[test]
    fn test_op_1sub() {
        let mut sig = Script::new();
        sig.push_opcode(0x52); // OP_2
        let mut sp = Script::new();
        sp.push_opcode(0x8c); // OP_1SUB → 1
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_1SUB should succeed");
    }

    #[test]
    fn test_op_negate() {
        let mut sig = Script::new();
        sig.push_opcode(0x51); // OP_1
        let mut sp = Script::new();
        sp.push_opcode(0x8f); // OP_NEGATE → -1 (truthy)
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_NEGATE should succeed");
    }

    #[test]
    fn test_op_abs() {
        let mut sig = Script::new();
        sig.push_opcode(0x51); // OP_1
        let mut sp = Script::new();
        sp.push_opcode(0x8f); // OP_NEGATE → -1
        sp.push_opcode(0x90); // OP_ABS → 1
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_ABS should succeed");
    }

    #[test]
    fn test_op_not_zero() {
        let mut sig = Script::new();
        sig.push_opcode(0x00); // OP_0
        let mut sp = Script::new();
        sp.push_opcode(0x91); // OP_NOT → 1 (true)
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_NOT(0) should be true");
    }

    #[test]
    fn test_op_not_nonzero_fails() {
        let mut sig = Script::new();
        sig.push_opcode(0x51); // OP_1
        let mut sp = Script::new();
        sp.push_opcode(0x91); // OP_NOT → 0 (false)
        let mut e = Engine::new(ScriptEnv::default());
        assert!(
            e.execute(&sig, &sp).is_err(),
            "OP_NOT(1) = 0, execute should fail"
        );
    }

    #[test]
    fn test_op_add() {
        let mut sig = Script::new();
        sig.push_opcode(0x52); // 2
        sig.push_opcode(0x53); // 3
        let mut sp = Script::new();
        sp.push_opcode(0x93); // OP_ADD → 5
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_ADD should succeed");
    }

    #[test]
    fn test_op_notif_true() {
        // OP_1 OP_NOTIF → false branch (push 0), OP_ENDIF → stack [0] → execute fails
        let mut sig = Script::new();
        sig.push_opcode(0x51); // OP_1
        let mut sp = Script::new();
        sp.push_opcode(0x64); // OP_NOTIF (1 is true → NOTIF → false branch)
        sp.push_opcode(0x00); // OP_0 (executed in false branch)
        sp.push_opcode(0x68); // OP_ENDIF
        sp.push_opcode(0x51); // OP_1 (for execute final check)
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp)
            .expect("OP_NOTIF true → false branch should execute");
    }

    #[test]
    fn test_op_notif_false() {
        // OP_0 OP_NOTIF → true branch (push 1), OP_ENDIF → stack [1] → execute passes
        let mut sig = Script::new();
        sig.push_opcode(0x00); // OP_0
        let mut sp = Script::new();
        sp.push_opcode(0x64); // OP_NOTIF (0 is false → NOTIF → true branch)
        sp.push_opcode(0x51); // OP_1 (executed in true branch)
        sp.push_opcode(0x68); // OP_ENDIF
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp)
            .expect("OP_NOTIF false → true branch should execute");
    }

    #[test]
    fn test_op_sub() {
        let mut sig = Script::new();
        sig.push_opcode(0x53); // 3
        sig.push_opcode(0x52); // 2
        let mut sp = Script::new();
        sp.push_opcode(0x94); // OP_SUB → 1
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_SUB should succeed");
    }

    #[test]
    fn test_op_1negate_abs() {
        let sig = Script::new();
        let mut sp = Script::new();
        sp.push_opcode(0x4f); // OP_1NEGATE → -1
        sp.push_opcode(0x90); // OP_ABS → 1
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp)
            .expect("OP_1NEGATE + OP_ABS should succeed");
    }

    #[test]
    fn test_op_within_true() {
        let sig = Script::new();
        let mut sp = Script::new();
        sp.push_data(&5i64.to_le_bytes()[..1]).unwrap(); // x = 5
        sp.push_data(&1i64.to_le_bytes()[..1]).unwrap(); // min = 1
        sp.push_data(&10i64.to_le_bytes()[..1]).unwrap(); // max = 10
        sp.push_opcode(0xa5); // OP_WITHIN → 1 (1 <= 5 < 10)
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp)
            .expect("5 within [1,10) should succeed");
    }

    #[test]
    fn test_op_within_false_below_min() {
        let sig = Script::new();
        let mut sp = Script::new();
        sp.push_data(&0i64.to_le_bytes()[..1]).unwrap(); // x = 0
        sp.push_data(&1i64.to_le_bytes()[..1]).unwrap(); // min = 1
        sp.push_data(&10i64.to_le_bytes()[..1]).unwrap(); // max = 10
        sp.push_opcode(0xa5); // OP_WITHIN → 0 (0 < 1)
        sp.push_opcode(0x51); // OP_1 (for execute final check)
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp)
            .expect("0 within [1,10) should evaluate without error");
    }

    #[test]
    fn test_op_ripemd160() {
        let sig = Script::new();
        let mut sp = Script::new();
        sp.push_data(b"hello").unwrap();
        sp.push_opcode(0xa6); // OP_RIPEMD160
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_RIPEMD160 should succeed");
        let result = e.stack.last().unwrap();
        assert_eq!(result.len(), 20);
        let expected: [u8; 20] = [
            0x10, 0x8f, 0x07, 0xb8, 0x38, 0x24, 0x12, 0x61, 0x2c, 0x04, 0x8d, 0x07, 0xd1, 0x3f,
            0x81, 0x41, 0x18, 0x44, 0x5a, 0xcd,
        ];
        assert_eq!(result.as_slice(), &expected);
    }

    #[test]
    fn test_op_sha1() {
        let sig = Script::new();
        let mut sp = Script::new();
        sp.push_data(b"hello").unwrap();
        sp.push_opcode(0xa7); // OP_SHA1
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_SHA1 should succeed");
        let result = e.stack.last().unwrap();
        assert_eq!(result.len(), 20);
        let expected: [u8; 20] = [
            0xaa, 0xf4, 0xc6, 0x1d, 0xdc, 0xc5, 0xe8, 0xa2, 0xda, 0xbe, 0xde, 0x0f, 0x3b, 0x48,
            0x2c, 0xd9, 0xae, 0xa9, 0x43, 0x4d,
        ];
        assert_eq!(result.as_slice(), &expected);
    }

    #[test]
    fn test_op_hash160() {
        let sig = Script::new();
        let mut sp = Script::new();
        sp.push_data(b"hello").unwrap();
        sp.push_opcode(0xa9); // OP_HASH160
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_HASH160 should succeed");
        let result = e.stack.last().unwrap();
        assert_eq!(result.len(), 20);
        let expected: [u8; 20] = [
            0xb6, 0xa9, 0xc8, 0xc2, 0x30, 0x72, 0x2b, 0x7c, 0x74, 0x83, 0x31, 0xa8, 0xb4, 0x50,
            0xf0, 0x55, 0x66, 0xdc, 0x7d, 0x0f,
        ];
        assert_eq!(result.as_slice(), &expected);
    }

    #[test]
    fn test_op_hash256() {
        let sig = Script::new();
        let mut sp = Script::new();
        sp.push_data(b"hello").unwrap();
        sp.push_opcode(0xaa); // OP_HASH256
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_HASH256 should succeed");
        let result = e.stack.last().unwrap();
        assert_eq!(result.len(), 32);
        let expected: [u8; 32] = [
            0x95, 0x95, 0xc9, 0xdf, 0x90, 0x07, 0x51, 0x48, 0xeb, 0x06, 0x86, 0x03, 0x65, 0xdf,
            0x33, 0x58, 0x4b, 0x75, 0xbf, 0xf7, 0x82, 0xa5, 0x10, 0xc6, 0xcd, 0x48, 0x83, 0xa4,
            0x19, 0x83, 0x3d, 0x50,
        ];
        assert_eq!(result.as_slice(), &expected);
    }

    #[test]
    fn test_op_equalverify_pass() {
        let sig = Script::new();
        let mut sp = Script::new();
        sp.push_data(b"abc").unwrap();
        sp.push_data(b"abc").unwrap();
        sp.push_opcode(0x88); // OP_EQUALVERIFY
        sp.push_opcode(0x51); // OP_1 (leave truthy for execute)
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp)
            .expect("OP_EQUALVERIFY on equal items should pass");
    }

    #[test]
    fn test_op_equalverify_fail() {
        let sig = Script::new();
        let mut sp = Script::new();
        sp.push_data(b"abc").unwrap();
        sp.push_data(b"xyz").unwrap();
        sp.push_opcode(0x88); // OP_EQUALVERIFY
        sp.push_opcode(0x51); // OP_1 (never reached)
        let mut e = Engine::new(ScriptEnv::default());
        assert!(
            e.execute(&sig, &sp).is_err(),
            "OP_EQUALVERIFY on different items must fail"
        );
    }

    #[test]
    fn test_op_mul() {
        let sig = Script::new();
        let mut sp = Script::new();
        sp.push_opcode(0x53); // 3
        sp.push_opcode(0x54); // 4
        sp.push_opcode(0x95); // OP_MUL → 12
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_MUL should succeed");
        assert_eq!(e.stack.last().unwrap(), &vec![12]);
    }

    #[test]
    fn test_op_div() {
        let sig = Script::new();
        let mut sp = Script::new();
        sp.push_opcode(0x5a); // 10
        sp.push_opcode(0x53); // 3
        sp.push_opcode(0x96); // OP_DIV → 3 (integer division)
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_DIV should succeed");
        assert_eq!(e.stack.last().unwrap(), &vec![3]);
    }

    #[test]
    fn test_op_mod() {
        let sig = Script::new();
        let mut sp = Script::new();
        sp.push_opcode(0x5a); // 10
        sp.push_opcode(0x53); // 3
        sp.push_opcode(0x97); // OP_MOD → 1
        let mut e = Engine::new(ScriptEnv::default());
        e.execute(&sig, &sp).expect("OP_MOD should succeed");
        assert_eq!(e.stack.last().unwrap(), &vec![1]);
    }

    // ── Multisig / P2SH tests ────────────────────────────────────────────────

    fn make_keypair_seed(seed: u8) -> (SecretKey, secp256k1::PublicKey) {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[seed; 32]).unwrap();
        let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
        (sk, pk)
    }

    #[test]
    fn test_2of3_multisig() {
        let (_, pk1) = make_keypair_seed(1);
        let (_, pk2) = make_keypair_seed(2);
        let (_, pk3) = make_keypair_seed(3);

        let tx_hash = [0xcd_u8; 32];

        let script_pubkey = build_p2ms(
            2,
            &[
                pk1.serialize().to_vec(),
                pk2.serialize().to_vec(),
                pk3.serialize().to_vec(),
            ],
        )
        .unwrap();

        let sig1 = sign_tx(&make_keypair_seed(1).0, &tx_hash);
        let sig2 = sign_tx(&make_keypair_seed(2).0, &tx_hash);

        let mut script_sig = Script::new();
        script_sig.push_opcode(0x00); // OP_0 dummy (Bitcoin CHECKMULTISIG bug)
        script_sig.push_data(&sig1).unwrap();
        script_sig.push_data(&sig2).unwrap();

        let env = ScriptEnv {
            tx_hash,
            ..Default::default()
        };
        let mut engine = Engine::new(env);
        engine
            .execute(&script_sig, &script_pubkey)
            .expect("2-of-3 multisig should succeed");
    }

    #[test]
    fn test_1of2_multisig() {
        let (_, pk1) = make_keypair_seed(10);
        let (_, pk2) = make_keypair_seed(20);

        let tx_hash = [0xef_u8; 32];

        let script_pubkey =
            build_p2ms(1, &[pk1.serialize().to_vec(), pk2.serialize().to_vec()]).unwrap();

        let sig = sign_tx(&make_keypair_seed(10).0, &tx_hash);

        let mut script_sig = Script::new();
        script_sig.push_opcode(0x00); // OP_0 dummy
        script_sig.push_data(&sig).unwrap();

        let env = ScriptEnv {
            tx_hash,
            ..Default::default()
        };
        let mut engine = Engine::new(env);
        engine
            .execute(&script_sig, &script_pubkey)
            .expect("1-of-2 multisig should succeed");
    }

    #[test]
    fn test_multisig_wrong_keys_fails() {
        let (_, pk1) = make_keypair_seed(1);
        let (_, pk2) = make_keypair_seed(2);
        let (_, pk3) = make_keypair_seed(3);

        let tx_hash = [0xab_u8; 32];

        let script_pubkey = build_p2ms(
            2,
            &[
                pk1.serialize().to_vec(),
                pk2.serialize().to_vec(),
                pk3.serialize().to_vec(),
            ],
        )
        .unwrap();

        // Sign with wrong keys (seeds 4 and 5, not part of the multisig)
        let sig1 = sign_tx(&make_keypair_seed(4).0, &tx_hash);
        let sig2 = sign_tx(&make_keypair_seed(5).0, &tx_hash);

        let mut script_sig = Script::new();
        script_sig.push_opcode(0x00); // OP_0 dummy
        script_sig.push_data(&sig1).unwrap();
        script_sig.push_data(&sig2).unwrap();

        let env = ScriptEnv {
            tx_hash,
            ..Default::default()
        };
        let mut engine = Engine::new(env);
        assert!(
            engine.execute(&script_sig, &script_pubkey).is_err(),
            "multisig with wrong keys must fail"
        );
    }

    #[test]
    fn test_multisig_insufficient_sigs_fails() {
        let (_, pk1) = make_keypair_seed(1);
        let (_, pk2) = make_keypair_seed(2);
        let (_, pk3) = make_keypair_seed(3);

        let tx_hash = [0xde_u8; 32];

        let script_pubkey = build_p2ms(
            2,
            &[
                pk1.serialize().to_vec(),
                pk2.serialize().to_vec(),
                pk3.serialize().to_vec(),
            ],
        )
        .unwrap();

        // Only 1 signature for a 2-of-3
        let sig = sign_tx(&make_keypair_seed(1).0, &tx_hash);

        let mut script_sig = Script::new();
        script_sig.push_opcode(0x00); // OP_0 dummy
        script_sig.push_data(&sig).unwrap();

        let env = ScriptEnv {
            tx_hash,
            ..Default::default()
        };
        let mut engine = Engine::new(env);
        assert!(
            engine.execute(&script_sig, &script_pubkey).is_err(),
            "multisig with insufficient sigs must fail"
        );
    }

    #[test]
    fn test_p2sh_roundtrip() {
        use ripemd::Ripemd160;

        // Build a simple redeem script: OP_1 (always true)
        let mut redeem_script = Script::new();
        redeem_script.push_opcode(0x51); // OP_1

        let redeem_bytes = redeem_script.as_bytes().to_vec();

        // Hash the redeem script: HASH160(redeem_script)
        let script_hash: [u8; 20] = {
            let sha = Sha256::digest(&redeem_bytes);
            Ripemd160::digest(sha).into()
        };

        // Build P2SH scriptPubKey: OP_HASH160 <hash> OP_EQUAL
        let mut script_pubkey = Script::new();
        script_pubkey.push_opcode(0xa9); // OP_HASH160
        script_pubkey.push_data(&script_hash).unwrap();
        script_pubkey.push_opcode(0x87); // OP_EQUAL

        // scriptSig: push the redeem script
        let mut script_sig = Script::new();
        script_sig.push_data(&redeem_bytes).unwrap();

        let env = ScriptEnv::default();
        let mut engine = Engine::new(env);
        engine
            .execute(&script_sig, &script_pubkey)
            .expect("P2SH roundtrip should succeed");
    }
}
