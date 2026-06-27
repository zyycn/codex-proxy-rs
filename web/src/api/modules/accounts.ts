import request from '../request'

export function getAccounts(params?: any) {
  return request({
    url: '/api/admin/accounts',
    method: 'GET',
    params,
  })
}

export function createAccount(data: any) {
  return request({
    url: '/api/admin/accounts',
    method: 'POST',
    data,
  })
}

export function importAccounts(data: any) {
  return request({
    url: '/api/admin/accounts/import',
    method: 'POST',
    data,
  })
}

export function deleteAccounts(data: any) {
  return request({
    url: '/api/admin/accounts/delete',
    method: 'POST',
    data,
  })
}

export function refreshAccount(data: any) {
  return request({
    url: '/api/admin/accounts/refresh',
    method: 'POST',
    data,
  })
}

export function updateAccount(data: any) {
  return request({
    url: '/api/admin/accounts/update',
    method: 'POST',
    data,
  })
}

export function getAccountQuota(params: any) {
  return request({
    url: '/api/admin/accounts/quota',
    method: 'GET',
    params,
  })
}

export function testAccountConnection(data: any) {
  return request({
    url: '/api/admin/accounts/health-check',
    method: 'POST',
    data,
  })
}

export async function testAccountConnectionStream(data: any, onEvent: any, signal?: AbortSignal) {
  const { id, ...payload } = data
  const baseURL = import.meta.env.DEV ? '/dev' : ''
  const params = new URLSearchParams({ id: String(id) })
  const response = await fetch(`${baseURL}/api/admin/accounts/test?${params}`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    credentials: 'include',
    body: JSON.stringify(payload),
    signal,
  })

  if (!response.ok) {
    throw new Error((await response.text()) || `HTTP ${response.status}`)
  }
  if (!response.body) {
    throw new Error('测试连接没有返回流')
  }

  const reader = response.body.getReader()
  const decoder = new TextDecoder()
  let buffer = ''

  while (true) {
    const { done, value } = await reader.read()
    if (done) break
    buffer += decoder.decode(value, { stream: true })
    buffer = consumeAccountTestFrames(buffer, onEvent)
  }

  buffer += decoder.decode()
  consumeAccountTestFrames(buffer, onEvent)
}

export function getAccountTestModels(data: any) {
  return request({
    url: '/api/admin/accounts/models',
    method: 'GET',
    params: {
      id: data.id,
    },
  })
}

function consumeAccountTestFrames(buffer: string, onEvent: any) {
  let rest = buffer
  let frameEnd = findFrameEnd(rest)
  while (frameEnd) {
    const frame = rest.slice(0, frameEnd.index)
    rest = rest.slice(frameEnd.index + frameEnd.length)
    const data = frame
      .split(/\r?\n/)
      .filter((line) => line.startsWith('data:'))
      .map((line) => line.slice(5).trimStart())
      .join('\n')
      .trim()
    if (data && data !== '[DONE]') {
      onEvent(JSON.parse(data))
    }
    frameEnd = findFrameEnd(rest)
  }
  return rest
}

function findFrameEnd(value: string) {
  const lf = value.indexOf('\n\n')
  const crlf = value.indexOf('\r\n\r\n')
  if (lf === -1 && crlf === -1) return null
  if (crlf !== -1 && (lf === -1 || crlf < lf)) {
    return { index: crlf, length: 4 }
  }
  return { index: lf, length: 2 }
}
