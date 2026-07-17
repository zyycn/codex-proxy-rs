import type { SystemUpdateEvent } from '@/api/streams/system-update'

export function reduceSystemUpdateLogs(
  logs: SystemUpdateEvent[],
  event: SystemUpdateEvent,
  limit = 200,
) {
  const withoutDuplicate = logs.filter(log => log.id !== event.id)
  return [...withoutDuplicate, event].slice(-limit)
}
