use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScriptError {
    #[error("Script execution failed: stack is empty")]
    EmptyStack,

    #[error("Script execution failed: top of stack is false")]
    VerifyFailed,

    #[error("Invalid opcode: 0x{0:02x}")]
    InvalidOpcode(u8),

    #[error("Stack overflow (max depth exceeded)")]
    StackOverflow,

    #[error("Unbalanced conditional (IF/ELSE/ENDIF mismatch)")]
    UnbalancedConditional,

    #[error("Invalid signature encoding")]
    InvalidSignature,

    #[error("Invalid public key encoding")]
    InvalidPublicKey,

    #[error("Signature verification failed")]
    SignatureVerification,

    #[error("Script too large ({0} bytes, max 10000)")]
    ScriptTooLarge(usize),

    #[error("Truncated push: length prefix exceeds script bounds")]
    TruncatedPush,

    #[error("Stack item too large ({0} bytes, max 520)")]
    PushTooLarge(usize),

    #[error("Multisig: {0} signatures required but only {1} keys provided")]
    MultisigKeyCount(usize, usize),

    #[error("Invalid script number")]
    InvalidScriptNumber,

    #[error("HTLC: hash preimage does not match")]
    HtlcHashMismatch,

    #[error("HTLC: locktime has not expired")]
    HtlcLocktimeNotExpired,

    #[error("OP_CHECKLOCKTIMEVERIFY: negative locktime")]
    NegativeLocktime,

    #[error("OP_CHECKLOCKTIMEVERIFY: unsatisfied (final input or type mismatch)")]
    UnsatisfiedLocktime,

    #[error(
        "OP_CHECKSEQUENCEVERIFY: unsatisfied (final input, type mismatch, or insufficient age)"
    )]
    UnsatisfiedSequence,

    #[error("OP_CHECKSEQUENCEVERIFY: negative locktime")]
    NegativeSequence,

    #[error("OP_RETURN output is unspendable")]
    OpReturnUnspendable,

    #[error("Script type not recognized")]
    UnknownScriptType,

    #[error("Serialization error: {0}")]
    Serialization(String),
}

pub type Result<T> = std::result::Result<T, ScriptError>;
