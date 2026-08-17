import { chmodSync, existsSync, lstatSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import {
  createRuntimeDescriptor,
  resolveDshBin,
  stageHarnessRuntime,
  validateHarnessClosure,
  type CommandInvocation,
} from './stage-harness-runtime.mts'

const roots: string[] = []
const temporaryRoot = (): string => {
  const root = mkdtempSync(join(tmpdir(), 'melon-harness-stage-test-'))
  roots.push(root)
  return root
}
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

function writeJson(path: string, value: unknown): void {
  mkdirSync(dirname(path), { recursive: true })
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`)
}

function writeFile(path: string, value = 'fixture\n'): void {
  mkdirSync(dirname(path), { recursive: true })
  writeFileSync(path, value)
}

function closure(root: string): void {
  writeJson(join(root, 'package.json'), {
    name: '@deepseek-ai/dsh',
    version: '0.1.0-rc.5',
    bin: { dsh: 'lib/bin.js' },
    files: ['lib', 'linked-bin.js'],
    dependencies: {
      '@deepseek-ai/dsh-web-frontend': 'workspace:^',
      'fixture-helper': '^1.0.0',
    },
  })
  writeFile(join(root, 'lib/bin.js'), '#!/usr/bin/env node\n')
  chmodSync(join(root, 'lib/bin.js'), 0o755)
  writeJson(join(root, 'node_modules/@deepseek-ai/dsh-web-frontend/package.json'), {
    name: '@deepseek-ai/dsh-web-frontend',
    version: '0.1.0-rc.5',
    files: ['dist'],
  })
  writeFile(join(root, 'node_modules/@deepseek-ai/dsh-web-frontend/dist/index.html'), '<main>Harness</main>\n')
  writeJson(join(root, 'node_modules/fixture-helper/package.json'), {
    name: 'fixture-helper',
    version: '1.0.0',
    files: ['lib'],
  })
}

function repository(root: string): void {
  writeJson(join(root, 'package.json'), { name: '@deepseek-ai/dsh-root', private: true })
  writeJson(join(root, 'apps/cli/package.json'), { name: '@deepseek-ai/dsh', version: '0.1.0-rc.5' })
  writeJson(join(root, 'apps/melon-desktop/runtime/runtime-pins.json'), {
    schemaVersion: 1,
    melonVersion: '0.1.0',
    harness: { sourceSha: '47f943859bef60e4160492346772ded9b24f765a' },
  })
  writeJson(join(root, 'vendor/cosmokit/package.json'), { name: '@deepseek-ai/cosmokit', version: '1.8.2' })
  writeJson(join(root, 'packages/fixture/helper/package.json'), {
    name: 'fixture-helper',
    version: '1.0.0',
    files: ['lib', 'assets'],
  })
  writeFile(join(root, 'packages/fixture/helper/lib/index.js'))
  writeFile(join(root, 'packages/fixture/helper/assets/prompt.txt'))
  writeFile(join(root, 'packages/fixture/helper/README.md'))
  writeFile(join(root, 'packages/fixture/helper/LICENSE'))
  writeFile(join(root, 'packages/fixture/helper/src/secret.ts'), 'source secret\n')
  writeFile(join(root, 'packages/fixture/helper/tests/helper.spec.ts'), 'test secret\n')
  writeFile(join(root, 'packages/fixture/helper/lib/cache.tsbuildinfo'), 'absolute machine path\n')
  writeFile(join(root, 'packages/fixture/helper/.env.local'), 'TOKEN=do-not-copy\n')
  mkdirSync(join(root, 'apps/melon-desktop/src-tauri/resources'), { recursive: true })
}

function fixtureRunner(invocations: CommandInvocation[]): (invocation: CommandInvocation) => void {
  return (invocation) => {
    invocations.push(invocation)
    const deployIndex = invocation.args.indexOf('deploy')
    if (deployIndex !== -1) {
      const candidate = invocation.args.at(-1)!
      closure(candidate)
      rmSync(join(candidate, 'node_modules/fixture-helper'), { recursive: true, force: true })
      const realPackage = join(candidate, 'lib/bin.js')
      symlinkSync(realPackage, join(candidate, 'linked-bin.js'))
    }
  }
}

describe('Harness package metadata', () => {
  it('resolves dsh from package bin metadata', () => {
    const root = temporaryRoot()
    writeFile(join(root, 'lib/entry.js'))
    expect(resolveDshBin(root, { name: '@deepseek-ai/dsh', bin: { dsh: 'lib/entry.js' } })).toBe('lib/entry.js')
  })

  it.each([
    ['missing', { name: '@deepseek-ai/dsh' }],
    ['wrong command', { name: '@deepseek-ai/dsh', bin: { other: 'lib/bin.js' } }],
    ['non-string', { name: '@deepseek-ai/dsh', bin: { dsh: 7 } }],
    ['escaping', { name: '@deepseek-ai/dsh', bin: { dsh: '../bin.js' } }],
  ])('rejects %s bin metadata', (_label, manifest) => {
    expect(() => resolveDshBin(temporaryRoot(), manifest)).toThrow(/bin\.dsh|relative package path/)
  })
})

describe('Harness closure validation', () => {
  it('requires every production dependency in the deployed closure', () => {
    const root = temporaryRoot()
    closure(root)
    rmSync(join(root, 'node_modules/fixture-helper'), { recursive: true })
    expect(() => validateHarnessClosure(root)).toThrow(/fixture-helper.*missing/)
  })

  it('rejects links anywhere in the staged closure', () => {
    const root = temporaryRoot()
    closure(root)
    symlinkSync(join(root, 'lib/bin.js'), join(root, 'linked-bin.js'))
    expect(() => validateHarnessClosure(root)).toThrow(/symbolic link/)
  })

  it('requires built Web assets and expected package identities', () => {
    const root = temporaryRoot()
    closure(root)
    rmSync(join(root, 'node_modules/@deepseek-ai/dsh-web-frontend/dist/index.html'))
    expect(() => validateHarnessClosure(root)).toThrow(/Web.*index\.html/)
  })
  it('rejects non-publishable files while allowing explicitly published source and assets', () => {
    const root = temporaryRoot()
    closure(root)
    writeFile(join(root, 'node_modules/fixture-helper/src/allowed.ts'))
    const helperPath = join(root, 'node_modules/fixture-helper/package.json')
    const helper = JSON.parse(readFileSync(helperPath, 'utf8')) as { files: string[] }
    helper.files.push('src', 'assets')
    writeJson(helperPath, helper)
    writeFile(join(root, 'node_modules/fixture-helper/assets/prompt.txt'))
    expect(() => validateHarnessClosure(root)).not.toThrow()
    writeFile(join(root, 'node_modules/fixture-helper/tests/leak.spec.ts'))
    expect(() => validateHarnessClosure(root)).toThrow(/non-publishable.*tests\/leak\.spec\.ts/)
  })

  it('does not apply workspace publish-surface audit to third-party packages', () => {
    const root = temporaryRoot()
    closure(root)
    writeJson(join(root, 'node_modules/@anthropic-ai/sdk/package.json'), {
      name: '@anthropic-ai/sdk',
      version: '0.39.0',
      files: ['index.js'],
    })
    writeFile(join(root, 'node_modules/@anthropic-ai/sdk/index.js'))
    writeFile(join(root, 'node_modules/@anthropic-ai/sdk/CHANGELOG.md'), '# changelog\n')
    const workspaceNames = new Set(['@deepseek-ai/dsh', '@deepseek-ai/dsh-web-frontend', 'fixture-helper'])
    expect(() => validateHarnessClosure(root, { workspaceNames })).not.toThrow()
    writeFile(join(root, 'node_modules/fixture-helper/tests/leak.spec.ts'))
    expect(() => validateHarnessClosure(root, { workspaceNames })).toThrow(/non-publishable.*fixture-helper\/tests\/leak\.spec\.ts/)
  })
})

describe('Harness dependency validation', () => {


  it('validates npm aliases and declared dependency versions', () => {
    const root = temporaryRoot()
    closure(root)
    const manifestPath = join(root, 'package.json')
    const manifest = JSON.parse(readFileSync(manifestPath, 'utf8')) as { dependencies: Record<string, string> }
    manifest.dependencies.alias = 'npm:fixture-helper@^1.0.0'
    writeJson(manifestPath, manifest)
    writeJson(join(root, 'node_modules/alias/package.json'), { name: 'fixture-helper', version: '1.2.0' })
    expect(() => validateHarnessClosure(root)).not.toThrow()
    writeJson(join(root, 'node_modules/alias/package.json'), { name: 'fixture-helper', version: '2.0.0' })
    expect(() => validateHarnessClosure(root)).toThrow(/alias.*version 2\.0\.0.*\^1\.0\.0/)
  })

  it('accepts spaced comparators such as greater-or-equal ranges', () => {
    const root = temporaryRoot()
    closure(root)
    const manifestPath = join(root, 'node_modules/fixture-helper/package.json')
    writeJson(manifestPath, {
      name: 'fixture-helper',
      version: '1.0.0',
      files: ['lib'],
      dependencies: { express: '>= 4.11' },
    })
    writeJson(join(root, 'node_modules/express/package.json'), { name: 'express', version: '5.2.1' })
    expect(() => validateHarnessClosure(root)).not.toThrow()
  })

  it('does not enforce third-party dependency ranges against workspace restore rules', () => {
    const root = temporaryRoot()
    closure(root)
    writeJson(join(root, 'node_modules/https-proxy-agent/package.json'), {
      name: 'https-proxy-agent',
      version: '7.0.6',
      dependencies: { debug: '4' },
    })
    writeJson(join(root, 'node_modules/debug/package.json'), { name: 'debug', version: '4.4.3' })
    const workspaceNames = new Set(['@deepseek-ai/dsh', '@deepseek-ai/dsh-web-frontend', 'fixture-helper'])
    expect(() => validateHarnessClosure(root, { workspaceNames })).not.toThrow()
  })

  it('includes executable bits in closure integrity', () => {
    const root = temporaryRoot()
    closure(root)
    const executable = validateHarnessClosure(root).closure.sha256
    chmodSync(join(root, 'lib/bin.js'), 0o644)
    expect(validateHarnessClosure(root).closure.sha256).not.toBe(executable)
  })
})


describe('Harness runtime descriptor', () => {
  it('is deterministic, relative, and contains no ambient secret', () => {
    const root = temporaryRoot()
    closure(root)
    const validated = validateHarnessClosure(root)
    const facts = { harnessSourceSha: '47f943859bef60e4160492346772ded9b24f765a' }
    const first = createRuntimeDescriptor(validated, facts)
    process.env.MELON_DURINDOOR_API_KEY = 'sk-do-not-stage'
    const second = createRuntimeDescriptor(validateHarnessClosure(root), facts)
    delete process.env.MELON_DURINDOOR_API_KEY

    expect(second).toEqual(first)
    expect(first.packageRoot).toBe('.')
    expect(first.dshBin).toBe('lib/bin.js')
    expect(first.webAssets).toBe('node_modules/@deepseek-ai/dsh-web-frontend/dist')
    expect(JSON.stringify(first)).not.toContain(root)
    expect(JSON.stringify(first)).not.toMatch(/sk-do-not-stage|api.?key|secret|token/i)
    expect(first.closure.sha256).toMatch(/^[a-f0-9]{64}$/)
  })
})

describe('Harness publish surface', () => {
  it('restores only files allowed by package publication metadata', () => {
    const root = temporaryRoot()
    repository(root)
    const result = stageHarnessRuntime({ repoRoot: root, run: fixtureRunner([]) })
    const installed = join(result.publicationPath, 'node_modules/fixture-helper')

    expect(readFileSync(join(installed, 'lib/index.js'), 'utf8')).toBe('fixture\n')
    expect(readFileSync(join(installed, 'assets/prompt.txt'), 'utf8')).toBe('fixture\n')
    expect(readFileSync(join(installed, 'README.md'), 'utf8')).toBe('fixture\n')
    expect(readFileSync(join(installed, 'LICENSE'), 'utf8')).toBe('fixture\n')
    expect(existsSync(join(installed, 'src'))).toBe(false)
    expect(existsSync(join(installed, 'tests'))).toBe(false)
    expect(existsSync(join(installed, 'lib/cache.tsbuildinfo'))).toBe(false)
    expect(existsSync(join(installed, '.env.local'))).toBe(false)
  })

  it('restores nested files globs including files in the glob root', () => {
    const root = temporaryRoot()
    repository(root)
    writeJson(join(root, 'packages/fixture/helper/package.json'), {
      name: 'fixture-helper',
      version: '1.0.0',
      files: ['lib/types/**/*.d.ts'],
    })
    writeFile(join(root, 'packages/fixture/helper/lib/types/context.d.ts'), 'export {}\n')
    writeFile(join(root, 'packages/fixture/helper/lib/types/nested/fiber.d.ts'), 'export {}\n')

    const result = stageHarnessRuntime({ repoRoot: root, run: fixtureRunner([]) })
    const installed = join(result.publicationPath, 'node_modules/fixture-helper')

    expect(readFileSync(join(installed, 'lib/types/context.d.ts'), 'utf8')).toBe('export {}\n')
    expect(readFileSync(join(installed, 'lib/types/nested/fiber.d.ts'), 'utf8')).toBe('export {}\n')
  })

  it('skips workspace node_modules unless files explicitly publishes them', () => {
    const root = temporaryRoot()
    repository(root)
    const helper = join(root, 'packages/fixture/helper')
    mkdirSync(join(helper, 'node_modules/@deepseek-ai'), { recursive: true })
    symlinkSync(join(root, 'vendor/cosmokit'), join(helper, 'node_modules/@deepseek-ai/cosmokit'))

    const result = stageHarnessRuntime({ repoRoot: root, run: fixtureRunner([]) })
    const installed = join(result.publicationPath, 'node_modules/fixture-helper')

    expect(existsSync(join(installed, 'lib/index.js'))).toBe(true)
    expect(existsSync(join(installed, 'node_modules'))).toBe(false)
  })
})

const pnpmEntry = process.env.npm_execpath ?? 'pnpm.cjs'

const pnpmCommand = process.platform === 'win32' ? process.execPath : pnpmEntry
const pnpmArgs = (...args: unknown[]) => process.platform === 'win32' ? [pnpmEntry, ...args] : args

describe('Harness runtime staging', () => {
  it('preserves the previous publication when verification fails', () => {
    const root = temporaryRoot()
    repository(root)
    const published = join(root, 'apps/melon-desktop/src-tauri/resources/harness')
    writeFile(join(published, 'previous.txt'), 'keep\n')
    const invocations: CommandInvocation[] = []
    const run = fixtureRunner(invocations)

    expect(() => stageHarnessRuntime({
      repoRoot: root,
      run: (invocation) => {
        run(invocation)
        if (invocation.args.includes('release:verify-packed-install')) throw new Error('fixture verification failed')
      },
    })).toThrow(/fixture verification failed/)

    expect(readFileSync(join(published, 'previous.txt'), 'utf8')).toBe('keep\n')
    expect(invocations.some(invocation => invocation.args.includes('release:verify-packed-install'))).toBe(true)
  })

  it.each(['top-level', 'nested'])('rejects an out-of-tree %s symlink without replacing the prior publication', (kind) => {
    const root = temporaryRoot()
    repository(root)
    const published = join(root, 'apps/melon-desktop/src-tauri/resources/harness')
    writeFile(join(published, 'previous.txt'), 'keep\n')
    const outside = join(temporaryRoot(), 'sentinel.txt')
    writeFile(outside, 'must not stage\n')
    const run = fixtureRunner([])

    expect(() => stageHarnessRuntime({
      repoRoot: root,
      run: (invocation) => {
        run(invocation)
        if (!invocation.args.includes('deploy')) return
        const candidate = invocation.args.at(-1)!
        if (kind === 'top-level') symlinkSync(outside, join(candidate, 'outside-link'))
        else {
          mkdirSync(join(candidate, 'nested'), { recursive: true })
          symlinkSync(outside, join(candidate, 'nested/outside-link'))
        }
      },
    })).toThrow(/symlink target.*outside allowed roots/)

    expect(readFileSync(join(published, 'previous.txt'), 'utf8')).toBe('keep\n')
    expect(readFileSync(outside, 'utf8')).toBe('must not stage\n')
  })

  it('restores the prior publication after one candidate rename failure', () => {
    const root = temporaryRoot()
    repository(root)
    const resources = join(root, 'apps/melon-desktop/src-tauri/resources')
    const published = join(resources, 'harness')
    writeFile(join(published, 'previous.txt'), 'keep\n')
    let renames = 0

    expect(() => stageHarnessRuntime({
      repoRoot: root,
      run: fixtureRunner([]),
      rename: (from, to) => {
        renames += 1
        if (renames === 2) throw new Error('candidate publication failed')
        renameSync(from, to)
      },
    })).toThrow(/candidate publication failed/)

    expect(readFileSync(join(published, 'previous.txt'), 'utf8')).toBe('keep\n')
    expect(readdirSync(resources).filter(name => name.startsWith('.harness-backup-'))).toEqual([])
  })

  it('retains the prior publication outside candidate cleanup when publication and restore both fail', () => {
    const root = temporaryRoot()
    repository(root)
    const resources = join(root, 'apps/melon-desktop/src-tauri/resources')
    const published = join(resources, 'harness')
    writeFile(join(published, 'previous.txt'), 'recover me\n')
    let renames = 0

    expect(() => stageHarnessRuntime({
      repoRoot: root,
      run: fixtureRunner([]),
      rename: (from, to) => {
        renames += 1
        if (renames >= 2) throw new Error(`rename ${String(renames)} failed`)
        renameSync(from, to)
      },
    })).toThrow(/prior runtime retained for recovery at .*\.harness-backup-/)

    expect(existsSync(published)).toBe(false)
    const backups = readdirSync(resources).filter(name => name.startsWith('.harness-backup-'))
    expect(backups).toHaveLength(1)
    expect(readFileSync(join(resources, backups[0]!, 'runtime', 'previous.txt'), 'utf8')).toBe('recover me\n')
    expect(readdirSync(resources).some(name => name.startsWith('.harness-stage-'))).toBe(false)
  })

  it('publishes a validated candidate and cleans staging state', () => {
    const root = temporaryRoot()
    repository(root)
    const published = join(root, 'apps/melon-desktop/src-tauri/resources/harness')
    writeFile(join(published, 'previous.txt'), 'replace\n')
    const invocations: CommandInvocation[] = []

    const result = stageHarnessRuntime({ repoRoot: root, run: fixtureRunner(invocations) })

    expect(result.publicationPath).toBe(published)
    expect(existsSync(join(published, 'previous.txt'))).toBe(false)
    expect(lstatSync(join(published, 'lib/bin.js')).isFile()).toBe(true)
    const descriptor = JSON.parse(readFileSync(join(published, 'melon-harness-runtime.json'), 'utf8')) as { dshBin: string; harnessSourceSha: string }
    expect(descriptor).toMatchObject({
      dshBin: 'lib/bin.js',
      harnessSourceSha: '47f943859bef60e4160492346772ded9b24f765a',
    })
    expect(invocations.map(invocation => [invocation.command, ...invocation.args])).toEqual([
      [pnpmCommand, ...pnpmArgs('run', 'build')],
      [pnpmCommand, ...pnpmArgs('run', 'release:pack', '--family', 'dsh', '--out', expect.any(String))],
      [pnpmCommand, ...pnpmArgs('run', 'release:pack', '--family', 'vendor', '--out', expect.any(String))],
      [pnpmCommand, ...pnpmArgs('--dir', 'native/landlock-run', 'run', 'build:ts')],
      [pnpmCommand, ...pnpmArgs('--dir', 'native/landlock-run/packages/entry', 'pack', '--pack-destination', expect.any(String))],
      [pnpmCommand, ...pnpmArgs('run', 'release:verify-packed-install', '--family', 'dsh', '--from', expect.any(String), '--from', expect.any(String), '--from', expect.any(String))],
      [pnpmCommand, ...pnpmArgs('--filter', '@deepseek-ai/dsh', 'deploy', '--legacy', '--prod', '--config.node-linker=hoisted', '--config.auto-install-peers=false', '--config.link-workspace-packages=true', expect.any(String))],
    ])
    expect(existsSync(join(result.publicationPath, 'linked-bin.js'))).toBe(true)
  })
})
