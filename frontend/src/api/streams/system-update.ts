import { z } from 'zod'

import { API_BASE_URL } from '../constants'

const systemUpdateEventSchema = z.object({
  id: z.string(),
  operationId: z.string().nullable().optional(),
  level: z.enum(['info', 'success', 'warning', 'error']),
  step: z.string().nullable().optional(),
  message: z.string(),
  terminal: z.boolean().optional(),
  at: z.string(),
})

export type SystemUpdateEvent = z.infer<typeof systemUpdateEventSchema>

export function openSystemUpdateStream(options: {
  onOpen: () => void
  onEvent: (event: SystemUpdateEvent) => void
  onError: (closed: boolean) => void
  onParseError: () => void
}) {
  if (typeof EventSource === 'undefined')
    return null

  const source = new EventSource(`${API_BASE_URL}/api/admin/system/update-events`, {
    withCredentials: true,
  })
  source.onopen = options.onOpen
  source.addEventListener('update', (event) => {
    try {
      options.onEvent(systemUpdateEventSchema.parse(JSON.parse((event as MessageEvent<string>).data)))
    }
    catch {
      options.onParseError()
    }
  })
  source.onerror = () => {
    options.onError(source.readyState === EventSource.CLOSED)
  }
  return source
}
