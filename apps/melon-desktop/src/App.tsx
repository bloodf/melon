import { useEffect, useReducer, useRef } from 'react'
import type { Dispatch, FormEvent, ReactNode } from 'react'
import { initialSetupState, reduceSetup } from './setup.ts'
import type { ExternalInput, ProbeResult, SetupAction, SetupState } from './setup.ts'
import { createTauriClient } from './tauri.ts'
import type { ControllerClient } from './tauri.ts'
import './styles.css'

interface SetupScreenProps {
  state: SetupState
  dispatch: Dispatch<SetupAction>
  onProbeExternal?: (input: ExternalInput & { allowInsecureHttp?: boolean }) => void
  onProbeManaged?: () => void
  onActivate?: (probe: ProbeResult, model: string) => void
  onReconfigure?: () => void
}
function Frame({ eyebrow, title, children }: { eyebrow: string; title: string; children: ReactNode }) {
  return <main className="setup-shell"><aside className="brand-rail" aria-label="Melon"><span className="brand-mark">M</span></aside><section className="setup-panel"><p className="eyebrow">{eyebrow}</p><h1>{title}</h1>{children}<p className="attribution">Built on DeepSeek Harness</p></section></main>
}

/** Renders only controller-derived setup state. */
export function SetupScreen({ state, dispatch, onProbeExternal, onProbeManaged, onActivate, onReconfigure }: SetupScreenProps) {
  switch (state.screen) {
    case 'choice': return <Frame eyebrow="Melon / Setup" title="Choose how Melon reaches DurinDoor"><div className="choice-grid"><button type="button" onClick={() => onProbeManaged?.()}><strong>Install DurinDoor locally</strong><span>Download and run a managed runtime on this machine.</span></button><button type="button" onClick={() => dispatch({ type: 'choose-external' })}><strong>Connect an existing DurinDoor</strong><span>Use a URL and optional API key.</span></button></div></Frame>
    case 'external': {
      const submit = (event: FormEvent) => { event.preventDefault(); onProbeExternal?.(state.input) }
      return <Frame eyebrow="Melon / External" title="Connect an existing DurinDoor"><form className="form-stack" onSubmit={submit}><label>DurinDoor URL<input type="url" value={state.input.baseUrl} aria-invalid={state.validationError !== undefined} onChange={event => dispatch({ type: 'update-external', input: { ...state.input, baseUrl: event.target.value } })} /></label><label>API key (optional)<input type="password" value={state.input.apiKey} aria-invalid={state.validationError !== undefined} onChange={event => dispatch({ type: 'update-external', input: { ...state.input, apiKey: event.target.value } })} /></label>{state.validationError !== undefined && <p role="alert">{state.validationError}</p>}<button type="submit">Check connection</button></form></Frame>
    }
    case 'insecure-http': return <Frame eyebrow="Melon / Security" title="Confirm insecure HTTP"><p>Traffic is not encrypted. An API key can be exposed to the network.</p><div className="actions"><button type="button" onClick={() => onProbeExternal?.({ ...state.input, allowInsecureHttp: true })}>Continue over HTTP</button><button type="button" onClick={() => dispatch({ type: 'return-external', input: state.input })}>Go back</button></div></Frame>
    case 'probing': return <Frame eyebrow="Melon / Probe" title="Checking DurinDoor"><p>Validating endpoint and loading available models.</p></Frame>
    case 'managed-progress': return <Frame eyebrow="Melon / Local" title="Installing DurinDoor"><progress max={100} value={state.percent} /><p>{state.message ?? state.stage}</p></Frame>
    case 'model-selection': {
      const selectedModel = state.selectedModel
      return <Frame eyebrow="Melon / Models" title="Choose a model">{state.probe.auth === 'unavailable' && <p>API key was not verified by this endpoint.</p>}{state.probe.ownership === 'external' && <p>Using a pre-existing DurinDoor. Melon will not stop it.</p>}<ul className="model-list">{state.probe.models.map(model => <li key={model.id}><button type="button" onClick={() => dispatch({ type: 'select-model', model: model.id })}>{model.name ?? model.id}</button></li>)}</ul>{selectedModel !== undefined && <button type="button" onClick={() => onActivate?.(state.probe, selectedModel)}>Launch Melon</button>}</Frame>
    }
    case 'activating': return <Frame eyebrow="Melon / Launch" title="Launching Harness"><p>Starting DeepSeek Harness with {state.selectedModel} through DurinDoor.</p></Frame>
    case 'saved': return <Frame eyebrow="Melon / Ready" title="Ready to launch"><p>{state.connection.model} via {state.connection.baseUrl}</p><div className="actions"><button type="button" onClick={() => dispatch({ type: 'retry-saved' })}>Retry launch</button><button type="button" onClick={onReconfigure}>Change connection</button></div></Frame>
    case 'error': return <Frame eyebrow="Melon / Error" title={state.error.message}><button type="button" onClick={onReconfigure}>Change connection</button></Frame>
  }
}

