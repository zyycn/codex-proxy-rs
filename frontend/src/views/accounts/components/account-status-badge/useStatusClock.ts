import { createSharedComposable, useIntervalFn, useNow } from '@vueuse/core'

export const useStatusClock = createSharedComposable(() =>
  useNow({
    scheduler: callback => useIntervalFn(callback, 30_000),
  }),
)
