import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from 'react'
import { RPC_BASE } from '../api'

// ─── Model ────────────────────────────────────────────────────────────────────

export type NotifyCategory = 'payment' | 'staking' | 'swap' | 'torrent' | 'node'
export type NotifySeverity = 'info' | 'success' | 'warning' | 'critical'

export interface Notification {
  id: number
  category: NotifyCategory
  severity: NotifySeverity
  title: string
  body: string
  createdAt: number
  read: boolean
}

interface NotificationsContextType {
  notifications: Notification[]
  unread: number
  markAllRead: () => void
  clear: () => void
  /** Push a notification (also used by non-WS sources). */
  push: (n: Omit<Notification, 'id' | 'createdAt' | 'read'>) => void
}

const NotificationsContext = createContext<NotificationsContextType | null>(null)

const MAX_FEED = 100
/** Rate-limit per category: at most one toast per category per this window. */
const CATEGORY_THROTTLE_MS = 5_000

export function NotificationsProvider({ children }: { children: React.ReactNode }) {
  const [notifications, setNotifications] = useState<Notification[]>([])
  const idRef = useRef(0)
  const lastByCategory = useRef<Record<string, number>>({})

  const push = useCallback(
    (n: Omit<Notification, 'id' | 'createdAt' | 'read'>) => {
      const now = Date.now()
      // Dedupe/rate-limit per category so a busy chain or swarm can't storm the
      // feed. Critical notifications bypass the throttle.
      if (n.severity !== 'critical') {
        const last = lastByCategory.current[n.category] ?? 0
        if (now - last < CATEGORY_THROTTLE_MS) return
      }
      lastByCategory.current[n.category] = now
      setNotifications(prev => {
        const next: Notification = {
          ...n,
          id: ++idRef.current,
          createdAt: now,
          read: false,
        }
        const feed = [next, ...prev]
        return feed.length > MAX_FEED ? feed.slice(0, MAX_FEED) : feed
      })
    },
    [],
  )

  const markAllRead = useCallback(() => {
    setNotifications(prev => prev.map(x => ({ ...x, read: true })))
  }, [])

  const clear = useCallback(() => setNotifications([]), [])

  // ── Subscribe to node events over the existing WebSocket ──────────────────
  useEffect(() => {
    let closed = false
    let ws: WebSocket | null = null
    let reconnect: ReturnType<typeof setTimeout> | null = null

    const connect = () => {
      if (closed) return
      try {
        ws = new WebSocket(`${RPC_BASE.replace(/^http/, 'ws')}/ws`)
        ws.onopen = () => {
          try {
            ws?.send(
              JSON.stringify({
                subscribe: ['tx_confirmed', 'staking_reward', 'reorg', 'peer_disconnected'],
              }),
            )
          } catch {
            /* ignore */
          }
        }
        ws.onmessage = e => {
          try {
            const ev = JSON.parse((e as MessageEvent).data as string)
            const type = String(ev.event ?? ev.type ?? '').toLowerCase()
            const data = ev.data ?? ev.payload ?? ev
            if (type === 'tx_confirmed') {
              push({
                category: 'payment',
                severity: 'success',
                title: 'Transaction confirmed',
                body: `Block ${data.block_height ?? '?'}`,
              })
            } else if (type === 'staking_reward') {
              push({
                category: 'staking',
                severity: 'success',
                title: 'Staking reward',
                body: `Block ${data.block_height ?? '?'}`,
              })
            } else if (type === 'reorg') {
              push({
                category: 'node',
                severity: 'warning',
                title: 'Chain reorganization',
                body: `Depth ${data.depth ?? '?'}`,
              })
            } else if (type === 'peer_disconnected') {
              push({
                category: 'node',
                severity: 'info',
                title: 'Peer disconnected',
                body: String(data.addr ?? ''),
              })
            }
          } catch {
            /* ignore */
          }
        }
        ws.onclose = () => {
          ws = null
          if (!closed) reconnect = setTimeout(connect, 3000)
        }
      } catch {
        if (!closed) reconnect = setTimeout(connect, 3000)
      }
    }
    connect()
    return () => {
      closed = true
      if (reconnect) clearTimeout(reconnect)
      ws?.close()
    }
  }, [push])

  // ── Poll swap deadlines (money-critical) ──────────────────────────────────
  useEffect(() => {
    let stopped = false
    const check = async () => {
      try {
        const { getSwapDeadlines } = await import('./useNode')
        const deadlines = await getSwapDeadlines()
        if (stopped) return
        for (const d of deadlines) {
          if (!d.atRisk) continue
          // Escalate: critical under 1h, warning under 24h.
          if (d.secondsRemaining <= 3600) {
            push({
              category: 'swap',
              severity: 'critical',
              title: 'Swap expires within 1 hour',
              body: `Order ${d.orderId.slice(0, 12)}… — claim or refund now`,
            })
          } else if (d.secondsRemaining <= 86_400) {
            push({
              category: 'swap',
              severity: 'warning',
              title: 'Swap expires within 24 hours',
              body: `Order ${d.orderId.slice(0, 12)}… — ${Math.round(d.secondsRemaining / 3600)}h left`,
            })
          }
        }
      } catch {
        /* ignore */
      }
    }
    void check()
    const timer = setInterval(check, 60_000)
    return () => {
      stopped = true
      clearInterval(timer)
    }
  }, [push])

  const unread = notifications.filter(n => !n.read).length

  return (
    <NotificationsContext.Provider
      value={{ notifications, unread, markAllRead, clear, push }}
    >
      {children}
    </NotificationsContext.Provider>
  )
}

export function useNotifications(): NotificationsContextType {
  const ctx = useContext(NotificationsContext)
  if (!ctx) throw new Error('useNotifications must be used within NotificationsProvider')
  return ctx
}
