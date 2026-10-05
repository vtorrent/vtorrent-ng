# Notifications & Alerting — Design

Status: DRAFT (no consensus change; RPC/daemon + UI)
Scope: `vtorrent-rpc`/`vtorrent-daemon` (notification service), `vtorrent-ui`
(toasts + feed), `vtorrent-tauri` (desktop notifications).
Motivation: several events are **time-critical or money-critical** and are
currently invisible unless the user is staring at the right page — an incoming
payment, a staking reward, a swap that needs funding/claiming before a deadline,
a torrent finishing, or staking silently stopping.

## 1. What exists

- `NodeEvent` (`vtorrent-node/src/events.rs:18`) already carries `NewBlock`,
  `TxConfirmed`, `TxUnconfirmed`, `PeerConnected`, … and is bridged to the RPC
  WebSocket broadcaster (`main.rs` event bridge).
- The UI has **no toast/notification system** — only inline page alerts.
- Swap states (`SwapStatus`) and torrent sessions (`TorrentSession`) are polled
  per page; nothing pushes.

So the transport exists; the gap is **classification, delivery, persistence, and
UI**.

## 2. Design

### 2.1 Notification model

```rust
pub enum Category { Payment, Staking, Swap, Torrent, Node }
pub enum Severity { Info, Success, Warning, Critical }

pub struct Notification {
    pub id: u64,
    pub category: Category,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub action: Option<NotificationAction>, // e.g. deep-link to the swap
    pub created_at: u64,
    pub read: bool,
}
```

### 2.2 Sources → notifications

| Event | Category | Severity | Action |
|---|---|---|---|
| `TxConfirmed` to a wallet address | Payment | Success | open tx |
| staking reward (coinstake to us) | Staking | Success | open staking |
| staking stopped / no eligible UTXOs | Staking | Warning | open staking |
| swap needs funding / claim / refund window closing | Swap | **Critical** | deep-link to swap |
| swap completed | Swap | Success | open trade |
| torrent completed | Torrent | Info | open torrents |
| peer count 0 / sync stalled | Node | Warning | open dashboard |

### 2.3 Service

- A **notification service** in the daemon subscribes to the existing event
  bridge, classifies events, **dedupes** (e.g. one "staking stopped" until it
  resumes), persists a bounded feed (ring buffer + optional disk), and pushes to
  the UI over the existing WebSocket.
- **Swap deadlines** are the important case: a periodic check computes
  time-to-deadline and emits escalating notifications (T-24h, T-1h) — a swap
  refund/claim window is money at risk.
- Delivery channels: in-app (always), **desktop** via Tauri's notification API
  (opt-in), and optional **webhook** (for node operators) — feature-gated.

### 2.4 UI

- A **toast** component (top-right), and a **Notifications** page/panel with the
  feed, filters by category, mark-read, and **action buttons** that deep-link.
- Per-category preferences (in-app / desktop / off) and quiet hours, persisted
  with the wallet config.

## 3. Adversarial review

- **R1 — notification storms.** A busy chain or a torrent swarm can emit
  thousands of events. Dedupe + rate-limit per category; never one toast per
  block. Batch "N new blocks" instead of N toasts.
- **R2 — don't miss the money-critical ones.** Swap deadlines must not be
  rate-limited away. Give `Critical` a separate, guaranteed path and test that
  a deadline notification always fires.
- **R3 — privacy on the desktop.** OS notifications may be visible on a lock
  screen; default to **no amounts** in desktop notifications (or opt-in), and
  never include keys/addresses beyond what the user chose.
- **R4 — don't block the event loop.** Classification/delivery must be
  off the hot path (the event bridge already appends to the store); a slow
  webhook must not stall block processing.
- **R5 — persistence bounds.** The feed is a ring buffer (bounded memory); disk
  persistence is optional and capped.
- **R6 — no consensus/security surface.** Read-only fan-out of existing events.

## 4. Test plan

- Classifier: each source event maps to the expected category/severity.
- Dedupe: repeated "staking stopped" yields one notification until resume.
- Swap deadline: a swap at T-1h emits a Critical notification even under load.
- Storm: 1,000 blocks yield a bounded number of notifications.
- Desktop notifications omit amounts by default.
- Feed is bounded (ring buffer) and survives restart if persisted.

## 5. Non-goals

- Not push notifications to a mobile app (a follow-on).
- Not changing any event semantics.
- Not part of the current soak window.

## 6. References

- `vtorrent-node/src/events.rs:18` `NodeEvent`.
- `vtorrent-daemon/src/main.rs` event bridge → RPC WS broadcaster.
- `vtorrent-rpc/src/handlers/swap.rs` `SwapStatus`; `handlers/torrent.rs`.
- `vtorrent-ui/src/components/Layout.tsx` (nav); `pages/`.
