<script setup lang="ts">
import type { AccountImportTask, AccountImportTaskDetail } from '@/api'

import { ZButton, ZDialog, ZEmpty, ZIconButton, ZPopover, ZSelect } from '@codex-proxy/ui'
import { CircleAlert, ListTodo } from '@lucide/vue'
import { computed, shallowRef, useId, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { taskLabel } from './presenter'
import TaskDetail from './TaskDetail.vue'

const props = defineProps<{
  tasks: AccountImportTask[]
  selectedId: string
  detail: AccountImportTaskDetail | null
  loading: boolean
  stopping: boolean
  error: string
}>()

const emit = defineEmits<{ select: [id: string], refresh: [], stop: [], viewAccounts: [] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const retentionOpen = shallowRef(false)
const retentionId = useId()
const taskOptions = computed(() => props.tasks.map(task => ({
  value: task.taskId,
  label: task.createdAtDisplay,
  description: t('accounts.importTasks.items', {
    total: task.total,
    value: taskLabel(task),
  }),
})))

watch(open, () => {
  retentionOpen.value = false
})

function handleRetentionKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape' && retentionOpen.value) {
    // 先关闭说明浮层，避免同一次按键也关闭任务弹窗。
    event.preventDefault()
    event.stopPropagation()
    retentionOpen.value = false
  }
}
</script>

<template>
  <ZDialog v-model="open" :title="t('accounts.filters.importTasks')" :description="t('accounts.importTasks.description')" width="48rem">
    <div v-if="error" role="alert" class="mb-4 flex items-center justify-between gap-3 rounded-cp bg-cp-warning-container p-3 text-xs text-cp-warning-on-container">
      <span>{{ t('accounts.importTasks.progressUnavailable', { value: error.replace(/[。.]\s*$/u, '') }) }}</span>
      <ZButton size="small" :loading="loading" @click="emit('refresh')">
        {{ t('common.actions.retry') }}
      </ZButton>
    </div>
    <ZEmpty v-if="!tasks.length" class="min-h-[min(20rem,50dvh)] content-center" :icon="ListTodo" :title="loading ? t('accounts.importTasks.loadingImportTasks') : t('accounts.importTasks.noImportTasks')" :description="t('accounts.importTasks.empty')" />
    <div v-else class="min-h-[min(20rem,50dvh)] min-w-0">
      <div v-if="tasks.length > 1" class="mb-5">
        <ZSelect
          :model-value="selectedId"
          :options="taskOptions"
          size="small"
          :aria-label="t('accounts.importTasks.switchImportTasks')"
          class="w-full min-w-0 sm:max-w-96"
          @update:model-value="emit('select', $event ?? selectedId)"
        />
      </div>
      <TaskDetail v-if="detail" :task="detail" :stopping="stopping" @stop="emit('stop')" @view-accounts="emit('viewAccounts')" />
      <ZEmpty v-else class="min-h-[min(16rem,40dvh)] content-center" :title="loading ? t('accounts.importTasks.loadingItemResults') : t('accounts.importTasks.selectTask')" surface="none" />
    </div>
    <template #footer>
      <div class="flex w-full flex-wrap items-center justify-between gap-3">
        <ZPopover v-model="retentionOpen" trigger="hover-click" placement="top-start" :hover-delay="240">
          <template #reference>
            <ZIconButton
              :aria-label="t('accounts.importTasks.taskRetentionInstructions')"
              size="small"
              :aria-expanded="retentionOpen"
              :aria-describedby="retentionOpen ? retentionId : undefined"
              @keydown="handleRetentionKeydown"
            >
              <CircleAlert class="size-4" />
            </ZIconButton>
          </template>
          <div :id="retentionId" role="tooltip" class="grid w-72 max-w-[calc(100vw-2rem)] gap-2 p-4 text-cp-xs leading-relaxed text-cp-text-secondary">
            <p class="m-0">
              {{ t('accounts.importTasks.resultsRetainedForHour') }}
            </p>
            <p class="m-0">
              {{ t('accounts.importTasks.retentionHelp') }}
            </p>
          </div>
        </ZPopover>
        <ZButton @click="open = false">
          {{ t('common.actions.close') }}
        </ZButton>
      </div>
    </template>
  </ZDialog>
</template>
