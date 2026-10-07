import { useEffect, useRef, useState } from 'react'
import { CheckCircle, AlertTriangle, Info, XCircle, X } from 'lucide-react'
import { useNotifications, type NotifySeverity } from '../hooks/useNotifications'

const ICONS: Record<NotifySeverity, typeof Info> = {
  info: Info,
  success: CheckCircle,
  warning: AlertTriangle,
  critical: XCircle,
}

const COLORS: Record<NotifySeverity, string> = {
  info: 'border-sky-500/40 text-sky-300',
  success: 'border-emerald-500/40 text-emerald-300',
  warning: 'border-amber-500/40 text-amber-300',
  critical: 'border-red-500/50 text-red-300',
}

/** Transient toasts for the newest notifications (top-right). */
export default function Toasts() {
  const { notifications } = useNotifications()
  const [dismissed, setDismissed] = useState<number[]>([])
  const timers = useRef<Map<number, ReturnType<typeof setTimeout>>>(new Map())

  const newest = notifications.slice(0, 3)

  // Schedule auto-dismiss for each newly-seen toast. The dismissal is a timer
  // callback (not a direct effect setState), so it does not trip the
  // set-state-in-effect lint.
  useEffect(() => {
    const map = timers.current
    for (const n of newest) {
      if (map.has(n.id)) continue
      const t = setTimeout(() => {
        setDismissed(d => [...d, n.id])
        map.delete(n.id)
      }, 6_000)
      map.set(n.id, t)
    }
    return () => {
      for (const t of map.values()) clearTimeout(t)
      map.clear()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [notifications])

  const shown = newest.filter(n => !dismissed.includes(n.id))
  if (shown.length === 0) return null

  return (
    <div className="fixed top-4 right-4 z-50 space-y-2 w-80">
      {shown.map(n => {
        const Icon = ICONS[n.severity]
        return (
          <div
            key={n.id}
            className={`card border ${COLORS[n.severity]} flex items-start gap-2 shadow-lg`}
            role="status"
          >
            <Icon size={16} className="mt-0.5 shrink-0" />
            <div className="flex-1 min-w-0">
              <div className="text-sm font-medium">{n.title}</div>
              {n.body && <div className="text-xs text-slate-400 truncate">{n.body}</div>}
            </div>
            <button
              onClick={() => setDismissed(d => [...d, n.id])}
              className="text-slate-500 hover:text-slate-300"
              aria-label="Dismiss"
            >
              <X size={14} />
            </button>
          </div>
        )
      })}
    </div>
  )
}
