import { Bell, CheckCircle, AlertTriangle, Info, XCircle, Trash2 } from 'lucide-react'
import { useNotifications, type NotifySeverity } from '../hooks/useNotifications'

const ICONS: Record<NotifySeverity, typeof Info> = {
  info: Info,
  success: CheckCircle,
  warning: AlertTriangle,
  critical: XCircle,
}

const COLORS: Record<NotifySeverity, string> = {
  info: 'text-sky-400',
  success: 'text-emerald-400',
  warning: 'text-amber-400',
  critical: 'text-red-400',
}

function ago(ts: number): string {
  const d = Math.floor((Date.now() - ts) / 1000)
  if (d < 60) return `${d}s ago`
  if (d < 3600) return `${Math.floor(d / 60)}m ago`
  if (d < 86400) return `${Math.floor(d / 3600)}h ago`
  return `${Math.floor(d / 86400)}d ago`
}

export default function NotificationsPage() {
  const { notifications, markAllRead, clear } = useNotifications()

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold flex items-center gap-2">
          <Bell size={22} className="text-amber-400" /> Notifications
        </h1>
        <div className="flex gap-2">
          <button onClick={markAllRead} className="btn-secondary text-xs">
            Mark all read
          </button>
          <button onClick={clear} className="btn-secondary text-xs flex items-center gap-1">
            <Trash2 size={13} /> Clear
          </button>
        </div>
      </div>

      {notifications.length === 0 ? (
        <div className="card text-sm text-slate-500">No notifications yet.</div>
      ) : (
        <div className="space-y-2">
          {notifications.map(n => {
            const Icon = ICONS[n.severity]
            return (
              <div
                key={n.id}
                className={`card flex items-start gap-3 ${n.read ? 'opacity-60' : ''}`}
              >
                <Icon size={16} className={`mt-0.5 shrink-0 ${COLORS[n.severity]}`} />
                <div className="flex-1 min-w-0">
                  <div className="text-sm font-medium">{n.title}</div>
                  {n.body && <div className="text-xs text-slate-400">{n.body}</div>}
                </div>
                <div className="text-xs text-slate-500 shrink-0">{ago(n.createdAt)}</div>
              </div>
            )
          })}
        </div>
      )}
    </div>
  )
}
