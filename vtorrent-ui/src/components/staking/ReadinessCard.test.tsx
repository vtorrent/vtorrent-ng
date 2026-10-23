// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest'
import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'
import ReadinessCard from './ReadinessCard'
import type { NodeInfo, StakingStatus } from '../../hooks/useNode'

const node: NodeInfo = {
  version: '2.0.0-beta.3',
  network: 'vtorrent-regtest',
  blockHeight: 100,
  bestBlockHash: '00',
  connections: 2,
  syncing: false,
  syncPercent: 100,
  mempoolSize: 0,
  uptimeSecs: 60,
}

function status(overrides: Partial<StakingStatus> = {}): StakingStatus {
  return {
    enabled: false,
    stakingAddress: 'VStake',
    eligibleUtxos: 0,
    totalStakingSatoshis: 0,
    eligibleStakingSatoshis: 0,
    immatureUtxos: 0,
    immatureStakingSatoshis: 0,
    expiredUtxos: 0,
    expiredStakingSatoshis: 0,
    nextMaturityAt: null,
    nextMaturityInSeconds: null,
    minStakeAgeSeconds: 21_600,
    maxStakeAgeSeconds: 518_400,
    expectedRewardPerDay: 0,
    lastStakeTime: null,
    blocksStaked: 0,
    owned: true,
    walletUnlocked: true,
    warning: null,
    ...overrides,
  }
}

describe('staking readiness card', () => {
  afterEach(cleanup)

  it('asks for an unlocked wallet first', () => {
    render(
      <ReadinessCard
        status={status({ walletUnlocked: false })}
        node={node}
        totalBalance={0}
        effectiveAddress="VStake"
        isEnabled={false}
      />
    )
    expect(screen.getByText('Unlock the wallet')).toBeInTheDocument()
  })

  it('explains immature funds and the next maturity', () => {
    render(
      <ReadinessCard
        status={status({
          immatureUtxos: 1,
          immatureStakingSatoshis: 100_000_000,
          nextMaturityInSeconds: 3600,
        })}
        node={node}
        totalBalance={100_000_000}
        effectiveAddress="VStake"
        isEnabled={false}
      />
    )
    expect(screen.getByText('Wait for funds to mature')).toBeInTheDocument()
    expect(screen.getByText(/1 VTR matures in 1h/)).toBeInTheDocument()
  })

  it('shows ready to stake when eligible funds exist', () => {
    render(
      <ReadinessCard
        status={status({
          eligibleUtxos: 1,
          eligibleStakingSatoshis: 100_000_000,
          totalStakingSatoshis: 100_000_000,
        })}
        node={node}
        totalBalance={100_000_000}
        effectiveAddress="VStake"
        isEnabled={false}
      />
    )
    expect(screen.getByText('Ready to stake')).toBeInTheDocument()
  })
})
