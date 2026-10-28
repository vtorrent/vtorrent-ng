// Live regtest E2E. Build the daemon with the test-only miner first:
//   cargo build -p vtorrent-daemon --features test-util
//   VTORRENT_E2E_LIVE=1 pnpm exec playwright test e2e/cold-stake.spec.ts

import { expect, test } from '@playwright/test'
import { spawn, type ChildProcess } from 'node:child_process'
import { createHash, generateKeyPairSync } from 'node:crypto'
import { existsSync } from 'node:fs'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'

const RPC = 'http://127.0.0.1:22525'
const API_KEY = 'e2e-test-key'
const PASSPHRASE = 'e2e-test-passphrase'
const DAEMON_BIN =
  process.env.VTORRENT_E2E_DAEMON_BIN ??
  path.resolve(process.cwd(), '../target/debug/vtorrent-daemon')

const BASE58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'

function base58Check(payload: Buffer): string {
  const checksum = createHash('sha256')
    .update(createHash('sha256').update(payload).digest())
    .digest()
    .subarray(0, 4)
  const data = Buffer.concat([payload, checksum])
  let value = BigInt(`0x${data.toString('hex')}`)
  let encoded = ''
  while (value > 0n) {
    encoded = BASE58[Number(value % 58n)] + encoded
    value /= 58n
  }
  for (const byte of data) {
    if (byte !== 0) break
    encoded = `1${encoded}`
  }
  return encoded
}

function regtestWallet(): { wif: string; address: string } {
  const { publicKey, privateKey } = generateKeyPairSync('ec', { namedCurve: 'secp256k1' })
  const secret = privateKey.export({ format: 'jwk' }) as { d: string }
  const pub = publicKey.export({ format: 'jwk' }) as { x: string; y: string }
  const raw = Buffer.from(secret.d, 'base64url')
  const x = Buffer.from(pub.x, 'base64url')
  const y = Buffer.from(pub.y, 'base64url')
  const compressed = Buffer.concat([Buffer.from([y[31] % 2 === 0 ? 0x02 : 0x03]), x])
  const hash160 = createHash('ripemd160')
    .update(createHash('sha256').update(compressed).digest())
    .digest()
  return {
    wif: base58Check(Buffer.concat([Buffer.from([198]), raw, Buffer.from([1])])),
    address: base58Check(Buffer.concat([Buffer.from([70]), hash160])),
  }
}

