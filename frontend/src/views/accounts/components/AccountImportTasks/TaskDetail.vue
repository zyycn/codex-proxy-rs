<script setup lang="ts">
import type { AccountImportTaskDetail } from '@/api'
import { ArrowUpRight, Check, CircleAlert, Square, X } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { itemDescription, itemStates, outcomeOrder, processed, taskLabel, taskTime } from './presenter'

const props = defineProps<{ task: AccountImportTaskDetail, stopping: boolean }>()
const emit = defineEmits<{ stop: [], viewAccounts: [] }>()
const filter = shallowRef('all')
const filterOptions = [
  { label: '全部', value: 'all' },
  { label: '需处理', value: 'attention' },
]
const hasAttention = computed(() => props.task.counts.failed + props.task.counts.unknown > 0)
const visibleItems = computed(() => filter.value === 'attention'
  ? props.task.items.filter(item => item.status === 'failed' || item.status === 'unknown')
  : props.task.items)

watch(() => props.task.taskId, () => {
  filter.value = hasAttention.value ? 'attention' : 'all'
}, { immediate: true })

watch(hasAttention, (value) => {
  if (value) {
    filter.value = 'attention'
  }
})
</script>

<template>
  <section class="min-w-0 space-y-5" aria-label="导入任务详情">
    <div class="flex flex-wrap items-start justify-between gap-3">
      <div>
        <h3 class="text-base font-bold text-cp-text">
          {{ taskLabel(task) }}
        </h3>
        <p class="mt-1 text-xs text-cp-text-secondary">
          {{ taskTime(task.createdAt) }} 创建
        </p>
      </div>
      <BaseButton
        v-if="!task.finishedAt"
        size="sm"
        :loading="stopping"
        :disabled="task.stopRequested || task.counts.pending === 0"
        @click="emit('stop')"
      >
        <template #icon>
          <Square class="size-3 text-cp-error" />
        </template>
        {{ task.stopRequested || task.counts.pending === 0 ? '等待当前条目结束' : '停止未开始条目' }}
      </BaseButton>
      <BaseButton v-else size="sm" variant="soft" @click="emit('viewAccounts')">
        查看账号 <ArrowUpRight class="size-3.5" />
      </BaseButton>
    </div>

    <div>
      <div class="mb-2 flex items-baseline justify-between gap-3 text-xs">
        <span class="text-cp-text-secondary">条目进度 <strong class="font-mono font-medium text-cp-text">{{ processed(task) }} / {{ task.total }}</strong></span>
        <span class="font-semibold text-cp-text">已入库 {{ task.counts.importedAccounts }} 个账号</span>
      </div>
      <div
        class="flex h-2 overflow-hidden rounded-full bg-cp-fill-alter"
        role="progressbar"
        aria-label="条目处理进度"
        :aria-valuenow="processed(task)"
        :aria-valuemax="task.total"
        :aria-valuemin="0"
      >
        <div v-for="status in outcomeOrder" :key="status" :class="itemStates[status].fill" :style="{ width: `${task.counts[status] / task.total * 100}%` }" />
      </div>
      <div class="mt-3 flex flex-wrap gap-x-4 gap-y-2 text-xs text-cp-text-secondary">
        <span v-for="status in outcomeOrder" :key="status" class="inline-flex items-center gap-1.5">
          <span class="size-1.5 rounded-full" :class="itemStates[status].fill" aria-hidden="true" />
          {{ itemStates[status].label }} <span class="font-mono text-cp-text">{{ task.counts[status] }}</span>
        </span>
      </div>
    </div>

    <div>
      <div v-if="hasAttention" class="mb-3 flex flex-wrap items-center gap-x-4 gap-y-3">
        <p v-if="task.counts.unknown" class="flex min-w-0 items-start gap-2 text-xs leading-relaxed text-cp-warning-text">
          <CircleAlert class="mt-0.5 size-3.5 shrink-0" />
          待核对条目可能已入库，请先查看账号，再决定是否重新导入
        </p>
        <BaseSegmented
          v-model="filter"
          class="ml-auto shrink-0"
          label="筛选导入明细"
          :options="filterOptions"
          size="sm"
        />
      </div>
      <div>
        <BaseScrollbar :key="`${task.taskId}-${filter}`" max-height="min(18rem, 32dvh)">
          <BaseEmpty v-if="!visibleItems.length" size="sm" title="暂无需要处理的条目" surface="none" />
          <ol v-else class="m-0 list-none space-y-1 p-0" aria-label="导入条目结果">
            <li
              v-for="item in visibleItems"
              :key="item.index"
              class="grid grid-cols-[1.75rem_minmax(0,1fr)_4.5rem] items-center gap-x-3 gap-y-1 rounded-cp-sm px-3 py-2.5 odd:bg-cp-fill-quaternary sm:grid-cols-[2rem_2rem_minmax(0,1fr)_4.5rem]"
            >
              <span class="font-mono text-xs text-cp-text-secondary">{{ String(item.index).padStart(2, '0') }}</span>
              <ProviderIconGroup :provider="item.provider" size="sm" />
              <p class="col-span-2 col-start-2 row-start-2 min-w-0 text-xs leading-relaxed wrap-anywhere text-cp-text-secondary sm:col-span-1 sm:col-start-3 sm:row-start-1">
                {{ itemDescription(item) }}
              </p>
              <span class="col-start-3 row-start-1 inline-flex items-center justify-self-end sm:col-start-4" :title="itemStates[item.status].label">
                <template v-if="item.status === 'succeeded' || item.status === 'failed'">
                  <Check v-if="item.status === 'succeeded'" class="size-4 text-cp-success-text" aria-hidden="true" />
                  <X v-else class="size-4 text-cp-error-text" aria-hidden="true" />
                  <span class="sr-only">{{ itemStates[item.status].label }}</span>
                </template>
                <span v-else class="rounded-cp-sm px-2 py-0.5 text-[11px] font-semibold" :class="itemStates[item.status].color">
                  {{ itemStates[item.status].label }}
                </span>
              </span>
            </li>
          </ol>
        </BaseScrollbar>
      </div>
    </div>
  </section>
</template>
