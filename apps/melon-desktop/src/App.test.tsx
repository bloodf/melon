import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { App, SetupScreen } from './App.tsx'
import type { SetupState } from './setup.ts'

const modelProbe = {
  mode: 'external' as const,
  baseUrl: 'https://gateway.example/v1',
  auth: 'verified' as const,
  health: 'healthy' as const,
  models: [{ id: 'durindoor/model-a' }],
}

const cases: ReadonlyArray<readonly [SetupState, string]> = [
  [{ screen: 'choice' }, 'Choose how Melon reaches DurinDoor'],
  [{ screen: 'external', input: { baseUrl: '', apiKey: '' } }, 'Connect an existing DurinDoor'],
  [{ screen: 'insecure-http', input: { baseUrl: 'http://lan/v1', apiKey: '' } }, 'Confirm insecure HTTP'],
  [{ screen: 'probing', mode: 'external' }, 'Checking DurinDoor'],
  [{ screen: 'managed-progress', stage: 'download', percent: 40 }, 'Installing DurinDoor'],
  [{ screen: 'managed-progress', stage: 'verify', percent: 100 }, 'Installing DurinDoor'],
  [{ screen: 'managed-progress', stage: 'extract', percent: 100 }, 'Installing DurinDoor'],
  [{ screen: 'managed-progress', stage: 'durindoor-ready', percent: 100 }, 'Installing DurinDoor'],
  [{ screen: 'managed-progress', stage: 'harness-ready', percent: 100 }, 'Installing DurinDoor'],
  [{ screen: 'model-selection', probe: modelProbe }, 'Choose a model'],
  [{ screen: 'activating', probe: modelProbe, selectedModel: 'durindoor/model-a' }, 'Launching Harness'],
  [{ screen: 'saved', connection: { mode: 'external', baseUrl: modelProbe.baseUrl, model: 'durindoor/model-a' } }, 'Ready to launch'],
  [{ screen: 'error', error: { code: 'occupied-port', message: 'Port 20128 is occupied', recover: 'choose' } }, 'Port 20128 is occupied'],
]

afterEach(cleanup)

describe('SetupScreen', () => {
  for (const [state, heading] of cases) {
    it(`renders ${state.screen}`, () => {
      render(<SetupScreen state={state} dispatch={() => undefined} />)
      expect(screen.getByRole('heading', { name: heading })).toBeTruthy()
    })
  }

  it('labels the API key as optional', () => {
    render(<SetupScreen state={{ screen: 'external', input: { baseUrl: '', apiKey: '' } }} dispatch={() => undefined} />)
    expect(screen.getByLabelText('API key (optional)')).toBeTruthy()
  })

  it('states credential exposure in insecure HTTP confirmation', () => {
    render(<SetupScreen state={{ screen: 'insecure-http', input: { baseUrl: 'http://lan/v1', apiKey: 'secret' } }} dispatch={() => undefined} />)
    expect(screen.getByText(/API key can be exposed/i)).toBeTruthy()
  })

  it('shows unverified authentication and pre-existing ownership truthfully', () => {
    render(<SetupScreen state={{
      screen: 'model-selection',
      probe: { ...modelProbe, auth: 'unavailable', ownership: 'external' },
    }} dispatch={() => undefined} />)
    expect(screen.getByText(/API key was not verified/i)).toBeTruthy()
    expect(screen.getByText(/pre-existing DurinDoor/i)).toBeTruthy()
  })

  it.each([
    ['auth-required', 'API key required'],
    ['auth-unverified', 'API key could not be verified'],
    ['empty-models', 'DurinDoor returned no models'],
    ['malformed-models', 'DurinDoor returned an invalid model list'],
    ['activation-failed', 'Harness failed to start'],
  ] as const)('renders %s recovery', (code, message) => {
    render(<SetupScreen state={{ screen: 'error', error: { code, message, recover: 'retry' } }} dispatch={() => undefined} />)
    expect(screen.getByRole('heading', { name: message })).toBeTruthy()
  })

  it('offers saved connection retry and reconfigure', () => {
    render(<SetupScreen state={{ screen: 'saved', connection: { mode: 'external', baseUrl: modelProbe.baseUrl, model: 'model-a' } }} dispatch={() => undefined} />)
    expect(screen.getByRole('button', { name: 'Retry launch' })).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Change connection' })).toBeTruthy()
  })
})

