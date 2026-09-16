<script setup lang="ts">
import type { AccountImportTask, AccountImportTaskDetail } from '@/api'
import { CircleAlert, ListTodo } from '@lucide/vue'
import { computed, shallowRef, useId, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BasePopover from '@/components/base/BasePopover.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { taskLabel, taskTime } from './presenter'
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
const open = defineModel<boolean>({ required: true })
const retentionOpen = shallowRef(false)
const retentionId = useId()
const taskOptions = computed(() => props.tasks.map(task => ({
  value: task.taskId,
  label: taskTime(task.createdAt),
  description: `${task.total} 个条目 · ${taskLabel(task)}`,
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
  <BaseModal v-model="open" title="导入任务" description="关闭页面后继续导入，再次打开即可查看进度" size="lg">
    <div v-if="error" role="alert" class="mb-4 flex items-center justify-between gap-3 rounded-cp bg-cp-warning-container p-3 text-xs text-cp-warning-on-container">
      <span>进度暂未更新：{{ error.replace(/[。.]\s*$/u, '') }}，恢复连接后将自动刷新</span>
      <BaseButton size="sm" :loading="loading" @click="emit('refresh')">
        重试
      </BaseButton>
    </div>
    <BaseEmpty v-if="!tasks.length" class="min-h-[min(20rem,50dvh)] content-center" :icon="ListTodo" :title="loading ? '正在读取导入任务' : '暂无导入任务'" description="新建一次账号导入，即可在这里查看逐条入库结果" />
    <div v-else class="min-h-[min(20rem,50dvh)] min-w-0">
      <div v-if="tasks.length > 1" class="mb-5">
        <BaseSelect
          :model-value="selectedId"
          :options="taskOptions"
          size="sm"
          aria-label="切换导入任务"
          class="w-full min-w-0 sm:max-w-96"
          @update:model-value="emit('select', $event)"
        />
      </div>
      <TaskDetail v-if="detail" :task="detail" :stopping="stopping" @stop="emit('stop')" @view-accounts="emit('viewAccounts')" />
      <BaseEmpty v-else class="min-h-[min(16rem,40dvh)] content-center" :title="loading ? '正在读取条目结果' : '请选择一个任务'" surface="none" />
    </div>
    <template #footer>
      <div class="flex w-full flex-wrap items-center justify-between gap-3">
        <BasePopover v-model="retentionOpen" trigger="hover-click" placement="top-start" :hover-delay="240">
          <template #trigger>
            <BaseIconButton
              label="任务保留说明"
              size="sm"
              :aria-expanded="retentionOpen"
              :aria-describedby="retentionOpen ? retentionId : undefined"
              @keydown="handleRetentionKeydown"
            >
              <CircleAlert class="size-4" />
            </BaseIconButton>
          </template>
          <div :id="retentionId" role="tooltip" class="grid w-72 max-w-[calc(100vw-2rem)] gap-2 p-4 text-cp-xs leading-relaxed text-cp-text-secondary">
            <p class="m-0">
              结果保留 1 小时
            </p>
            <p class="m-0">
              服务重启后任务记录消失，已入库账号保留
            </p>
          </div>
        </BasePopover>
        <BaseButton @click="open = false">
          关闭
        </BaseButton>
      </div>
    </template>
  </BaseModal>
</template>
