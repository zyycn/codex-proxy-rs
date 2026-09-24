<script setup lang="ts">
import type { dashboardSnapshotView } from '../composables/useDashboard'
import { computed } from 'vue'

import { authenticationIcon, formatAuthenticationLabel, formatProviderLabel, providerIcon } from '@/utils/providers'
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
const providerIconComponent = computed(() => normalizedProvider.value ? providerIcon(normalizedProvider.value) : undefined)
const authenticationIconComponent = computed(() => authenticationIcon(props.account.authenticationKind))
const providerLabel = computed(() => formatProviderLabel(props.account.provider, '未知平台'))

const authenticationLabel = computed(() => formatAuthenticationLabel(props.account.authenticationKind))

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
        <component :is="providerIconComponent" v-if="providerIconComponent" class="size-3.5" />
        <span v-else class="text-cp-xs font-heavy">?</span>
        <span class="sr-only">{{ providerLabel }}</span>
      </span>

      <span
        class="absolute -right-1 -bottom-1 inline-flex size-4 items-center justify-center rounded-[5px] bg-cp-bg-container text-cp-text shadow-cp-tertiary"
        :title="authenticationLabel"
      >
        <component :is="authenticationIconComponent" v-if="authenticationIconComponent" class="size-2.5" />
        <span v-else class="text-[8px] font-heavy text-cp-text-quaternary">?</span>
        <span class="sr-only">{{ authenticationLabel }}</span>
      </span>
    </span>

    <span class="min-w-0 flex-1">
      <span class="flex min-w-0 items-center gap-1.5">
        <strong class="min-w-0 truncate text-cp leading-[1.15] font-heavy text-cp-text">
          {{ displayTitle }}
        </strong>
        <AccountPlanBadge :authentication-kind="account.authenticationKind" :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" size="xs" />
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
