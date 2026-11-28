import type { Ref } from 'vue'
import { useConfirmDialog, useEventListener } from '@vueuse/core'
import { onScopeDispose, shallowRef } from 'vue'
import { onBeforeRouteLeave } from 'vue-router'

export function useSettingsLeaveGuard(hasChanges: Readonly<Ref<boolean>>) {
  const showLeaveConfirm = shallowRef(false)
  const { reveal, confirm, cancel } = useConfirmDialog(showLeaveConfirm)
  let discardChanges = false
  let pending: Promise<boolean> | undefined

  onBeforeRouteLeave((to) => {
    if (to.matched.some(record => record.path === '/settings') || !hasChanges.value)
      return true
    pending ??= reveal().then(result => !result.isCanceled).finally(() => {
      pending = undefined
    })
    return pending
  })

  function confirmLeave() {
    discardChanges = true
    showLeaveConfirm.value = false
  }

  // 等退场完成再决定导航，避免路由切换提前卸载确认框。
  function finishLeave() {
    if (discardChanges)
      confirm()
    else
      cancel()
    discardChanges = false
  }

  useEventListener(() => hasChanges.value ? window : undefined, 'beforeunload', (event: BeforeUnloadEvent) => {
    event.preventDefault()
    event.returnValue = ''
  })
  onScopeDispose(cancel)

  return { showLeaveConfirm, confirmLeave, finishLeave }
}