async function rpc<T>(route: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${RPC}${route}`, {
    ...init,
    headers: {
      'Content-Type': 'application/json',
      'X-API-Key': API_KEY,
      ...(init?.headers ?? {}),
    },
  })
  if (!response.ok) {
    throw new Error(`${route} -> ${response.status}: ${await response.text()}`)
  }
  return response.json() as Promise<T>
}

let daemon: ChildProcess | undefined
let dataDir: string | undefined
let daemonLog = ''
let walletAddress = ''

test.skip(
  process.env.VTORRENT_E2E_LIVE !== '1',
  'set VTORRENT_E2E_LIVE=1 to run the live regtest cold-stake journey',
)

test.beforeAll(async () => {
  if (!existsSync(DAEMON_BIN)) {
    throw new Error(
      `daemon binary not found at ${DAEMON_BIN}; build it with ` +
        '`cargo build -p vtorrent-daemon --features test-util` or set VTORRENT_E2E_DAEMON_BIN',
    )
  }

  dataDir = await mkdtemp(path.join(tmpdir(), 'vtr-e2e-'))
  daemon = spawn(
    DAEMON_BIN,
    [
      '--regtest',
      '--isolated',
      '--listen',
      '127.0.0.1:22526',
      '--rpc-addr',
      '127.0.0.1:22525',
      '--data-dir',
      dataDir,
      '--log-level',
      'warn',
    ],
    {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, VTORRENT_RPC_API_KEY: API_KEY },
    },
  )
  daemon.stdout?.on('data', chunk => {
    daemonLog += chunk.toString()
  })
  daemon.stderr?.on('data', chunk => {
    daemonLog += chunk.toString()
  })

  let ready = false
  for (let attempt = 0; attempt < 120; attempt += 1) {
    if (daemon.exitCode !== null) {
      throw new Error(`daemon exited with ${daemon.exitCode}\n${daemonLog}`)
    }
    try {
      const response = await fetch(`${RPC}/api/v1/info`)
      if (response.ok) {
        ready = true
        break
      }
    } catch {
      // The RPC listener is still starting.
    }
    await new Promise(resolve => setTimeout(resolve, 250))
  }
  if (!ready) throw new Error(`daemon RPC did not start\n${daemonLog}`)

  const wallet = regtestWallet()
  const imported = await rpc<{ address: string }>('/api/v1/wallet/import', {
    method: 'POST',
    body: JSON.stringify({ wif: wallet.wif, passphrase: PASSPHRASE, overwrite: true }),
  })
  walletAddress = imported.address
  await rpc('/api/v1/wallet/unlock', {
    method: 'POST',
    body: JSON.stringify({ passphrase: PASSPHRASE, timeout_secs: 600 }),
  })
  await rpc('/api/v1/faucet', {
    method: 'POST',
    body: JSON.stringify({ address: walletAddress, amount_satoshis: 10_000_000_000 }),
  })
})

test.afterAll(async () => {
  if (daemon && daemon.exitCode === null) {
    daemon.kill('SIGTERM')
    await new Promise<void>(resolve => {
      daemon?.once('exit', () => resolve())
    })
  }
  if (dataDir) await rm(dataDir, { recursive: true, force: true })
})

test('generates and funds a P2CS output through the browser UI', async ({ page, context }) => {
  await context.addInitScript(key => {
    window.localStorage.setItem('vtorrent.rpc_api_key', key)
  }, API_KEY)

  await page.goto('/create')
  await page.getByPlaceholder('Choose a strong passphrase').fill('e2e-ui-passphrase')
  await page.getByPlaceholder('Repeat your passphrase').fill('e2e-ui-passphrase')
  await page.getByRole('button', { name: 'Create Wallet' }).click()
  await expect(page).toHaveURL(/\/dashboard$/)

  await page.getByRole('link', { name: 'Staking' }).click()
  await expect(page).toHaveURL(/\/staking$/)
  await expect(page.getByRole('heading', { name: 'Cold staking (delegate)' })).toBeVisible()

  await page.getByTestId('cold-stake-generate').click()
  const script = (await page.getByTestId('cold-stake-script').textContent())?.trim() ?? ''
  expect(script).toMatch(/^[0-9a-f]+$/)
  await expect(page.getByTestId('cold-stake-script-hash')).not.toBeEmpty()
  await expect(page.getByTestId('cold-stake-staking-wif')).not.toBeEmpty()
  await expect(page.getByTestId('cold-stake-spending-wif')).not.toBeEmpty()

  await page.getByTestId('cold-stake-amount').fill('1')
  await page.getByTestId('cold-stake-passphrase').fill(PASSPHRASE)
  await page.getByTestId('cold-stake-fund').click()

  const txidLabel = await page.getByTestId('cold-stake-txid').textContent({ timeout: 20_000 })
  const txid = txidLabel?.replace('Funding txid:', '').trim() ?? ''
  expect(txid).toMatch(/^[0-9a-f]{64}$/)

  await rpc('/api/v1/blockchain/mine', {
    method: 'POST',
    body: JSON.stringify({ address: walletAddress, amount_satoshis: 100_000_000 }),
  })

  const tx = await rpc<{
    block_height: number | null
    outputs: { value_satoshis: number; script_pubkey: string }[]
  }>(`/api/v1/blockchain/tx/${txid}`)
  expect(tx.block_height).not.toBeNull()
  const coldOutput = tx.outputs.find(output => output.script_pubkey === script)
  expect(coldOutput?.value_satoshis).toBe(100_000_000)
})
