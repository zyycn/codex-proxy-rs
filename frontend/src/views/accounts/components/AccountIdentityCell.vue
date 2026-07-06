<script setup lang="ts">
import { sumBy } from 'es-toolkit'
import { computed } from 'vue'

import AccountPlanBadge from './AccountPlanBadge.vue'

type AccountIdentity = {
  id?: string | number | null
  email: string
  planType?: string | null
}

const props = withDefaults(
  defineProps<{
    account: AccountIdentity
    size?: 'md' | 'lg'
    showPlan?: boolean
  }>(),
  {
    size: 'md',
    showPlan: false,
  },
)

const emailText = computed(() => props.account.email.trim())

const displayTitle = computed(() => emailText.value.split('@')[0])

const secondaryText = emailText

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
  const hash = sumBy([...key], (char) => char.charCodeAt(0))
  return palettes[hash % palettes.length]
})
</script>

<template>
  <div class="flex min-w-0 items-center gap-3">
    <span
      class="inline-flex shrink-0 items-center justify-center rounded-lg font-[820]"
      :class="[avatarSizeClass, avatarClass]"
    >
      {{ initial }}
    </span>
    <div class="min-w-0">
      <div class="flex min-w-0 items-center gap-2">
        <span class="truncate text-[14px] font-[760] text-(--cp-text-primary)">
          {{ displayTitle }}
        </span>
        <AccountPlanBadge v-if="showPlan" :plan-type="account.planType" size="sm" />
      </div>
      <div class="truncate font-[650]" :class="secondaryClass">
        {{ secondaryText }}
      </div>
    </div>
  </div>
</template>
