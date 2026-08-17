// @vitest-environment node

import { spawn, spawnSync } from 'node:child_process'
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { generateMelonCordisPatch } from '../../../scripts/melon/generate-cordis-patch.mts'
import { startFakeDurinDoor } from './fake-durindoor.mts'

const STAGED_DSH = resolve(import.meta.dirname, '../src-tauri/resources/harness/lib/bin.js')
const SIDECAR = resolve(import.meta.dirname, '../src-tauri/binaries/node-x86_64-unknown-linux-gnu')
const MODEL = 'durindoor-test'
const homes: string[] = []

afterEach(() => { for (const home of homes.splice(0)) rmSync(home, { recursive: true, force: true }) })

describe('fake SSE through staged Harness', () => {
  it('requires the staged dsh entry and Node sidecar', () => {
    expect(existsSync(STAGED_DSH), 'stage Harness with melon:stage-harness-runtime').toBe(true)
    expect(existsSync(SIDECAR), 'stage Node with melon:stage-node-sidecar').toBe(true)
  })

  it('streams one DurinDoor reply through llm-pi-ai and the headless agent loop', () => {
    expect(existsSync(STAGED_DSH)).toBe(true)
    expect(existsSync(SIDECAR)).toBe(true)

    return startFakeDurinDoor({ auth: 'none', models: [{ id: MODEL }] }).then(async (server) => {
      const home = mkdtempSync(join(tmpdir(), 'melon-fake-chat-'))
      homes.push(home)
      const patch = join(home, 'melon.cordis.patch.yml')
      writeFileSync(patch, generateMelonCordisPatch({
        baseUrl: `${server.url}/v1`,
        model: MODEL,
        models: [{ id: MODEL }],
      }))

      const env = {
        ...process.env,
        DSH_HOME: home,
        DSH_TELEMETRY_DISABLED: '1',
        MELON_DURINDOOR_API_KEY: 'sk_durindoor',
      }
      const dumped = spawnSync(SIDECAR, [STAGED_DSH, '--profile', 'headless', '--patch', patch, '--dump-config'], {
        encoding: 'utf8',
        cwd: homedir(),
        stdio: ['ignore', 'pipe', 'pipe'],
        env,
      })
      expect(dumped.status, dumped.stderr).toBe(0)
      expect(dumped.stdout).toMatch(/id:\s*agent-default-model[\s\S]*provider:\s*durindoor[\s\S]*model:\s*durindoor-test/)
      expect(dumped.stdout).toMatch(/id:\s*llm-deepseek[\s\S]*disabled:\s*true/)
      expect(`${dumped.stdout}\n${dumped.stderr}\n${readFileSync(patch, 'utf8')}`).not.toMatch(/sk_durindoor/)

      const chat = await new Promise<{ status: number | null; stdout: string; stderr: string }>((resolve, reject) => {
        const child = spawn(SIDECAR, [STAGED_DSH, '--profile', 'headless', '--patch', patch, 'say melon once'], {
          cwd: homedir(),
          stdio: ['ignore', 'pipe', 'pipe'],
          env,
        })
        let stdout = ''
        let stderr = ''
        child.stdout.on('data', chunk => { stdout += String(chunk) })
        child.stderr.on('data', chunk => { stderr += String(chunk) })
        const timer = setTimeout(() => {
          child.kill('SIGKILL')
          reject(new Error(`staged dsh timed out\n${stdout}\n${stderr}`))
        }, 45_000)
        child.on('error', error => {
          clearTimeout(timer)
          reject(error)
        })
        child.on('close', status => {
          clearTimeout(timer)
          resolve({ status, stdout, stderr })
        })
      })
      expect(chat.status, `${chat.stdout}\n${chat.stderr}`).toBe(0)
      expect(chat.stdout).toContain('melon')
      expect(`${chat.stdout}\n${chat.stderr}`).not.toMatch(/sk_durindoor/)
      expect(server.chats.length).toBeGreaterThan(0)
      expect(server.chats[0]?.path).toBe('/v1/chat/completions')
      expect(server.chats[0]?.authorization).toBe('Bearer sk_durindoor')
      expect(JSON.stringify(server.chats[0]?.body)).toMatch(/say melon once/)
      await server.close()
    })
  }, 70_000)
})
