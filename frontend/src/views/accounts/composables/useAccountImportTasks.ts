import type { AccountImportTask, AccountImportTaskDetail } from '@/api'
import { computed, onMounted, onScopeDispose, shallowRef, watch } from 'vue'
import { getAccountImportTask, getAccountImportTasks, stopAccountImportTask } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { errorMessage } from '@/utils/async'

export function useAccountImportTasks(options: { reload: () => Promise<unknown> }) {
  const open = shallowRef(false)
  const tasks = shallowRef<AccountImportTask[]>([])
  const selectedId = shallowRef('')
  const detail = shallowRef<AccountImportTaskDetail | null>(null)
  const error = shallowRef('')
  const loading = shallowRef(false)
  const stopAction = useAsyncAction()
  const activeCount = computed(() => tasks.value.filter(task => !task.finishedAt).length)
  let timer: ReturnType<typeof setTimeout> | undefined
  let controller: AbortController | undefined
  let disposed = false
  let initialized = false

  async function refresh(background = false) {
    clearTimeout(timer)
    controller?.abort()
    const current = new AbortController()
    controller = current
    if (!background)
      loading.value = true
    try {
      const result = await getAccountImportTasks({
        silent: true,
        signal: current.signal,
      })
      if (current.signal.aborted)
        return
      const previous = new Map(tasks.value.map(task => [task.taskId, task.counts.importedAccounts]))
      // 首次查询只建立进度基线，历史入库结果不应重复触发页面的初始加载。
      const changed = initialized && result.items.some(task => task.counts.importedAccounts > (previous.get(task.taskId) ?? 0))
      initialized = true
      tasks.value = result.items
      if (changed)
        void options.reload().catch(() => undefined)
      if (!tasks.value.some(task => task.taskId === selectedId.value)) {
        detail.value = null
        selectedId.value = tasks.value[0]?.taskId ?? ''
      }
      if (open.value && selectedId.value) {
        const selected = selectedId.value
        const next = await getAccountImportTask({
          taskId: selected,
        }, {
          silent: true,
          signal: current.signal,
        })
        if (!current.signal.aborted && selectedId.value === selected)
          detail.value = next
      }
      if (current.signal.aborted)
        return
      error.value = ''
    }
    catch (cause) {
      if (!current.signal.aborted)
        error.value = errorMessage(cause, '进度读取失败，请重试')
    }
    finally {
      if (!current.signal.aborted && !disposed) {
        loading.value = false
        // 面板打开时也查询终态列表，以发现其他页面提交的任务和过期记录。
        if (open.value || activeCount.value > 0)
          timer = setTimeout(() => void refresh(true), error.value ? 5000 : 1500)
      }
    }
  }

  function select(taskId: string) {
    selectedId.value = taskId
    detail.value = null
    void refresh()
  }

  function created(task: AccountImportTask) {
    tasks.value = [task, ...tasks.value.filter(entry => entry.taskId !== task.taskId)]
    selectedId.value = task.taskId
    detail.value = null
    if (open.value)
      void refresh()
    else
      open.value = true
  }

  async function stop() {
    const taskId = selectedId.value
    if (!taskId)
      return
    await stopAction.run(async () => {
      await stopAccountImportTask({
        taskId,
      })
      await refresh()
    })
  }

  watch(open, () => void refresh())
  onMounted(() => void refresh())
  onScopeDispose(() => {
    disposed = true
    controller?.abort()
    clearTimeout(timer)
  })

  return { open, tasks, detail, selectedId, error, loading, activeCount, stopping: stopAction.loading, select, created, refresh, stop }
}
