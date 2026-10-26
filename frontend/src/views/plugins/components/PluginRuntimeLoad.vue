<script setup lang="ts">
import type { PluginInstanceRuntimeLoad } from '@/api'
import { ChevronDown } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import PluginHelpPopover from './PluginHelpPopover.vue'

const props = defineProps<{ load: PluginInstanceRuntimeLoad, revision: number | null }>()
const { t } = useI18n({ useScope: 'global' })

const values = computed(() => [
  {
    label: t('plugins.runtime.activeCalls'),
    value: props.load.activeCalls,
  },
  {
    label: t('plugins.runtime.executingCalls'),
    value: props.load.executingCalls,
  },
  {
    label: t('plugins.runtime.queuedCalls'),
    value: props.load.queuedCalls,
  },
  {
    label: t('plugins.runtime.resumingCalls'),
    value: props.load.resumingCalls,
  },
  {
    label: t('plugins.runtime.pendingCallbacks'),
    value: props.load.pendingCallbacks,
  },
  {
    label: t('plugins.runtime.activeStreams'),
    value: props.load.activeStreams,
  },
])

const summary = computed(() => values.value.every(item => item.value === 0)
  ? t('plugins.runtime.idle')
  : t('plugins.runtime.inFlightSummary', { count: props.load.activeCalls }))
</script>

<template>
  <section class="flex items-start gap-2 text-cp-xs" :aria-label="t('plugins.runtime.title')">
    <details class="group min-w-0 flex-1">
      <summary class="flex cursor-pointer list-none flex-wrap items-center justify-between gap-x-3 gap-y-1 rounded-cp py-1 text-cp-text-secondary outline-none hover:text-cp-text focus-visible:ring-2 focus-visible:ring-cp-control-outline active:text-cp-primary-text [&::-webkit-details-marker]:hidden">
        <span>{{ t('plugins.runtime.title') }}</span>
        <span class="inline-flex items-center gap-2">
          {{ summary }}
          <ChevronDown class="size-3.5 shrink-0 transition-transform group-open:rotate-180 motion-reduce:transition-none" aria-hidden="true" />
        </span>
      </summary>
      <dl class="m-0 mt-2 grid grid-cols-2 gap-x-4 gap-y-2 sm:grid-cols-3">
        <div v-for="item in values" :key="item.label" class="flex min-w-0 items-baseline gap-2">
          <dt class="text-cp-text-secondary">
            {{ item.label }}
          </dt>
          <dd class="m-0 shrink-0 font-mono text-cp-text tabular-nums">
            {{ item.value }}
          </dd>
        </div>
      </dl>
    </details>
    <PluginHelpPopover :label="t('plugins.runtime.helpTitle')" class="mt-0.5">
      <p v-if="revision !== null" class="m-0 font-emphasis text-cp-text">
        {{ t('plugins.runtime.revision', { revision }) }}
      </p>
      <p class="m-0">
        {{ t('plugins.runtime.description') }}
      </p>
    </PluginHelpPopover>
  </section>
</template>
