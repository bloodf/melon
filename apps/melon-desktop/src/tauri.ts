import { invoke as tauriInvoke } from '@tauri-apps/api/core'
import type { ConnectionMode, ConnectionOwnership, ModelInfo, ProbeResult, SavedConnection } from './setup.ts'

export interface ProbeInput { mode: ConnectionMode; baseUrl?: string; apiKey?: string; allowInsecureHttp?: boolean }
export interface ControllerStatus {
  connection?: SavedConnection
  keyPersistenceAvailable: boolean
  running: boolean
  recoverableError?: string
}
export interface ControllerClient {
  status(): Promise<ControllerStatus>
  probe(input: ProbeInput): Promise<ProbeResult>
  activate(probe: unknown, model: string, apiKey?: string): Promise<SavedConnection>
  shutdown(): Promise<void>
}
type Invoke = (command: string, args?: Record<string, unknown>) => Promise<unknown>
const invokeTauri: Invoke = (command, args) => tauriInvoke(command, args)
const record = (value: unknown): value is Record<string, unknown> => typeof value === 'object' && value !== null
const savedResponse = (value: unknown): SavedConnection => {
  if (!record(value) || (value.mode !== 'external' && value.mode !== 'managed-local') || typeof value.baseUrl !== 'string' || value.baseUrl.length === 0 || typeof value.model !== 'string' || value.model.length === 0) throw new Error('invalid activation response')
  return { mode: value.mode, baseUrl: value.baseUrl, model: value.model }
}
const statusResponse = (value: unknown): ControllerStatus => {
  if (!record(value) || typeof value.keyPersistenceAvailable !== 'boolean' || typeof value.running !== 'boolean') throw new Error('invalid status response')
  const connection = value.connection === undefined ? undefined : savedResponse(value.connection)
  if (value.recoverableError !== undefined && typeof value.recoverableError !== 'string') throw new Error('invalid status response')
  return { keyPersistenceAvailable: value.keyPersistenceAvailable, running: value.running, ...(connection === undefined ? {} : { connection }), ...(typeof value.recoverableError === 'string' ? { recoverableError: value.recoverableError } : {}) }
}
type ProbeAuth = ProbeResult['auth']
type ProbeHealth = ProbeResult['health']
const connectionMode = (value: unknown): value is ConnectionMode => value === 'external' || value === 'managed-local'
const probeAuth = (value: unknown): value is ProbeAuth => value === 'verified' || value === 'not-required' || value === 'unavailable'
const probeHealth = (value: unknown): value is ProbeHealth => value === 'healthy' || value === 'not-exposed'
const ownership = (value: unknown): value is ConnectionOwnership => value === 'managed' || value === 'external'
const modelInfo = (value: unknown): value is ModelInfo => record(value) && typeof value.id === 'string' && value.id.length > 0 && (value.name === undefined || typeof value.name === 'string')
const probeResponse = (value: unknown): ProbeResult => {
  if (!record(value) || !connectionMode(value.mode) || typeof value.baseUrl !== 'string' || value.baseUrl.length === 0 || !probeAuth(value.auth) || !probeHealth(value.health) || (value.ownership !== undefined && !ownership(value.ownership)) || !Array.isArray(value.models) || value.models.length === 0 || !value.models.every(modelInfo)) throw new Error('invalid probe response')
  return {
    mode: value.mode,
    baseUrl: value.baseUrl,
    auth: value.auth,
    health: value.health,
    ...(value.ownership === undefined ? {} : { ownership: value.ownership }),
    models: value.models.map(model => ({ id: model.id, ...(model.name === undefined ? {} : { name: model.name }) })),
  }
}

/** Creates the sole frontend adapter to native controller commands. */
export function createTauriClient(invoke: Invoke = invokeTauri): ControllerClient {
  return {
    status: async () => statusResponse(await invoke('status')),
    probe: async input => probeResponse(await invoke('probe', { input })),
    activate: async (probe, model, apiKey) => savedResponse(await invoke('activate', { probe, model, ...(apiKey ? { apiKey } : {}) })),
    shutdown: async () => { await invoke('shutdown') },
  }
}
