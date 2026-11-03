<script setup lang="ts">
import type { AccountUpstream } from '@/api'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { accountUpstreamErrorMessage } from '@/utils/account-upstream'
import { formatRelativeTime } from '@/utils/relative-time'

const props = defineProps<{ upstream: AccountUpstream }>()
const { t } = useI18n({ useScope: 'global' })
const status = computed(() => {
  const upstream = props.upstream
  if (upstream.lastError)
    return accountUpstreamErrorMessage(upstream.lastError)
  if (!upstream.observedAt)
    return ''
  return t('accounts.upstream.observedAt', {
    time: formatRelativeTime(upstream.observedAtRelative),
  })
})
const detail = computed(() => [
  props.upstream.observedAtDisplay,
  props.upstream.stale
    ? t('accounts.upstream.stale')
    : null,
  accountUpstreamErrorMessage(props.upstream.lastError),
].filter(Boolean).join(' · '))
</script>

<template>
  <span
    class="min-w-0 truncate text-z-xs font-emphasis"
    :class="upstream.stale || upstream.lastError ? 'text-z-warning-text' : 'text-z-text-tertiary'"
    :title="detail"
    role="status"
  >
    {{ status }}
  </span>
</template>
