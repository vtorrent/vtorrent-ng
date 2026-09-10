import { describe, it, expect } from 'vitest'
import { parseSeeds } from './useNetwork'

describe('parseSeeds', () => {
  it('splits comma-separated peers and trims', () => {
    expect(parseSeeds('127.0.0.1:22526, 10.0.0.2:22526')).toEqual([
      '127.0.0.1:22526',
      '10.0.0.2:22526',
    ])
  })

  it('drops empties', () => {
    expect(parseSeeds(' , 127.0.0.1:22526,,')).toEqual(['127.0.0.1:22526'])
    expect(parseSeeds('')).toEqual([])
  })
})
