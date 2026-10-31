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
  const observed = t('accounts.upstream.observedAt', {
    time: formatRelativeTime(upstream.observedAtRelative),
  })
  return upstream.stale
    ? `${observed} · ${t('accounts.upstream.stale')}`
    : observed
})
const detail = computed(() => [
  props.upstream.observedAtDisplay,
  accountUpstreamErrorMessage(props.upstream.lastError),
].filter(Boolean).join(' · '))
</script>

<template>
  <span
    class="min-w-0 truncate text-cp-xs font-emphasis"
    :class="upstream.stale || upstream.lastError ? 'text-cp-warning-text' : 'text-cp-text-tertiary'"
    :title="detail"
    role="status"
  >
    {{ status }}
  </span>
</template>
