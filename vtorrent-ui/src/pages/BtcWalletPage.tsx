import { useState } from 'react'
import { Bitcoin, RefreshCw, Send } from 'lucide-react'
import { useBtcStatus, useBtcAddress, useSendBtc } from '../hooks/useBtc'

export default function BtcWalletPage() {
  const { status } = useBtcStatus()
  const { address, generate, loading } = useBtcAddress()
  const { send, loading: sending, error: sendError } = useSendBtc()
  const [toAddress, setToAddress] = useState('')
  const [amount, setAmount] = useState('')
  const [sent, setSent] = useState<string | null>(null)

  const handleSend = async () => {
    const txid = await send(toAddress, Number(amount))
    if (txid) setSent(txid)
  }

  return (
    <div className="p-6 space-y-6">
      <h1 className="text-2xl font-bold flex items-center gap-2">
        <Bitcoin className="text-amber-400" /> Bitcoin Wallet
      </h1>

      <div className="grid grid-cols-3 gap-4">
        <div className="bg-navy-800 rounded-lg p-4">
          <p className="text-sm text-gray-400">Balance</p>
          <p className="text-2xl font-bold">
            {(status?.balanceSatoshis ?? 0) / 100_000_000} BTC
          </p>
        </div>
        <div className="bg-navy-800 rounded-lg p-4">
          <p className="text-sm text-gray-400">Sync Height</p>
          <p className="text-2xl font-bold">{status?.bestHeight ?? 0}</p>
        </div>
        <div className="bg-navy-800 rounded-lg p-4">
          <p className="text-sm text-gray-400">Status</p>
          <p className="text-2xl font-bold">
            {status?.initialized ? (status.synced ? 'Synced' : 'Syncing') : 'Not set up'}
          </p>
        </div>
      </div>

      <div className="bg-navy-800 rounded-lg p-4 space-y-3">
        <h2 className="font-semibold">Receive</h2>
        <div className="flex items-center gap-2">
          <code className="flex-1 bg-navy-900 p-2 rounded text-sm break-all">
            {address ?? 'Generate an address'}
          </code>
          <button
            onClick={generate}
            disabled={loading}
            className="p-2 bg-vtorrent-600 rounded hover:bg-vtorrent-500 disabled:opacity-50"
          >
            <RefreshCw size={16} />
          </button>
        </div>
      </div>

      <div className="bg-navy-800 rounded-lg p-4 space-y-3">
        <h2 className="font-semibold">Send</h2>
        <input
          value={toAddress}
          onChange={(e) => setToAddress(e.target.value)}
          placeholder="bc1q destination"
          className="w-full bg-navy-900 p-2 rounded text-sm"
        />
        <input
          value={amount}
          onChange={(e) => setAmount(e.target.value)}
          placeholder="amount in satoshis"
          className="w-full bg-navy-900 p-2 rounded text-sm"
        />
        <button
          onClick={handleSend}
          disabled={sending}
          className="flex items-center gap-2 px-4 py-2 bg-vtorrent-600 rounded hover:bg-vtorrent-500 disabled:opacity-50"
        >
          <Send size={16} /> {sending ? 'Sending...' : 'Send'}
        </button>
        {sendError && <p className="text-sm text-red-400">{sendError}</p>}
        {sent && <p className="text-sm text-green-400">Sent! TXID: {sent}</p>}
      </div>
    </div>
  )
}
