import { useScrollLock } from '@vueuse/core'

const bodyScrollLocked = useScrollLock(document.body)
let lockCount = 0

export function lockBodyScroll() {
  lockCount += 1
  if (lockCount === 1)
    bodyScrollLocked.value = true
}

export function unlockBodyScroll() {
  lockCount = Math.max(0, lockCount - 1)
  if (lockCount === 0)
    bodyScrollLocked.value = false
}
