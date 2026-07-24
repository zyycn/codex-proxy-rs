<script setup lang="ts">
import type { getAccounts } from '@/api'
import { sumBy } from 'es-toolkit'
import { computed } from 'vue'

import AccountPlanBadge from './AccountPlanBadge.vue'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]
type AccountIdentity = Pick<AccountRow, 'id' | 'email' | 'planType'>
  & Partial<Pick<AccountRow, 'accountId'>>

const props = withDefaults(
  defineProps<{
    account: AccountIdentity
    size?: 'md' | 'lg'
    showPlan?: boolean
    showAvatar?: boolean
    titleMode?: 'local-part' | 'email'
    metaPosition?: 'title' | 'secondary'
  }>(),
  {
    size: 'md',
    showPlan: false,
    showAvatar: true,
    titleMode: 'local-part',
    metaPosition: 'title',
  },
)

const emailText = computed(() => {
  const email = props.account.email?.trim()
  if (email)
    return email
  if ('accountId' in props.account && typeof props.account.accountId === 'string')
    return props.account.accountId
  return String(props.account.id)
})

const displayTitle = computed(() =>
  props.titleMode === 'email' ? emailText.value : emailText.value.split('@')[0],
)

const secondaryText = computed(() =>
  props.titleMode === 'email' ? null : emailText.value,
)

const initial = computed(() => displayTitle.value.slice(0, 1).toUpperCase())

const avatarSizeClass = computed(() =>
  props.size === 'lg' ? 'size-10 text-[15px]' : 'size-9 text-[13px]',
)

const secondaryClass = computed(() =>
  props.size === 'lg'
    ? 'mt-1 text-[12px] text-(--cp-text-secondary)'
    : 'mt-0.5 font-mono text-[11px] text-(--cp-text-muted)',
)

const avatarClass = computed(() => {
  const palettes = [
    'bg-(--cp-info-bg) text-(--cp-info-text) shadow-[inset_0_0_0_1px_var(--cp-info-border)]',
    'bg-(--cp-success-bg) text-(--cp-success-text) shadow-[inset_0_0_0_1px_var(--cp-success-border)]',
    'bg-(--cp-normal-bg) text-(--cp-normal-text) shadow-[inset_0_0_0_1px_var(--cp-normal-border)]',
    'bg-(--cp-warning-bg) text-(--cp-warning-text) shadow-[inset_0_0_0_1px_var(--cp-warning-border)]',
  ]
  const key = String(props.account.id || props.account.email || displayTitle.value)
  const hash = sumBy([...key], char => char.charCodeAt(0))
  return palettes[hash % palettes.length]
})
</script>

<template>
  <div class="flex min-w-0 items-center gap-3">
    <span
      v-if="showAvatar"
      class="inline-flex shrink-0 items-center justify-center rounded-lg font-[820]"
      :class="[avatarSizeClass, avatarClass]"
    >
      {{ initial }}
    </span>
    <div class="min-w-0 flex-1">
      <div class="flex min-w-0 items-center gap-2">
        <span class="min-w-0 flex-1 truncate text-[14px] font-[760] text-(--cp-text-primary)">
          {{ displayTitle }}
        </span>
        <span
          v-if="metaPosition === 'title' && (showPlan || $slots.meta)"
          class="inline-flex shrink-0 items-center justify-end gap-1.5"
        >
          <slot name="meta" />
          <AccountPlanBadge v-if="showPlan" :plan-type="account.planType" size="sm" />
        </span>
      </div>
      <div
        v-if="metaPosition === 'secondary' && (showPlan || $slots.meta)"
        class="mt-1 inline-flex min-w-0 items-center gap-1.5"
      >
        <slot name="meta" />
        <AccountPlanBadge v-if="showPlan" :plan-type="account.planType" size="sm" />
      </div>
      <div v-else-if="secondaryText" class="truncate font-[650]" :class="secondaryClass">
        {{ secondaryText }}
      </div>
    </div>
  </div>
</template>