function controllerError(error: unknown) {
  if (typeof error === 'object' && error !== null && 'message' in error && typeof error.message === 'string') return { code: 'code' in error && typeof error.code === 'string' ? error.code : 'controller-error', message: error.message, recover: 'retry' as const }
  return { code: 'controller-error', message: error instanceof Error ? error.message : 'Controller operation failed', recover: 'retry' as const }
}

function needsInsecureHttpConfirm(baseUrl: string | undefined): boolean {
  if (baseUrl === undefined || !baseUrl.startsWith('http://')) return false
  try {
    const host = new URL(baseUrl).hostname
    return host !== '127.0.0.1' && host !== 'localhost' && host !== '::1' && host !== '[::1]'
  } catch {
    return true
  }
}

export function App({ client = createTauriClient() }: { client?: ControllerClient }) {
  const [state, dispatch] = useReducer(reduceSetup, initialSetupState)
  const currentState = useRef(state)
  const generation = useRef(0)
  const lastApiKey = useRef<string | undefined>(undefined)
  currentState.current = state
  useEffect(() => {
    const operation = generation.current
    void client.status().then((status) => {
      if (operation === generation.current && currentState.current.screen === 'choice' && status.connection !== undefined) dispatch({ type: 'activated', connection: status.connection })
    }).catch((error) => {
      if (operation === generation.current && currentState.current.screen === 'choice') dispatch({ type: 'failed', error: controllerError(error) })
    })
  }, [client])
  const probe = async (input: Parameters<ControllerClient['probe']>[0]) => {
    if (input.mode === 'external' && needsInsecureHttpConfirm(input.baseUrl) && input.allowInsecureHttp !== true) {
      dispatch({ type: 'confirm-insecure-http', input: { baseUrl: input.baseUrl ?? '', apiKey: input.apiKey ?? '' } })
      return
    }
    const operation = ++generation.current
    lastApiKey.current = input.apiKey
    dispatch({ type: 'begin-probe', mode: input.mode })
    try {
      const result = await client.probe(input)
      if (operation === generation.current) dispatch({ type: 'probe-succeeded', probe: result })
    } catch (error) {
      if (operation !== generation.current) return
      const failure = controllerError(error)
      if (input.mode === 'external' && failure.code === 'auth-required') dispatch({ type: 'external-failed', input: { baseUrl: input.baseUrl ?? '', apiKey: input.apiKey ?? '' }, message: failure.message })
      else dispatch({ type: 'failed', error: failure })
    }
  }
  const activate = async (result: ProbeResult, model: string) => {
    const operation = ++generation.current
    dispatch({ type: 'select-model', model }); dispatch({ type: 'activate' })
    try {
      const connection = await client.activate(result, model, lastApiKey.current)
      if (operation === generation.current) dispatch({ type: 'activated', connection })
    } catch (error) {
      if (operation === generation.current) dispatch({ type: 'failed', error: controllerError(error) })
    }
  }
  const reconfigure = async () => {
    ++generation.current
    try { await client.shutdown(); dispatch({ type: 'choose-again' }) } catch (error) { dispatch({ type: 'failed', error: controllerError(error) }) }
  }
  return <SetupScreen state={state} dispatch={dispatch} onProbeExternal={(input) => { void probe({ mode: 'external', ...input }) }} onProbeManaged={() => { void probe({ mode: 'managed-local' }) }} onActivate={(result, model) => { void activate(result, model) }} onReconfigure={() => { void reconfigure() }} />
}
