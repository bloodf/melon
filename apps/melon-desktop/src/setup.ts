export type ConnectionMode = 'managed-local' | 'external'
export type ConnectionOwnership = 'managed' | 'external'

export interface ExternalInput { baseUrl: string; apiKey: string }
export interface ModelInfo { id: string; name?: string }
export interface ProbeResult {
  mode: ConnectionMode
  baseUrl: string
  auth: 'verified' | 'not-required' | 'unavailable'
  health: 'healthy' | 'not-exposed'
  ownership?: ConnectionOwnership
  models: ModelInfo[]
}
export interface SavedConnection { mode: ConnectionMode; baseUrl: string; model: string }
export type RecoveryAction = 'choose' | 'edit-external' | 'retry'
export interface SetupError { code: string; message: string; recover: RecoveryAction }
export type ProgressStage = 'download' | 'verify' | 'extract' | 'durindoor-ready' | 'harness-ready'

export type SetupState =
  | { screen: 'choice' }
  | { screen: 'external'; input: ExternalInput; validationError?: string }
  | { screen: 'insecure-http'; input: ExternalInput }
  | { screen: 'probing'; mode: ConnectionMode }
  | { screen: 'managed-progress'; stage: ProgressStage; percent: number; message?: string }
  | { screen: 'model-selection'; probe: ProbeResult; selectedModel?: string }
  | { screen: 'activating'; probe: ProbeResult; selectedModel: string }
  | { screen: 'saved'; connection: SavedConnection }
  | { screen: 'error'; error: SetupError }

export type SetupAction =
  | { type: 'choose-external' }
  | { type: 'update-external'; input: ExternalInput }
  | { type: 'confirm-insecure-http'; input: ExternalInput }
  | { type: 'begin-probe'; mode: ConnectionMode }
  | { type: 'progress'; progress: { stage: ProgressStage; percent: number; message?: string } }
  | { type: 'external-failed'; input: ExternalInput; message: string }
  | { type: 'return-external'; input: ExternalInput }
  | { type: 'probe-succeeded'; probe: ProbeResult }
  | { type: 'select-model'; model: string }
  | { type: 'activate' }
  | { type: 'activated'; connection: SavedConnection }
  | { type: 'model-disappeared'; models: ModelInfo[] }
  | { type: 'failed'; error: SetupError }
  | { type: 'retry-saved' }
  | { type: 'choose-again' }

export const initialSetupState: SetupState = { screen: 'choice' }

/** Applies one setup transition without performing native work. */
export function reduceSetup(state: SetupState, action: SetupAction): SetupState {
  switch (action.type) {
    case 'choose-external': return { screen: 'external', input: { baseUrl: '', apiKey: '' } }
    case 'update-external':
      return state.screen === 'external' ? { screen: 'external', input: action.input } : state
    case 'confirm-insecure-http': return { screen: 'insecure-http', input: action.input }
    case 'begin-probe': return { screen: 'probing', mode: action.mode }
    case 'progress': return { screen: 'managed-progress', ...action.progress }
    case 'probe-succeeded': return { screen: 'model-selection', probe: action.probe }
    case 'external-failed': return { screen: 'external', input: action.input, validationError: action.message }
    case 'return-external': return { screen: 'external', input: action.input }
    case 'select-model':
      if (state.screen !== 'model-selection' || !state.probe.models.some(model => model.id === action.model)) return state
      return { ...state, selectedModel: action.model }
    case 'activate':
      if (state.screen !== 'model-selection' || state.selectedModel === undefined) return state
      return { screen: 'activating', probe: state.probe, selectedModel: state.selectedModel }
    case 'activated': return { screen: 'saved', connection: action.connection }
    case 'model-disappeared':
      if (state.screen !== 'activating') return state
      return { screen: 'model-selection', probe: { ...state.probe, models: action.models } }
    case 'failed': return { screen: 'error', error: action.error }
    case 'retry-saved':
      return state.screen === 'saved' ? { screen: 'probing', mode: state.connection.mode } : state
    case 'choose-again': return initialSetupState
  }
}
