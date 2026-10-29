<script setup lang="ts">
import type { AccountUpstream } from '@/api'
import { Link2 } from '@lucide/vue'
import { useI18n } from 'vue-i18n'

defineProps<{ upstream: AccountUpstream }>()
const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <span
    class="inline-flex h-5.5 max-w-full items-center gap-1 rounded-full px-2 text-cp-xs leading-none font-heavy"
    :class="upstream.stale || upstream.lastError ? 'bg-cp-warning-container text-cp-warning-on-container' : 'bg-cp-cyan-container text-cp-cyan-on-container'"
    :aria-label="t('accounts.upstream.link')"
  >
    <Link2 class="size-3 shrink-0" aria-hidden="true" />
    <span>CPR</span>
  </span>
</template>
