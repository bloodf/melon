import { spawnSync } from 'node:child_process'
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { loadOverlayPatches } from '../../packages/boot/app-boot/src/index.ts'
import { generateMelonCordisPatch } from './generate-cordis-patch.mts'

const STAGED_DSH = resolve('apps/melon-desktop/src-tauri/resources/harness/lib/bin.js')

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

  it('rejects credential-bearing and query/fragment DurinDoor URLs', () => {
    const models = [{ id: 'ok' }]
    expect(() => generateMelonCordisPatch({
      baseUrl: 'https://user:s3cr3t@durindoor.example/v1',
      model: 'ok',
      models,
    })).toThrow(/userinfo|credential|endpoint/i)
    expect(() => generateMelonCordisPatch({
      baseUrl: 'https://host/v1?k=secret',
      model: 'ok',
      models,
    })).toThrow(/query|endpoint/i)
    expect(() => generateMelonCordisPatch({
      baseUrl: 'https://host/v1#token',
      model: 'ok',
      models,
    })).toThrow(/fragment|endpoint/i)
    expect(generateMelonCordisPatch({
      baseUrl: 'http://127.0.0.1:20128/v1',
      model: 'ok',
      models,
    })).not.toMatch(/s3cr3t|user:|@durindoor/)
  })

  it.skipIf(!existsSync(STAGED_DSH))('applies DurinDoor as the default through staged dsh dump-config', () => {
    const dir = mkdtempSync(join(tmpdir(), 'melon-cordis-dump-'))
    roots.push(dir)
    const patch = join(dir, 'melon.cordis.patch.yml')
    writeFileSync(patch, generateMelonCordisPatch({
      baseUrl: 'http://127.0.0.1:20128/v1',
      model: 'durindoor-test',
      models: [{ id: 'durindoor-test' }],
    }))
    const dumped = spawnSync(process.execPath, [STAGED_DSH, '--profile', 'web', '--patch', patch, '--dump-config'], {
      encoding: 'utf8',
      env: { ...process.env, DSH_HOME: join(dir, 'home') },
    })
    expect(dumped.status).toBe(0)
    expect(dumped.stdout).toMatch(/id:\s*agent-default-model[\s\S]*provider:\s*durindoor/)
    expect(dumped.stdout).toMatch(/id:\s*llm-deepseek[\s\S]*disabled:\s*true/)
    expect(dumped.stdout).not.toMatch(/sk-|s3cr3t|Bearer /)
  })
})
