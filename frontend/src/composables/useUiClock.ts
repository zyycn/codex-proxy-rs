import { createSharedComposable, useIntervalFn, useNow } from '@vueuse/core'

export const useUiClock = createSharedComposable(() =>
  useNow({
    scheduler: callback => useIntervalFn(callback, 30_000),
  }),
)
