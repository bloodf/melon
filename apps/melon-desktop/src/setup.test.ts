import { describe, expect, it } from 'vitest'
import { initialSetupState, reduceSetup } from './setup.ts'

const firstModel = { id: 'durindoor/model-a' }
const models = [firstModel, { id: 'durindoor/model-b', name: 'Model B' }]

describe('setup state', () => {
  it('starts at the connection choice', () => {
    expect(initialSetupState).toEqual({ screen: 'choice' })
  })

  it('keeps external input while requiring insecure HTTP confirmation', () => {
    const editing = reduceSetup(initialSetupState, { type: 'choose-external' })
    const warned = reduceSetup(editing, {
      type: 'confirm-insecure-http',
      input: { baseUrl: 'http://lan.example/v1', apiKey: 'secret' },
    })

    expect(warned).toEqual({
      screen: 'insecure-http',
      input: { baseUrl: 'http://lan.example/v1', apiKey: 'secret' },
    })
  })

  it.each(['download', 'verify', 'extract', 'durindoor-ready', 'harness-ready'] as const)('tracks managed %s progress', (stage) => {
    const progress = reduceSetup({ screen: 'managed-progress', stage: 'download', percent: 5 }, {
      type: 'progress',
      progress: { stage, percent: 100, message: `${stage} complete` },
    })

    expect(progress).toEqual({
      screen: 'managed-progress',
      stage,
      percent: 100,
      message: `${stage} complete`,
    })
  })

  it('selects only models returned by the probe', () => {
    const selection = reduceSetup({ screen: 'probing', mode: 'external' }, {
      type: 'probe-succeeded',
      probe: {
        mode: 'external',
        baseUrl: 'https://gateway.example/v1',
        auth: 'verified',
        health: 'healthy',
        models,
      },
    })
    expect(reduceSetup(selection, { type: 'select-model', model: 'missing' })).toEqual(selection)
    expect(reduceSetup(selection, { type: 'select-model', model: 'durindoor/model-b' })).toEqual({
      ...selection,
      selectedModel: 'durindoor/model-b',
    })
  })

  it('preserves unverified auth and pre-existing service ownership from probe', () => {
    const probe = {
      mode: 'managed-local' as const,
      baseUrl: 'http://127.0.0.1:20128/v1',
      auth: 'unavailable' as const,
      health: 'healthy' as const,
      ownership: 'external' as const,
      models,
    }
    expect(reduceSetup({ screen: 'probing', mode: 'managed-local' }, { type: 'probe-succeeded', probe })).toEqual({
      screen: 'model-selection',
      probe,
    })
  })

  it('returns a disappeared selected model to refreshed selection', () => {
    const activating = {
      screen: 'activating' as const,
      probe: {
        mode: 'external' as const,
        baseUrl: 'https://gateway.example/v1',
        auth: 'verified' as const,
        health: 'healthy' as const,
        models,
      },
      selectedModel: 'durindoor/model-b',
    }

    expect(reduceSetup(activating, { type: 'model-disappeared', models: [firstModel] })).toEqual({
      screen: 'model-selection',
      probe: { ...activating.probe, models: [firstModel] },
    })
  })

  it.each([
    ['auth-required', 'API key required', 'edit-external'],
    ['auth-unverified', 'API key could not be verified', 'edit-external'],
    ['empty-models', 'DurinDoor returned no models', 'retry'],
    ['malformed-models', 'DurinDoor returned an invalid model list', 'retry'],
    ['activation-failed', 'Harness failed to start', 'retry'],
  ] as const)('preserves recoverable %s errors', (code, message, recover) => {
    expect(reduceSetup({ screen: 'probing', mode: 'external' }, {
      type: 'failed',
      error: { code, message, recover },
    })).toEqual({ screen: 'error', error: { code, message, recover } })
  })

  it('retries and reconfigures a saved connection explicitly', () => {
    const connection = { mode: 'external' as const, baseUrl: 'https://gateway.example/v1', model: 'model-a' }
    const saved = { screen: 'saved' as const, connection }
    expect(reduceSetup(saved, { type: 'retry-saved' })).toEqual({ screen: 'probing', mode: 'external' })
    expect(reduceSetup(saved, { type: 'choose-again' })).toEqual(initialSetupState)
  })
})
