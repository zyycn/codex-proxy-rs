import { shallowRef } from 'vue'

type MaybePromise<T> = T | Promise<T>

export function useIdSet<T extends string | number>() {
  const ids = shallowRef<Set<T>>(new Set())

  function has(id: T) {
    return ids.value.has(id)
  }

  function add(id: T) {
    ids.value = new Set(ids.value).add(id)
  }

  function remove(id: T) {
    const next = new Set(ids.value)
    next.delete(id)
    ids.value = next
  }

  async function run<R>(id: T, task: () => MaybePromise<R>) {
    if (has(id)) {
      return undefined
    }

    add(id)
    try {
      return await task()
    } finally {
      remove(id)
    }
  }

  return {
    ids,
    has,
    add,
    remove,
    run,
  }
}
