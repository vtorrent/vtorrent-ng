export interface RewardPoint {
  height: number
  timestamp: number
  rewardSats: number
}

export function dailyAvgReward(rewards: RewardPoint[], nowSecs: number): number {
  if (rewards.length === 0) return 0
  const oldest = Math.min(...rewards.map(r => r.timestamp))
  const spanSecs = Math.max(1, nowSecs - oldest)
  const total = rewards.reduce((sum, r) => sum + r.rewardSats, 0)
  return (total / spanSecs) * 86400
}

export function maturityCountdown(confirmations: number, required = 100): number {
  return Math.max(0, required - confirmations)
}

export function healthSummary(info: {
  blockHeight: number
  connections: number
  syncing: boolean
  syncPercent: number
  mempoolSize: number
}): string {
  const sync = info.syncing ? `syncing ${info.syncPercent.toFixed(1)}%` : 'synced'
  return `height ${info.blockHeight} · ${info.connections} peers · ${sync} · mempool ${info.mempoolSize}`
}

export function stakingStartError(raw: string): string {
  const lower = raw.toLowerCase()
  if (lower.includes('wallet') && lower.includes('lock')) {
    return 'Wallet is locked. Unlock first, then start staking.'
  }
  if (lower.includes('no address') || lower.includes('address is required')) {
    return 'No staking address selected. Unlock your wallet first.'
  }
  return raw
}
