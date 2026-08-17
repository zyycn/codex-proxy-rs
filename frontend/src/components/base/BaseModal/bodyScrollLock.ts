import { useScrollLock } from '@vueuse/core'

let bodyScrollLocked: ReturnType<typeof useScrollLock> | undefined
let lockCount = 0

function getBodyScrollLock() {
  if (typeof document === 'undefined')
    return undefined

  bodyScrollLocked ??= useScrollLock(document.body)
  return bodyScrollLocked
}

export function lockBodyScroll() {
  const scrollLock = getBodyScrollLock()
  if (!scrollLock)
    return

  lockCount += 1
  if (lockCount === 1)
    scrollLock.value = true
}

export function unlockBodyScroll() {
  if (!bodyScrollLocked)
    return

  lockCount = Math.max(0, lockCount - 1)
  if (lockCount === 0)
    bodyScrollLocked.value = false
}
