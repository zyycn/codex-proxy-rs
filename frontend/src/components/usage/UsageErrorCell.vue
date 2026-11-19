<script setup lang="ts">
import type { UsageListRecord } from '@/api'
import { CheckCheck, CircleHelp, Network, Server, Unplug } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { usageErrorCode, usageErrorOrigin, usageErrorTitle } from './shared/errors'

const props = defineProps<{ record: UsageListRecord }>()

const originIcons = {
  proxy: Network,
  upstream: Server,
  connection: Unplug,
  unknown: CircleHelp,
}

const { t } = useI18n({ useScope: 'global' })
const message = computed(() => props.record.errorMessage?.replace(/^OpenAI upstream failure: /, ''))
const code = computed(() => usageErrorCode(props.record) || props.record.errorKind)
const origin = computed(() => usageErrorOrigin(props.record))
const outcome = computed(() => usageErrorTitle(props.record))
const summary = computed(() => message.value || code.value || t('usage.error.noDetails'))
</script>

<template>
  <div class="min-w-0 py-0.5">
    <div class="flex min-w-0 items-start gap-2">
      <span
        class="inline-flex size-5 shrink-0 items-center justify-center"
        :class="origin.className"
        role="img"
        :aria-label="origin.label"
        :title="origin.hint ? `${origin.label} · ${origin.hint}` : origin.label"
      >
        <component :is="originIcons[origin.kind]" class="size-3.5" aria-hidden="true" />
      </span>
      <span class="line-clamp-2 min-w-0 text-z-sm font-medium wrap-anywhere text-z-text" :title="summary" translate="no">{{ summary }}</span>
    </div>
    <div class="mt-1 flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 text-z-xs text-z-text-secondary">
      <span>{{ outcome }}</span>
      <code v-if="message && code && code !== message" class="truncate font-mono" :title="code" translate="no">{{ code }}</code>
      <span v-if="record.upstreamStatusCode != null" class="shrink-0 font-mono text-z-xs text-z-text-secondary" :title="t('usage.error.upstreamStatusCode')">
        {{ t('usage.error.upstreamHttpStatus', { status: record.upstreamStatusCode }) }}
      </span>
      <span v-if="record.clientStatusCode != null && record.clientStatusCode !== record.upstreamStatusCode" class="shrink-0 font-mono text-z-xs text-z-text-secondary" :title="t('usage.error.clientStatusCode')">
        {{ t('usage.error.clientHttpStatus', { status: record.clientStatusCode }) }}
      </span>
      <span v-if="record.recovered" class="inline-flex shrink-0 text-z-success-text" role="img" :aria-label="t('usage.error.recovered')" :title="t('usage.error.recovered')">
        <CheckCheck class="size-4" :stroke-width="2.25" aria-hidden="true" />
      </span>
    </div>
  </div>
</template>
