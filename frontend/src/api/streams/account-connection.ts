import { z } from 'zod'

import { API_BASE_URL } from '../constants'

const requestPayloadSchema = z
  .object({
    input: z
      .array(
        z
          .object({
            content: z
              .array(
                z
                  .object({
                    type: z.string().optional(),
                    text: z.string().optional(),
                  })
                  .passthrough(),
              )
              .optional(),
          })
          .passthrough(),
      )
      .optional(),
  })
  .passthrough()

const accountConnectionEventSchema = z.discriminatedUnion('type', [
  z.object({ type: z.literal('test_start'), model: z.string().optional() }),
  z.object({ type: z.literal('request'), payload: requestPayloadSchema.optional() }),
  z.object({ type: z.literal('status'), text: z.string().optional() }),
  z.object({ type: z.literal('content'), text: z.string().optional() }),
  z.object({
    type: z.literal('test_complete'),
    success: z.boolean().optional(),
    error: z.string().optional(),
    accountStatus: z.string().optional(),
  }),
  z.object({
    type: z.literal('error'),
    error: z.string().optional(),
    accountStatus: z.string().optional(),
  }),
])

export type AccountConnectionEvent = z.infer<typeof accountConnectionEventSchema>

export function openAccountConnectionStream(options: {
  accountId: string
  modelId: string
  onEvent: (event: AccountConnectionEvent) => void
  onError: () => void
}) {
  const params = new URLSearchParams({
    id: options.accountId,
    modelId: options.modelId,
  })
  const source = new EventSource(`${API_BASE_URL}/api/admin/accounts/test?${params}`, {
    withCredentials: true,
  })
  source.onmessage = (event) => {
    try {
      options.onEvent(accountConnectionEventSchema.parse(JSON.parse(event.data)))
    }
    catch {
      options.onError()
    }
  }
  source.onerror = options.onError
  return source
}
