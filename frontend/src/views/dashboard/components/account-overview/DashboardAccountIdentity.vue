<script setup lang="ts">
import type { DashboardAccountUsage } from '@/api'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

import AccountPlanBadge from '@/components/account-plan-badge/index.vue'
import { authenticationIcon, formatAuthenticationLabel, formatProviderLabel, providerIcon } from '@/providers/catalog'
import { stablePresetVisualToneClass } from '@/utils/color'

const props = defineProps<{
  account: DashboardAccountUsage
}>()

const { t } = useI18n({ useScope: 'global' })

const email = computed(() => props.account.email?.trim() || String(props.account.id))
const displayTitle = computed(() => email.value.split('@')[0] || email.value)
const secondaryText = computed(() => props.account.authenticationKind === 'api_key'
  ? props.account.notes
  : email.value)
const normalizedProvider = computed(() => props.account.provider?.trim().toLowerCase() || '')
const providerIconComponent = computed(() => normalizedProvider.value
  ? providerIcon(normalizedProvider.value)
  : undefined)
const authenticationIconComponent = computed(() => authenticationIcon(props.account.authenticationKind))
const providerLabel = computed(() => formatProviderLabel(props.account.provider, t('common.providers.unknown')))

const authenticationLabel = computed(() => formatAuthenticationLabel(props.account.authenticationKind))

const avatarToneClass = computed(() =>
  stablePresetVisualToneClass(props.account.id || props.account.email || email.value),
)
</script>

<template>
  <div class="flex min-w-0 items-center gap-3">
    <span class="relative inline-flex size-9 shrink-0">
      <span
        class="inline-flex size-9 items-center justify-center rounded-lg shadow-z-tertiary"
        :class="avatarToneClass"
        :title="providerLabel"
      >
        <component :is="providerIconComponent" v-if="providerIconComponent" class="size-3.5" />
        <span v-else class="text-z-xs font-heavy">?</span>
        <span class="sr-only">{{ providerLabel }}</span>
      </span>

      <span
        class="absolute -right-1 -bottom-1 inline-flex size-4 items-center justify-center rounded-[5px] bg-z-bg-container text-z-text shadow-z-tertiary"
        :title="authenticationLabel"
      >
        <component :is="authenticationIconComponent" v-if="authenticationIconComponent" class="size-2.5" />
        <span v-else class="text-[8px] font-heavy text-z-text-quaternary">?</span>
        <span class="sr-only">{{ authenticationLabel }}</span>
      </span>
    </span>

    <span class="min-w-0 flex-1">
      <span class="flex min-w-0 items-center gap-1.5">
        <strong class="min-w-0 truncate text-z leading-[1.15] font-heavy text-z-text">
          {{ displayTitle }}
        </strong>
        <AccountPlanBadge :upstream="account.upstream" :authentication-kind="account.authenticationKind" :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" size="xs" />
      </span>
      <span
        v-if="secondaryText"
        class="mt-0.5 block min-w-0 truncate font-mono text-z-xs font-emphasis text-z-text-quaternary"
        :title="secondaryText"
      >
        {{ secondaryText }}
      </span>
    </span>
  </div>
</template>
