/** Generate Melon's non-secret DurinDoor Cordis `--patch` overlay. */

import * as yaml from 'js-yaml'

const API_KEY_ENV = 'MELON_DURINDOOR_API_KEY'
const SECRET_ID = /sk[-_]|api[_-]?key|secret|token|password|bearer/i

export interface MelonPatchModel {
  readonly id: string
}

export interface MelonPatchInput {
  readonly baseUrl: string
  readonly model: string
  readonly models: readonly MelonPatchModel[]
}

function requireModelId(id: string, label: string): string {
  if (typeof id !== 'string' || id.trim() === '' || id !== id.trim()) {
    throw new Error(`Melon Cordis patch: ${label} must be a non-empty trimmed model id.`)
  }
  if (SECRET_ID.test(id)) throw new Error(`Melon Cordis patch: ${label} looks like a secret.`)
  return id
}

/**
 * Serialize a complete id-targeted overlay for Melon's DurinDoor composition.
 * Row config is replacement, not a deep merge. No credential bytes are emitted.
 */
export function generateMelonCordisPatch(input: MelonPatchInput): string {
  if (!/^https?:\/\/\S+\/v1$/.test(input.baseUrl)) {
    throw new Error('Melon Cordis patch: baseUrl must be an http(s) origin ending in /v1.')
  }
  const model = requireModelId(input.model, 'selected model')
  if (!input.models.some(entry => entry.id === model)) {
    throw new Error('Melon Cordis patch: selected model is missing from the catalog.')
  }
  const models = input.models.map(entry => ({ id: requireModelId(entry.id, 'model id') }))
  return yaml.dump([
    {
      id: 'llm-pi-ai',
      config: {
        providers: {
          durindoor: {
            displayName: 'DurinDoor',
            apiKeyEnv: API_KEY_ENV,
            api: 'openai-completions',
            baseURL: input.baseUrl,
            models,
          },
        },
      },
    },
    { id: 'agent-default-model', config: { provider: 'durindoor', model } },
    { id: 'llm-deepseek', disabled: true },
    { id: 'web-search-deepseek', disabled: true },
    { id: 'tool-web', config: { search: false, fetch: false } },
  ], { lineWidth: 120, noRefs: true })
}
