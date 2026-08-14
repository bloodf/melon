// @vitest-environment node

import { afterEach, describe, expect, it } from 'vitest'
import { startFakeDurinDoor } from './fake-durindoor.mts'
import type { FakeDurinDoor } from './fake-durindoor.mts'

let server: FakeDurinDoor | undefined
afterEach(async () => { await server?.close(); server = undefined })

async function start(mode: Parameters<typeof startFakeDurinDoor>[0]) {
  server = await startFakeDurinDoor(mode)
  return server.url
}

describe('fake DurinDoor', () => {
  it('serves no-auth health, auth, and models', async () => {
    const url = await start({ auth: 'none', models: [{ id: 'model-a' }] })
    expect(await fetch(`${url}/api/health`).then(r => r.json())).toEqual({ ok: true })
    expect((await fetch(`${url}/api/v1/realtime/auth`)).status).toBe(200)
    expect(await fetch(`${url}/v1/models`).then(r => r.json())).toEqual({ data: [{ id: 'model-a' }] })
  })

  it('requires the configured bearer key', async () => {
    const url = await start({ auth: 'key', apiKey: 'valid-key', models: [{ id: 'model-a' }] })
    expect((await fetch(`${url}/api/v1/realtime/auth`)).status).toBe(401)
    expect((await fetch(`${url}/api/v1/realtime/auth`, { headers: { authorization: 'Bearer wrong' } })).status).toBe(401)
    expect((await fetch(`${url}/api/v1/realtime/auth`, { headers: { authorization: 'Bearer valid-key' } })).status).toBe(200)
  })

  it.each([
    ['missing-health', 404],
    ['unsupported-auth', 404],
  ] as const)('supports %s routes', async (mode, expected) => {
    const url = await start({ auth: mode === 'unsupported-auth' ? 'unsupported' : 'none', health: mode === 'missing-health' ? 'missing' : 'ok', models: [{ id: 'model-a' }] })
    const path = mode === 'missing-health' ? '/api/health' : '/api/v1/realtime/auth'
    expect((await fetch(`${url}${path}`)).status).toBe(expected)
  })

  it.each(['empty', 'malformed', 'oversized'] as const)('serves %s model responses', async models => {
    const url = await start({ auth: 'none', models })
    const response = await fetch(`${url}/v1/models`)
    if (models === 'empty') expect(await response.json()).toEqual({ data: [] })
    if (models === 'malformed') expect(await response.text()).toBe('{not-json')
    if (models === 'oversized') expect(Number(response.headers.get('content-length'))).toBeGreaterThan(1_000_000)
  })

  it('delays configured responses', async () => {
    const url = await start({ auth: 'none', models: [{ id: 'model-a' }], delayMs: 30 })
    const before = performance.now()
    await fetch(`${url}/v1/models`)
    expect(performance.now() - before).toBeGreaterThanOrEqual(20)
  })

  it('streams a chat completion', async () => {
    const url = await start({ auth: 'none', models: [{ id: 'model-a' }] })
    const response = await fetch(`${url}/v1/chat/completions`, { method: 'POST' })
    expect(response.headers.get('content-type')).toContain('text/event-stream')
    expect(await response.text()).toContain('data: [DONE]')
  })

  it('closes bounded after keep-alive requests', async () => {
    const url = await start({ auth: 'none', models: [{ id: 'model-a' }] })
    await fetch(`${url}/api/health`)
    await expect(Promise.race([
      server!.close().then(() => 'closed'),
      new Promise(resolve => setTimeout(() => resolve('timeout'), 200)),
    ])).resolves.toBe('closed')
    server = undefined
  })
})
