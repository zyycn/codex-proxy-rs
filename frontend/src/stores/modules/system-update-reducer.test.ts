import type { SystemUpdateEvent } from '@/api/streams/system-update'

import { describe, expect, it } from 'vitest'
import { reduceSystemUpdateLogs } from './system-update-reducer'

describe('system update event reducer', () => {
  it('deduplicates reconnect events and retains the newest bounded window', () => {
    const first = event('1', 'first')
    const replacement = event('1', 'replacement')
    const second = event('2', 'second')

    const logs = reduceSystemUpdateLogs(
      reduceSystemUpdateLogs(reduceSystemUpdateLogs([], first, 2), second, 2),
      replacement,
      2,
    )

    expect(logs.map(log => [log.id, log.message])).toEqual([
      ['2', 'second'],
      ['1', 'replacement'],
    ])
  })
})

function event(id: string, message: string): SystemUpdateEvent {
  return {
    id,
    level: 'info',
    message,
    at: '2026-07-17T00:00:00Z',
  }
}
