import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { loadOverlayPatches } from '../../packages/boot/app-boot/src/index.ts'
import { generateMelonCordisPatch } from './generate-cordis-patch.mts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

function parse(yamlText: string) {
  const dir = mkdtempSync(join(tmpdir(), 'melon-cordis-patch-'))
  roots.push(dir)
  const file = join(dir, 'melon.cordis.patch.yml')
  writeFileSync(file, yamlText)
  return loadOverlayPatches('melon', file)
}

describe('Melon Cordis patch', () => {
  it('declares DurinDoor as the default OpenAI-compatible route without key bytes', () => {
    const yamlText = generateMelonCordisPatch({
      baseUrl: 'http://127.0.0.1:20128/v1',
      model: 'durindoor-test',
      models: [{ id: 'durindoor-test' }, { id: 'other' }],
    })
    expect(yamlText).not.toMatch(/sk_|api[_-]?key\s*:|Bearer /i)
    const rows = parse(yamlText)
    const pi = rows.find(row => row.id === 'llm-pi-ai')
    expect(pi?.config).toEqual({
      providers: {
        durindoor: {
          displayName: 'DurinDoor',
          apiKeyEnv: 'MELON_DURINDOOR_API_KEY',
          api: 'openai-completions',
          baseURL: 'http://127.0.0.1:20128/v1',
          models: [{ id: 'durindoor-test' }, { id: 'other' }],
        },
      },
    })
  })

  it('selects DurinDoor as the default model and disables DeepSeek chat and search', () => {
    const rows = parse(generateMelonCordisPatch({
      baseUrl: 'https://example.test/v1',
      model: 'chosen',
      models: [{ id: 'chosen' }],
    }))
    expect(rows.find(row => row.id === 'agent-default-model')?.config).toEqual({
      provider: 'durindoor',
      model: 'chosen',
    })
    expect(rows.find(row => row.id === 'llm-deepseek')?.disabled).toBe(true)
    expect(rows.find(row => row.id === 'web-search-deepseek')?.disabled).toBe(true)
    expect(rows.find(row => row.id === 'tool-web')?.config).toEqual({ search: false, fetch: false })
  })

  it('rejects a catalog that omits the selected model or contains a secret-shaped id', () => {
    expect(() => generateMelonCordisPatch({
      baseUrl: 'http://127.0.0.1:20128/v1',
      model: 'missing',
      models: [{ id: 'other' }],
    })).toThrow(/selected model/)
    expect(() => generateMelonCordisPatch({
      baseUrl: 'http://127.0.0.1:20128/v1',
      model: 'ok',
      models: [{ id: 'ok' }, { id: 'sk-secret' }],
    })).toThrow(/secret|model id/i)
  })
})
