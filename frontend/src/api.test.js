import { afterEach, describe, expect, test } from 'bun:test'

// api.js reads window.fetch at load time to install its 401 interceptor. Shim
// window only for the import; the calls below use the global fetch, which is
// the one api.js reaches at call time.
const originalWindow = globalThis.window
globalThis.window = { fetch: globalThis.fetch }
const api = await import('./api.js')
globalThis.window = originalWindow

const originalFetch = globalThis.fetch
afterEach(() => { globalThis.fetch = originalFetch })

function mockFetch(response) {
  const calls = []
  globalThis.fetch = async (url, options = {}) => {
    calls.push({ url, method: options.method ?? 'GET', body: options.body })
    return response
  }
  return calls
}

describe('token and session API calls', () => {
  test('createToken defaults to full access with no expiry', async () => {
    const calls = mockFetch(new Response(JSON.stringify({ raw_token: 'x' }), { status: 200 }))
    await api.createToken('cli')
    expect(calls).toHaveLength(1)
    expect(calls[0].url).toBe('/api/auth/tokens')
    expect(calls[0].method).toBe('POST')
    expect(JSON.parse(calls[0].body)).toEqual({ name: 'cli', expires_in_days: null, scope: 'full' })
  })

  test('createToken forwards expiry and read-only scope', async () => {
    const calls = mockFetch(new Response('{}', { status: 200 }))
    await api.createToken('ci', { expiresInDays: 30, scope: 'read' })
    expect(JSON.parse(calls[0].body)).toEqual({ name: 'ci', expires_in_days: 30, scope: 'read' })
  })

  test('listSessions issues a GET to the sessions route', async () => {
    const calls = mockFetch(new Response('[]', { status: 200 }))
    expect(await api.listSessions()).toEqual([])
    expect(calls[0].url).toBe('/api/auth/sessions')
    expect(calls[0].method).toBe('GET')
  })

  test('revokeSession encodes the handle as a single path segment', async () => {
    const calls = mockFetch(new Response(null, { status: 204 }))
    await api.revokeSession('a/b c')
    expect(calls[0].url).toBe('/api/auth/sessions/a%2Fb%20c')
    expect(calls[0].method).toBe('DELETE')
  })

  test('revokeOtherSessions resolves to the revoked count', async () => {
    const calls = mockFetch(new Response(JSON.stringify({ revoked: 2 }), { status: 200 }))
    expect(await api.revokeOtherSessions()).toEqual({ revoked: 2 })
    expect(calls[0].url).toBe('/api/auth/sessions')
    expect(calls[0].method).toBe('DELETE')
  })

  test('server error messages reach the caller', async () => {
    mockFetch(new Response(JSON.stringify({ error: 'cannot revoke current session' }), { status: 400 }))
    await expect(api.revokeSession('h')).rejects.toThrow('cannot revoke current session')
  })

  test('deleteAdminUser issues a DELETE for the user uid', async () => {
    const calls = mockFetch(new Response(null, { status: 204 }))
    await api.deleteAdminUser('user-1')
    expect(calls[0].url).toBe('/api/admin/users/user-1')
    expect(calls[0].method).toBe('DELETE')
  })
})
