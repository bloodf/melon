import { describe, expect, it, vi } from 'vitest'
import { createTauriClient } from './tauri.ts'

it('uses the four controller command names and preserves payloads', async () => {
  const invoke = vi.fn(async (command: string) => ({
    status: { keyPersistenceAvailable: false, running: false },
    probe: { mode: 'external', baseUrl: 'https://gateway.example/v1', auth: 'verified', health: 'healthy', models: [{ id: 'model-a' }] },
    activate: { mode: 'external', baseUrl: 'https://gateway.example/v1', model: 'model-a' },
    shutdown: undefined,
  })[command as 'status' | 'probe' | 'activate' | 'shutdown'])
  const client = createTauriClient(invoke)
  const input = { mode: 'external' as const, baseUrl: 'https://gateway.example/v1', apiKey: 'key' }
  const probe = { token: 'opaque-probe' }

  await client.status()
  await client.probe(input)
  await client.activate(probe, 'model-a')
  await client.shutdown()

  expect(invoke.mock.calls).toEqual([
    ['status'],
    ['probe', { input }],
    ['activate', { probe, model: 'model-a' }],
    ['shutdown'],
  ])
})

describe('command input secrecy', () => {
  it('does not serialize inputs locally', () => {
    const client = createTauriClient(async () => undefined)
    expect(Object.keys(client)).toEqual(['status', 'probe', 'activate', 'shutdown'])
  })
})

it('rejects malformed native responses', async () => {
  const client = createTauriClient(async command => command === 'status' ? { running: 'yes' } : {})
  await expect(client.status()).rejects.toThrow('invalid status response')
})

it.each([
  { mode: 'external', baseUrl: 'https://x/v1', auth: 'wrong', health: 'healthy', models: [{ id: 'm' }] },
  { mode: 'external', baseUrl: 'https://x/v1', auth: 'verified', health: 'wrong', models: [{ id: 'm' }] },
  { mode: 'external', baseUrl: 'https://x/v1', auth: 'verified', health: 'healthy', ownership: 'unknown', models: [{ id: 'm' }] },
  { mode: 'external', baseUrl: 'https://x/v1', auth: 'verified', health: 'healthy', models: [] },
  { mode: 'external', baseUrl: 'https://x/v1', auth: 'verified', health: 'healthy', models: [{ id: '' }] },
  { mode: 'external', baseUrl: 'https://x/v1', auth: 'verified', health: 'healthy', models: [{ id: 'm', name: 1 }] },
] as const)('rejects malformed probe responses', async (response) => {
  const client = createTauriClient(async () => response)
  await expect(client.probe({ mode: 'external', baseUrl: 'https://x/v1' })).rejects.toThrow('invalid probe response')
})

it('rejects malformed nested saved status and activation responses', async () => {
  const status = createTauriClient(async () => ({ keyPersistenceAvailable: true, running: true, connection: { mode: 'external', baseUrl: 'https://x/v1', model: '' } }))
  await expect(status.status()).rejects.toThrow('invalid activation response')
  const activation = createTauriClient(async () => ({ mode: 'external', baseUrl: 'https://x/v1', model: '' }))
  await expect(activation.activate({}, 'm')).rejects.toThrow('invalid activation response')
})

it('rejects empty native URLs', async () => {
  const activation = createTauriClient(async () => ({ mode: 'external', baseUrl: '', model: 'm' }))
  await expect(activation.activate({}, 'm')).rejects.toThrow('invalid activation response')
  const probe = createTauriClient(async () => ({ mode: 'external', baseUrl: '', auth: 'verified', health: 'healthy', models: [{ id: 'm' }] }))
  await expect(probe.probe({ mode: 'external' })).rejects.toThrow('invalid probe response')
})
