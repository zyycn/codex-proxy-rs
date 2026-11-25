import type { ProviderRequestProfiles, ProviderRequestProfileUpdates } from '@/api'
import { isEqual } from 'es-toolkit'

export function requestProfileChanges(previous: ProviderRequestProfiles, current: ProviderRequestProfiles): ProviderRequestProfileUpdates {
  const changes: ProviderRequestProfileUpdates = {}
  for (const id of new Set([
    ...Object.keys(previous),
    ...Object.keys(current),
  ])) {
    if (!isEqual(previous[id], current[id]))
      changes[id] = current[id] ?? null
  }
  return changes
}
