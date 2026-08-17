import { createServer } from 'node:http'
import type { Server } from 'node:http'

interface Model { id: string; name?: string }
export interface FakeDurinDoorOptions {
  auth: 'none' | 'key' | 'unsupported'
  apiKey?: string
  health?: 'ok' | 'missing'
  models: Model[] | 'empty' | 'malformed' | 'oversized'
  delayMs?: number
}
export interface FakeDurinDoorChat { path: string; authorization?: string; body: unknown }
export interface FakeDurinDoor { url: string; chats: FakeDurinDoorChat[]; close(): Promise<void> }

function authorized(header: string | undefined, key: string | undefined): boolean {
  return header === `Bearer ${key}`
}

/** Starts a loopback-only deterministic DurinDoor HTTP fixture. */
export async function startFakeDurinDoor(options: FakeDurinDoorOptions): Promise<FakeDurinDoor> {
  const chats: FakeDurinDoorChat[] = []
  const server = createServer(async (request, response) => {
    if (options.delayMs !== undefined) await new Promise(resolve => setTimeout(resolve, options.delayMs))
    const path = new URL(request.url ?? '/', 'http://127.0.0.1').pathname
    const authRequired = options.auth === 'key'
    const isAuthorized = !authRequired || authorized(request.headers.authorization, options.apiKey)

    if (path === '/api/health') {
      if (options.health === 'missing') { response.writeHead(404).end(); return }
      response.setHeader('content-type', 'application/json')
      response.end(JSON.stringify({ ok: true }))
      return
    }
    if (path === '/api/v1/realtime/auth') {
      if (options.auth === 'unsupported') { response.writeHead(404).end(); return }
      response.writeHead(isAuthorized ? 200 : 401).end()
      return
    }
    if (path === '/v1/models') {
      if (!isAuthorized) { response.writeHead(401).end(); return }
      if (options.models === 'malformed') { response.end('{not-json'); return }
      if (options.models === 'oversized') {
        const body = JSON.stringify({ data: [{ id: 'x'.repeat(1_000_001) }] })
        response.setHeader('content-length', Buffer.byteLength(body))
        response.end(body)
        return
      }
      response.setHeader('content-type', 'application/json')
      response.end(JSON.stringify({ data: options.models === 'empty' ? [] : options.models }))
      return
    }
    if (path === '/v1/chat/completions' && request.method === 'POST') {
      if (!isAuthorized) { response.writeHead(401).end(); return }
      const chunks: Buffer[] = []
      for await (const chunk of request) chunks.push(chunk as Buffer)
      const raw = Buffer.concat(chunks).toString('utf8')
      chats.push({
        path,
        authorization: request.headers.authorization,
        body: raw === '' ? undefined : JSON.parse(raw) as unknown,
      })
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      response.end([
        'data: {"choices":[{"delta":{"content":"melon"}}]}',
        'data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}',
        'data: [DONE]',
        '',
      ].join('\n\n'))
      return
    }
    response.writeHead(404).end()
  })

  await new Promise<void>((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  const address = server.address()
  if (address === null || typeof address === 'string') throw new Error('fake DurinDoor did not bind TCP')
  return {
    url: `http://127.0.0.1:${address.port}`,
    chats,
    close: () => closeServer(server),
  }
}


function closeServer(server: Server): Promise<void> {
  server.closeIdleConnections()
  server.closeAllConnections()
  return new Promise((resolve, reject) => server.close(error => error === undefined ? resolve() : reject(error)))
}
