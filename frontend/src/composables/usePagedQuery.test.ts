import { describe, expect, it } from 'vitest'
import { effectScope } from 'vue'

import { usePagedQuery } from './usePagedQuery'

describe('usePagedQuery', () => {
  it('keeps the newest response when requests finish out of order', async () => {
    const first = deferred<PageResult>()
    const second = deferred<PageResult>()
    const requests = [first, second]
    const scope = effectScope()
    const query = scope.run(() =>
      usePagedQuery({
        initialPageSize: 20,
        load: () => requests.shift()!.promise,
      }),
    )!

    const firstLoad = query.execute()
    query.page.value = 2
    const secondLoad = query.execute()
    second.resolve(pageResult('new', 2))
    await secondLoad
    first.resolve(pageResult('stale', 1))
    await firstLoad

    expect(query.items.value).toEqual([{ id: 'new' }])
    expect(query.page.value).toBe(2)
    scope.stop()
  })

  it('moves to the last valid page after deleting the final row', async () => {
    const scope = effectScope()
    const results = [
      { items: [], page: { page: 3, pageSize: 20, total: 21, totalPages: 2 } },
      pageResult('last', 2),
    ]
    const query = scope.run(() =>
      usePagedQuery({
        initialPageSize: 20,
        load: async () => results.shift()!,
      }),
    )!
    query.page.value = 3

    await query.execute()

    expect(query.page.value).toBe(2)
    expect(query.items.value).toEqual([{ id: 'last' }])
    scope.stop()
  })
})

interface PageResult {
  items: Array<{ id: string }>
  page: {
    page: number
    pageSize: number
    total: number
    totalPages: number
  }
}

function pageResult(id: string, page: number): PageResult {
  return {
    items: [{ id }],
    page: { page, pageSize: 20, total: 40, totalPages: 2 },
  }
}

function deferred<Value>() {
  let resolve!: (value: Value) => void
  const promise = new Promise<Value>((resolvePromise) => {
    resolve = resolvePromise
  })
  return { promise, resolve }
}
