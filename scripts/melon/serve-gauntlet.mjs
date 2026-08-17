#!/usr/bin/env node
import { createServer } from 'node:http'
import { readFile } from 'node:fs/promises'
import { extname, join, normalize, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(fileURLToPath(new URL('../../apps/melon-desktop/design', import.meta.url)))
const types = { '.html': 'text/html; charset=utf-8', '.png': 'image/png', '.css': 'text/css', '.md': 'text/markdown; charset=utf-8' }

const server = createServer(async (request, response) => {
  const url = new URL(request.url ?? '/', 'http://127.0.0.1')
  const relative = decodeURIComponent(url.pathname === '/' ? 'gauntlet/index.html' : url.pathname.replace(/^\/+/, ''))
  if (relative.split(/[\\/]/).some(segment => segment === '..')) {
    response.writeHead(403).end()
    return
  }
  const file = resolve(root, relative)
  const prefix = root.endsWith('/') ? root : `${root}/`
  if (file !== root && !file.startsWith(prefix)) {
    response.writeHead(403).end()
    return
  }
  try {
    const body = await readFile(file)
    response.writeHead(200, { 'content-type': types[extname(file)] ?? 'application/octet-stream', 'x-content-type-options': 'nosniff' })
    response.end(body)
  } catch {
    response.writeHead(404).end()
  }
})

server.listen(4317, '127.0.0.1', () => {
  process.stdout.write('Melon Gauntlet http://127.0.0.1:4317/\n')
})
