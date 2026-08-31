<script setup lang="ts">
import type { getAccounts } from '@/api'
import { computed } from 'vue'

import { stablePresetVisualToneClass } from '../utils/visualTone'
import AccountPlanBadge from './AccountPlanBadge.vue'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]
type AccountIdentity = Pick<AccountRow, 'id' | 'email' | 'planType'>
  & Partial<Pick<AccountRow, 'accountId'>>

const props = withDefaults(
  defineProps<{
    account: AccountIdentity
    size?: 'md' | 'lg'
    showPlan?: boolean
    titleMode?: 'local-part' | 'email'
    metaPosition?: 'title' | 'secondary'
    metaSize?: 'xs' | 'sm'
  }>(),
  {
    size: 'md',
    showPlan: false,
    titleMode: 'local-part',
    metaPosition: 'title',
    metaSize: 'sm',
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
  props.size === 'lg' ? 'size-10 text-cp-xl' : 'size-9 text-cp',
)

const secondaryClass = computed(() =>
  props.size === 'lg'
    ? 'mt-1 text-cp-sm text-cp-text-secondary'
    : 'mt-0.5 font-mono text-cp-xs text-cp-text-quaternary',
)

const metaGapClass = computed(() => props.metaSize === 'xs' ? 'gap-1' : 'gap-1.5')

const avatarToneClass = computed(() => {
  const identity = props.account.id || props.account.email || displayTitle.value
  return stablePresetVisualToneClass(identity)
})
</script>

<template>
  <div class="flex min-w-0 items-center gap-3">
    <span
      class="inline-flex shrink-0 items-center justify-center rounded-lg font-extrabold"
      :class="[avatarSizeClass, avatarToneClass]"
    >
      {{ initial }}
    </span>
    <div class="min-w-0 flex-1">
      <div class="flex min-w-0 items-center gap-2">
        <span class="min-w-0 flex-1 truncate text-cp font-heavy text-cp-text">
          {{ displayTitle }}
        </span>
        <span
          v-if="metaPosition === 'title' && (showPlan || $slots.meta)"
          class="inline-flex shrink-0 items-center justify-end"
          :class="metaGapClass"
        >
          <slot name="meta" />
          <AccountPlanBadge v-if="showPlan" :plan-type="account.planType" :size="metaSize" />
        </span>
      </div>
      <div
        v-if="metaPosition === 'secondary' && (showPlan || $slots.meta)"
        class="mt-0.5 inline-flex min-w-0 items-center"
        :class="metaGapClass"
      >
        <slot name="meta" />
        <AccountPlanBadge v-if="showPlan" :plan-type="account.planType" :size="metaSize" />
      </div>
      <div v-else-if="secondaryText" class="truncate font-emphasis" :class="secondaryClass">
        {{ secondaryText }}
      </div>
    </div>
  </div>
</template>
