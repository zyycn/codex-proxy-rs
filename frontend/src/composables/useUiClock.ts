import { createSharedComposable, useNow } from '@vueuse/core'

export const useUiClock = createSharedComposable(() =>
  useNow({ interval: 30_000 }),
)
