<script setup lang="ts">
import { Minimize2 } from '@lucide/vue'
import { computed } from 'vue'

import { useI18n } from 'vue-i18n'

const props = defineProps<{ record: { route: string | null, compact?: boolean } }>()

const { t } = useI18n({ useScope: 'global' })
const routeLabel = computed(() => {
  const route = props.record.route
  return route === '/v1beta/models/{resource}'
    ? '/v1beta/models'
    : route || '—'
})
</script>

<template>
  <div class="inline-flex max-w-full items-center gap-1.5 whitespace-nowrap">
    <code class="min-w-0 truncate font-mono text-z-sm font-emphasis" :title="record.route ?? undefined">{{ routeLabel }}</code>
    <span
      v-if="record.compact"
      class="inline-flex shrink-0 text-z-orange-text"
      :title="t('usage.route.compressionRequest')"
      :aria-label="t('usage.route.compressionRequest')"
    >
      <Minimize2 class="size-3.5" stroke-width="2.4" />
    </span>
  </div>
</template>
