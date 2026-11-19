<script setup lang="ts">
import type { UsageListRecord } from '@/api'
import { ZPopover, ZScrollbar } from '@codex-proxy/ui'
import { CheckCheck, CircleHelp, Network, Server, Unplug } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { usageErrorOrigin, usageErrorReason, usageErrorTitle } from './shared/errors'

const props = defineProps<{ record: UsageListRecord }>()

const originIcons = {
  proxy: Network,
  upstream: Server,
  connection: Unplug,
  unknown: CircleHelp,
}

const { t } = useI18n({ useScope: 'global' })
const code = computed(() => props.record.errorCode || props.record.errorKind)
const errorType = computed(() => props.record.errorKind || props.record.errorCode)
const origin = computed(() => usageErrorOrigin(props.record))
const outcome = computed(() => usageErrorTitle(props.record))
const summary = computed(() => {
  const reason = usageErrorReason(code.value) || props.record.errorMessage || code.value
  if (reason)
    return reason
  return props.record.websocketCloseCode != null
    ? `WS ${props.record.websocketCloseCode}`
    : t('usage.error.noDetails')
})
</script>

<template>
  <div class="min-w-0 py-0.5">
    <div class="flex min-w-0 items-center gap-1.5 text-z-xs text-z-text-secondary">
      <span
        class="inline-flex size-5 shrink-0 items-center justify-center"
        :class="origin.className"
        role="img"
        :aria-label="origin.label"
        :title="origin.hint ? `${origin.label} · ${origin.hint}` : origin.label"
      >
        <component :is="originIcons[origin.kind]" class="size-3.5" aria-hidden="true" />
      </span>
      <span class="truncate">
        <span class="font-emphasis">{{ outcome }}</span>
        <span v-if="record.upstreamStatusCode != null" class="ms-1.5 font-mono" :title="t('usage.error.upstreamStatusCode')">
          {{ t('usage.error.upstreamHttpStatus', { status: record.upstreamStatusCode }) }}
        </span>
        <span v-if="record.clientStatusCode != null && record.clientStatusCode !== record.upstreamStatusCode" class="ms-1.5 font-mono" :title="t('usage.error.clientStatusCode')">
          {{ t('usage.error.clientHttpStatus', { status: record.clientStatusCode }) }}
        </span>
      </span>
      <span v-if="record.recovered" class="inline-flex shrink-0 text-z-success-text" role="img" :aria-label="t('usage.error.recovered')" :title="t('usage.error.recovered')">
        <CheckCheck class="size-4" :stroke-width="2.25" aria-hidden="true" />
      </span>
    </div>
    <ZPopover class="mt-1 flex!" trigger="hover-click" placement="top-start" role="tooltip">
      <template #reference="{ id, open }">
        <button
          type="button"
          class="block min-w-0 cursor-pointer truncate rounded-z-sm border-0 bg-transparent p-0 text-left text-z-sm font-medium text-z-text focus-visible:ring-2 focus-visible:ring-z-control-outline"
          :aria-expanded="open"
          :aria-describedby="open ? id : undefined"
          translate="no"
        >
          <template v-if="errorType && errorType !== summary">
            <code class="font-mono text-z-xs text-z-text-secondary">{{ errorType }}</code>
            <span class="mx-1 text-z-text-secondary" aria-hidden="true">·</span>
          </template>
          <span>{{ summary }}</span>
        </button>
      </template>
      <ZScrollbar class="w-max max-w-[min(26rem,calc(100vw-2rem))]" max-height="min(18rem, 60dvh)">
        <div class="grid gap-1 px-3 py-2 text-z-sm leading-relaxed whitespace-pre-wrap select-text wrap-anywhere" translate="no">
          <code v-if="code" class="font-mono font-emphasis">{{ code }}</code>
          <span v-if="record.errorKind && record.errorKind !== code">
            {{ t('usage.error.failureClassification') }} · <code>{{ record.errorKind }}</code>
          </span>
          <span v-if="record.websocketCloseCode != null">
            {{ t('usage.error.websocketCloseCode') }} · <code>{{ record.websocketCloseCode }}</code>
          </span>
          <span>{{ record.errorMessage || t('usage.error.noDetails') }}</span>
        </div>
      </ZScrollbar>
    </ZPopover>
  </div>
</template>
