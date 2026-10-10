import { describe, expect, test } from 'bun:test'
import { describeExpiry } from './utils.js'

const now = new Date('2026-10-09T00:00:00Z')

describe('describeExpiry', () => {
  test('null expiry means the token never expires', () => {
    expect(describeExpiry(null, now)).toEqual({ expired: false, text: 'Never expires' })
  })

  test('a future expiry shows the UTC date', () => {
    expect(describeExpiry('2026-11-01T12:00:00Z', now)).toEqual({ expired: false, text: 'Expires 2026-11-01' })
  })

  test('a past expiry is marked expired', () => {
    expect(describeExpiry('2026-10-01T12:00:00Z', now)).toEqual({ expired: true, text: 'Expired 2026-10-01' })
  })

  test('an expiry exactly at now counts as expired', () => {
    expect(describeExpiry('2026-10-09T00:00:00Z', now).expired).toBe(true)
  })

  test('an unparseable value is shown verbatim and not marked expired', () => {
    expect(describeExpiry('not-a-date', now)).toEqual({ expired: false, text: 'Expires not-a-date' })
  })
})
