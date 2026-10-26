<script setup lang="ts">
import type { AccountImportTaskDetail } from '@/api'

import { ZButton, ZEmpty, ZScrollbar, ZSegmented } from '@codex-proxy/ui'
import { ArrowUpRight, Check, CircleAlert, Square, X } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { itemDescription, itemStates, outcomeOrder, processed, taskLabel } from './presenter'

const props = defineProps<{ task: AccountImportTaskDetail, stopping: boolean }>()

const emit = defineEmits<{ stop: [], viewAccounts: [] }>()

const { t } = useI18n({ useScope: 'global' })

const filter = shallowRef('all')
const filterOptions = computed(() => ([
  {
    label: t('common.values.all'),
    value: 'all',
  },
  {
    label: t('accounts.importTasks.needProcessing'),
    value: 'attention',
  },
]))
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
  <section class="min-w-0 space-y-5" :aria-label="t('accounts.importTasks.importTaskDetails')">
    <div class="flex flex-wrap items-start justify-between gap-3">
      <div>
        <h3 class="text-base font-bold text-cp-text">
          {{ taskLabel(task) }}
        </h3>
        <p class="mt-1 text-xs text-cp-text-secondary">
          {{ t('accounts.importTasks.created', { createdAtDisplay: task.createdAtDisplay }) }}
        </p>
      </div>
      <div v-if="!task.finishedAt" class="grid max-w-full justify-items-end gap-1.5">
        <ZButton
          size="small"
          :loading="stopping"
          :disabled="task.stopRequested || task.counts.pending === 0"
          @click="emit('stop')"
        >
          <template #icon>
            <Square class="size-3 text-cp-error" />
          </template>
          {{ task.stopRequested || task.counts.pending === 0 ? t('accounts.importTasks.waitForCurrentEntryToEnd') : t('accounts.importTasks.stopUnstartedEntries') }}
        </ZButton>
        <p v-if="task.counts.pending > 0 || task.stopRequested" class="m-0 max-w-64 text-right text-cp-xs text-cp-text-secondary">
          {{ t('accounts.importTasks.stopHelp') }}
        </p>
      </div>
      <ZButton v-else size="small" type="primary" variant="plain" @click="emit('viewAccounts')">
        {{ t('accounts.importTasks.viewAccount') }} <ArrowUpRight class="size-3.5" />
      </ZButton>
    </div>

    <div>
      <div class="mb-2 flex items-baseline justify-between gap-3 text-xs">
        <span class="text-cp-text-secondary">{{ t('accounts.importTasks.entryProgress') }} <strong class="font-mono font-medium text-cp-text">{{ processed(task) }} / {{ task.total }}</strong></span>
        <span class="font-semibold text-cp-text">{{ t('accounts.importTasks.accountsImported', { importedAccounts: task.counts.importedAccounts }) }}</span>
      </div>
      <div
        class="flex h-2 overflow-hidden rounded-full bg-cp-fill-alter"
        role="progressbar"
        :aria-label="t('accounts.importTasks.itemProcessingProgress')"
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
          {{ t('accounts.importTasks.itemsToBeCheckedMayHaveBeen') }}
        </p>
        <ZSegmented
          v-model="filter"
          class="ml-auto shrink-0"
          :aria-label="t('accounts.importTasks.filterImportDetails')"
          :options="filterOptions"
          size="small"
        />
      </div>
      <div>
        <ZScrollbar :key="`${task.taskId}-${filter}`" max-height="min(18rem, 32dvh)">
          <ZEmpty v-if="!visibleItems.length" size="small" :title="t('accounts.importTasks.thereAreNoItemsToBeProcessed')" surface="none" />
          <ol v-else class="m-0 list-none space-y-1 p-0" :aria-label="t('accounts.importTasks.importEntryResults')">
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
        </ZScrollbar>
      </div>
    </div>
  </section>
</template>
