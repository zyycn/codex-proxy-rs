<script setup lang="ts">
import type { ApiKey } from '@/api'
import { ZPopover, ZTag } from '@codex-proxy/ui'
import { ChevronDown } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{ apiKey: ApiKey }>()
const { t } = useI18n({ useScope: 'global' })

const badge = computed(() => {
  if (!props.apiKey.enabled) {
    return {
      label: t('common.status.disabled'),
      tone: 'default',
    } as const
  }
  switch (props.apiKey.validityStatus) {
    case 'scheduled': return {
      label: t('keys.validity.scheduled'),
      tone: 'info',
    } as const
    case 'expired': return {
      label: t('keys.validity.expired'),
      tone: 'default',
    } as const
    default: return {
      label: props.apiKey.validity.expiresAt
        ? t('keys.validity.timeLimited')
        : t('keys.validity.noExpiry'),
      tone: 'success',
    } as const
  }
})
</script>

<template>
  <ZTag v-if="!apiKey.validity.expiresAt" :type="badge.tone" round>
    {{ badge.label }}
  </ZTag>
  <ZPopover v-else trigger="hover-click" placement="top" :hover-delay="240">
    <template #reference="{ id, open }">
      <button
        type="button"
        class="inline-flex cursor-pointer items-center rounded-full border-0 bg-transparent p-0 leading-none outline-none focus-visible:ring-2 focus-visible:ring-z-control-outline"
        :aria-controls="open ? id : undefined" :aria-expanded="open" aria-haspopup="dialog"
      >
        <ZTag :type="badge.tone" round>
          <span class="flex items-center gap-1 whitespace-nowrap">
            {{ badge.label }}
            <ChevronDown class="size-3" aria-hidden="true" />
          </span>
        </ZTag>
      </button>
    </template>

    <section role="dialog" :aria-label="t('keys.validity.label')" class="grid max-w-[calc(100vw-1rem)] gap-2 px-3 py-2.5 text-z-sm">
      <h3 class="m-0 text-z-sm font-emphasis text-z-text">
        {{ t('keys.validity.label') }}
      </h3>
      <dl class="m-0 grid grid-cols-[auto_auto] items-baseline gap-x-4 gap-y-1">
        <dt class="text-z-text-secondary">
          {{ t('keys.validity.startsAt') }}
        </dt>
        <dd class="m-0 text-right tabular-nums">
          <time :datetime="apiKey.validity.startsAt ?? undefined">{{ apiKey.startsAtDisplay }}</time>
        </dd>
        <dt class="text-z-text-secondary">
          {{ t('keys.validity.expiresAt') }}
        </dt>
        <dd class="m-0 text-right tabular-nums">
          <time :datetime="apiKey.validity.expiresAt">{{ apiKey.expiresAtDisplay }}</time>
        </dd>
      </dl>
    </section>
  </ZPopover>
</template>