describe('App controller wiring', () => {
  it('loads controller status without shutting down on unmount', async () => {
    const client = {
      status: vi.fn(async () => ({ keyPersistenceAvailable: false, running: false })),
      probe: vi.fn(), activate: vi.fn(), shutdown: vi.fn(async () => undefined),
    }
    const view = render(<App client={client} />)
    await waitFor(() => expect(client.status).toHaveBeenCalledOnce())
    view.unmount()
    expect(client.shutdown).not.toHaveBeenCalled()
  })

  it('stays on choice when controller status is unavailable', async () => {
    const client = {
      status: vi.fn(async () => { throw new Error("Cannot read properties of undefined (reading 'invoke')") }),
      probe: vi.fn(), activate: vi.fn(), shutdown: vi.fn(async () => undefined),
    }
    render(<App client={client} />)
    await waitFor(() => expect(client.status).toHaveBeenCalledOnce())
    expect(screen.getByRole('heading', { name: 'Choose how Melon reaches DurinDoor' })).toBeTruthy()
  })

  it('probes external input and activates the selected model', async () => {
    const client = {
      status: vi.fn(async () => ({ keyPersistenceAvailable: true, running: false })),
      probe: vi.fn(async () => modelProbe),
      activate: vi.fn(async () => ({ mode: 'external' as const, baseUrl: modelProbe.baseUrl, model: 'durindoor/model-a' })),
      shutdown: vi.fn(async () => undefined),
    }
    render(<App client={client} />)
    fireEvent.click(screen.getByRole('button', { name: /Connect an existing DurinDoor/i }))
    fireEvent.change(screen.getByLabelText('DurinDoor URL'), { target: { value: modelProbe.baseUrl } })
    fireEvent.change(screen.getByLabelText('API key (optional)'), { target: { value: 'secret' } })
    fireEvent.click(screen.getByRole('button', { name: 'Check connection' }))
    await screen.findByRole('heading', { name: 'Choose a model' })
    expect(client.probe).toHaveBeenCalledWith({ mode: 'external', baseUrl: modelProbe.baseUrl, apiKey: 'secret' })
    fireEvent.click(screen.getByRole('button', { name: 'durindoor/model-a' }))
    fireEvent.click(screen.getByRole('button', { name: 'Launch Melon' }))
    await screen.findByRole('heading', { name: 'Ready to launch' })
    expect(client.activate).toHaveBeenCalledWith(modelProbe, 'durindoor/model-a', 'secret')
  })

  it('shuts down only on explicit reconfigure', async () => {
    const client = {
      status: vi.fn(async () => ({
        keyPersistenceAvailable: true,
        running: true,
        connection: { mode: 'external' as const, baseUrl: modelProbe.baseUrl, model: 'durindoor/model-a' },
      })),
      probe: vi.fn(), activate: vi.fn(), shutdown: vi.fn(async () => undefined),
    }
    render(<App client={client} />)
    await screen.findByRole('heading', { name: 'Ready to launch' })
    fireEvent.click(screen.getByRole('button', { name: 'Change connection' }))
    await waitFor(() => expect(client.shutdown).toHaveBeenCalledOnce())
    expect(screen.getByRole('heading', { name: 'Choose how Melon reaches DurinDoor' })).toBeTruthy()
  })

  it('does not let delayed status replace a user-started setup flow', async () => {
    let resolveStatus!: (value: { keyPersistenceAvailable: boolean; running: boolean; connection: { mode: 'external'; baseUrl: string; model: string } }) => void
    const status = new Promise<{ keyPersistenceAvailable: boolean; running: boolean; connection: { mode: 'external'; baseUrl: string; model: string } }>((resolve) => { resolveStatus = resolve })
    const client = { status: vi.fn(() => status), probe: vi.fn(), activate: vi.fn(), shutdown: vi.fn(async () => undefined) }
    render(<App client={client} />)
    fireEvent.click(screen.getByRole('button', { name: /Connect an existing DurinDoor/i }))
    resolveStatus({ keyPersistenceAvailable: true, running: true, connection: { mode: 'external', baseUrl: modelProbe.baseUrl, model: 'old-model' } })
    await waitFor(() => expect(screen.getByRole('heading', { name: 'Connect an existing DurinDoor' })).toBeTruthy())
  })

  it('preserves URL and key fields after auth-required rejection', async () => {
    const client = {
      status: vi.fn(async () => ({ keyPersistenceAvailable: true, running: false })),
      probe: vi.fn(async () => { throw { code: 'auth-required', message: 'API key required' } }),
      activate: vi.fn(), shutdown: vi.fn(async () => undefined),
    }
    render(<App client={client} />)
    fireEvent.click(screen.getByRole('button', { name: /Connect an existing DurinDoor/i }))
    fireEvent.change(screen.getByLabelText('DurinDoor URL'), { target: { value: modelProbe.baseUrl } })
    fireEvent.change(screen.getByLabelText('API key (optional)'), { target: { value: 'bad-key' } })
    fireEvent.click(screen.getByRole('button', { name: 'Check connection' }))
    expect(await screen.findByText('API key required')).toBeTruthy()
    expect((screen.getByLabelText('DurinDoor URL') as HTMLInputElement).value).toBe(modelProbe.baseUrl)
    expect((screen.getByLabelText('API key (optional)') as HTMLInputElement).value).toBe('bad-key')
  })

  it('confirms non-loopback HTTP before probing', async () => {
    const client = {
      status: vi.fn(async () => ({ keyPersistenceAvailable: true, running: false })),
      probe: vi.fn(async () => modelProbe),
      activate: vi.fn(), shutdown: vi.fn(async () => undefined),
    }
    render(<App client={client} />)
    fireEvent.click(screen.getByRole('button', { name: /Connect an existing DurinDoor/i }))
    fireEvent.change(screen.getByLabelText('DurinDoor URL'), { target: { value: 'http://192.168.1.10:20128/v1' } })
    fireEvent.click(screen.getByRole('button', { name: 'Check connection' }))
    expect(client.probe).not.toHaveBeenCalled()
    expect(screen.getByRole('heading', { name: 'Confirm insecure HTTP' })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Continue over HTTP' }))
    await waitFor(() => expect(client.probe).toHaveBeenCalledWith({
      mode: 'external',
      baseUrl: 'http://192.168.1.10:20128/v1',
      apiKey: '',
      allowInsecureHttp: true,
    }))
  })

  it('shows truthful probing state when managed setup starts', async () => {
    const client = {
      status: vi.fn(async () => ({ keyPersistenceAvailable: true, running: false })),
      probe: vi.fn(() => new Promise<typeof modelProbe>(() => undefined)),
      activate: vi.fn(), shutdown: vi.fn(async () => undefined),
    }
    render(<App client={client} />)
    fireEvent.click(screen.getByRole('button', { name: /Install DurinDoor locally/i }))
    expect(screen.getByRole('heading', { name: 'Checking DurinDoor' })).toBeTruthy()
  })

  it('marks preserved auth-required inputs invalid', async () => {
    const client = {
      status: vi.fn(async () => ({ keyPersistenceAvailable: true, running: false })),
      probe: vi.fn(async () => { throw { code: 'auth-required', message: 'API key required' } }),
      activate: vi.fn(), shutdown: vi.fn(async () => undefined),
    }
    render(<App client={client} />)
    fireEvent.click(screen.getByRole('button', { name: /Connect an existing DurinDoor/i }))
    fireEvent.change(screen.getByLabelText('DurinDoor URL'), { target: { value: modelProbe.baseUrl } })
    fireEvent.click(screen.getByRole('button', { name: 'Check connection' }))
    await screen.findByRole('alert')
    expect(screen.getByLabelText('API key (optional)').getAttribute('aria-invalid')).toBe('true')
  })

  it('renders shutdown rejection during reconfigure', async () => {
    const client = {
      status: vi.fn(async () => ({ keyPersistenceAvailable: true, running: true, connection: { mode: 'external' as const, baseUrl: modelProbe.baseUrl, model: 'model-a' } })),
      probe: vi.fn(), activate: vi.fn(), shutdown: vi.fn(async () => { throw new Error('shutdown failed') }),
    }
    render(<App client={client} />)
    await screen.findByRole('heading', { name: 'Ready to launch' })
    fireEvent.click(screen.getByRole('button', { name: 'Change connection' }))
    expect(await screen.findByRole('heading', { name: 'shutdown failed' })).toBeTruthy()
  })
})
