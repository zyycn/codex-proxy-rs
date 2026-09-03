<script setup lang="ts">
import type { dashboardSnapshotView } from '../composables/useDashboard'
import { Key, LinkAlt, Openai, Xai } from '@boxicons/vue'
import { computed } from 'vue'

import { formatProviderLabel } from '@/utils/providers'
import AccountPlanBadge from '@/views/accounts/components/AccountPlanBadge.vue'
import { stablePresetVisualToneClass } from '@/views/accounts/utils/visualTone'

type DashboardSnapshot = ReturnType<typeof dashboardSnapshotView>
type DashboardAccount = DashboardSnapshot['accountUsage'][number]

const props = defineProps<{
  account: DashboardAccount
}>()

const email = computed(() => props.account.email?.trim() || String(props.account.id))
const displayTitle = computed(() => email.value.split('@')[0] || email.value)
const normalizedProvider = computed(() => props.account.provider?.trim().toLowerCase() || '')
const normalizedAuthenticationKind = computed(() =>
  props.account.authenticationKind?.trim().toLowerCase() || '',
)
const providerLabel = computed(() => formatProviderLabel(props.account.provider, '未知平台'))

const authenticationLabel = computed(() => {
  if (normalizedAuthenticationKind.value === 'oauth')
    return 'OAuth'
  if (normalizedAuthenticationKind.value === 'api_key')
    return 'API Key'
  return props.account.authenticationKind?.trim() || '未知认证类型'
})

const avatarToneClass = computed(() =>
  stablePresetVisualToneClass(props.account.id || props.account.email || email.value),
)
</script>

<template>
  <div class="flex min-w-0 items-center gap-3">
    <span class="relative inline-flex size-9 shrink-0">
      <span
        class="inline-flex size-9 items-center justify-center rounded-lg shadow-cp-tertiary"
        :class="avatarToneClass"
        :title="providerLabel"
      >
        <Openai v-if="normalizedProvider === 'openai'" class="size-3.5" />
        <Xai v-else-if="normalizedProvider === 'xai'" class="size-3.5" />
        <span v-else class="text-cp-xs font-heavy">?</span>
        <span class="sr-only">{{ providerLabel }}</span>
      </span>

      <span
        class="absolute -right-1 -bottom-1 inline-flex size-4 items-center justify-center rounded-[5px] bg-cp-bg-container text-cp-text shadow-cp-tertiary"
        :title="authenticationLabel"
      >
        <LinkAlt v-if="normalizedAuthenticationKind === 'oauth'" class="size-2.5" />
        <Key v-else-if="normalizedAuthenticationKind === 'api_key'" class="size-2.5" />
        <span v-else class="text-[8px] font-heavy text-cp-text-quaternary">?</span>
        <span class="sr-only">{{ authenticationLabel }}</span>
      </span>
    </span>

    <span class="min-w-0 flex-1">
      <span class="flex min-w-0 items-center gap-1.5">
        <strong class="min-w-0 truncate text-cp leading-[1.15] font-heavy text-cp-text">
          {{ displayTitle }}
        </strong>
        <AccountPlanBadge :plan-type="account.planType" size="xs" />
      </span>
      <span
        class="mt-0.5 block min-w-0 truncate font-mono text-cp-xs font-emphasis text-cp-text-quaternary"
        :title="email"
      >
        {{ email }}
      </span>
    </span>
  </div>
</template>
