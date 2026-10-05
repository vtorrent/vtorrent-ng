# Wallet Organization: Address Book & Transaction Notes — Design

Status: DRAFT (no consensus change; wallet + RPC + UI)
Scope: `vtorrent-wallet`, `vtorrent-rpc`, `vtorrent-ui`.
Motivation: the wallet has address `label`s (`wallet.rs:35`, `generate_key(label)`)
and a `SendRequest.memo` (`models.rs:164`), but there is **no address book**
(contacts, independent of your own keys), the **memo is not persisted or
displayed**, and `get_transactions` (`handlers/wallet.rs:594`) only queries the
**change address** — so history is incomplete. This makes the wallet usable for
real bookkeeping.

## 1. What exists

- `WalletKeyEntry.label` (`wallet.rs:35`) — labels on *your* addresses.
- `generate_key(label)` (`:203`), `list_addresses` (`:331`).
- `SendRequest.memo` (`models.rs:164`) — accepted but not stored/returned.
- `get_transactions` (`:594`) queries only `wallet_change_address`.

## 2. Design

### 2.1 Address book (contacts)

- A **contacts** list, separate from wallet keys:
  `Contact { name, address, note, created_at }`.
- Add/edit/delete; pick a contact when sending (pre-fills the address).
- **Name-service integration** (`docs/name-service-design.md`): a contact can be a
  `.vtr` name, resolved at send time with the resolved address shown.
- **Anti-phishing**: show the resolved address, warn if a contact's address
  changed, and never hide the address behind the name.

### 2.2 Transaction notes

- Persist the `memo` per txid (a `tx_notes: HashMap<txid, String>` in the wallet
  data), and **display it** in history.
- Allow adding/editing a note on any tx (sent or received) after the fact.
- Notes are **local only** (never broadcast) — a privacy property to state.

### 2.3 Complete history

- Fix `get_transactions` to query **all wallet addresses** (not just the change
  address), so history is complete. Reuse `get_recent_transactions_for_addresses`
  with the full address set.
- Show per-tx: direction, counterparty (contact name if known), amount, fee,
  confirmations, note.

### 2.4 UI

- An **Address Book** page; a contact picker in Send; notes inline in history;
  search/filter by contact, note, amount, date.

## 3. Adversarial review

- **R1 — the change-address-only history is a bug.** Fixing it is the highest-value
  part; without it, users see an incomplete history and lose trust. Test with a
  wallet that has multiple addresses.
- **R2 — notes are private.** Never broadcast a memo on-chain (unless explicitly
  requested, and then warn); keep notes in the encrypted wallet file.
- **R3 — contact address change is a phishing vector.** Warn loudly if a saved
  contact's address changes; require confirmation.
- **R4 — name resolution at send time.** Resolve `.vtr` names at send, show the
  address, and warn on fresh registrations (`docs/name-service-design.md` R1).
- **R5 — persistence/migration.** `tx_notes`/`contacts` use `#[serde(default)]` so
  old wallet files load unchanged.
- **R6 — no consensus change.** Wallet/RPC/UI only.

## 4. Test plan

- History includes txs to/from **all** wallet addresses (not just change).
- Memo is persisted and displayed; editing a note works; notes never broadcast.
- Contacts CRUD; send-to-contact pre-fills; changed-address warning fires.
- `.vtr` contact resolves at send with the address shown.
- Old wallet files load (serde default).
- No consensus/chain change.

## 5. Non-goals

- Not a CRM / not cloud-synced contacts.
- Not on-chain memos by default.
- Not part of the current soak window.

## 6. References

- `vtorrent-wallet/src/wallet.rs:35,203,331` labels/generate/list.
- `vtorrent-rpc/src/models.rs:164` `SendRequest.memo`.
- `vtorrent-rpc/src/handlers/wallet.rs:594` `get_transactions` (change-address bug).
- `docs/name-service-design.md`, `docs/payment-requests-design.md`.
