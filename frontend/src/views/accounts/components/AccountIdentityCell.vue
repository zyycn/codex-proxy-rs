<script setup lang="ts">
import type { Account } from '@/api'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

import AccountPlanBadge from '@/components/account-plan-badge/index.vue'
import { stablePresetVisualToneClass } from '@/utils/color'
import { accountIdentityLabel } from '../utils/accountIdentity'
import AccountSecondaryText from './AccountSecondaryText.vue'

type AccountIdentity = Pick<Account, 'id' | 'email' | 'planType' | 'planTypeDisplay'>
  & Partial<Pick<Account, 'accountId' | 'notes' | 'name' | 'authenticationKind' | 'upstream'>>

const props = withDefaults(
  defineProps<{
    account: AccountIdentity
    size?: 'md' | 'lg'
    showPlan?: boolean
    showNotes?: boolean
    metaPosition?: 'title' | 'secondary'
    metaSize?: 'xs' | 'sm'
  }>(),
  {
    size: 'md',
    showPlan: false,
    showNotes: false,
    metaPosition: 'title',
    metaSize: 'sm',
  },
)

const { t } = useI18n({ useScope: 'global' })
const emailText = computed(() => props.account.email?.trim())
const displayTitle = computed(() => {
  const label = accountIdentityLabel(props.account)
  return props.account.authenticationKind !== 'api_key' && label === emailText.value
    ? label.split('@')[0]
    : label
})

const visibleNotes = computed(() => props.showNotes ? props.account.notes : undefined)

const identityTitle = computed(() => {
  const email = emailText.value
  return email && email !== displayTitle.value
    ? `${displayTitle.value}\n${email}`
    : displayTitle.value
})

const secondaryText = computed(() => {
  if (visibleNotes.value)
    return visibleNotes.value
  if (props.account.authenticationKind !== 'api_key' && emailText.value !== displayTitle.value)
    return emailText.value
  return undefined
})

const secondaryClass = computed(() => props.size === 'lg' && !visibleNotes.value
  ? 'mt-1 text-z-sm text-z-text-secondary'
  : 'mt-0.5 font-mono text-z-xs leading-4 text-z-text-quaternary')

const initial = computed(() => displayTitle.value.slice(0, 1).toUpperCase())

const avatarSizeClass = computed(() =>
  props.size === 'lg'
    ? 'size-10 text-z-xl'
    : 'size-9 text-z',
)

const metaGapClass = computed(() => props.metaSize === 'xs'
  ? 'gap-1'
  : 'gap-1.5')

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
        <span class="min-w-0 truncate text-z font-heavy text-z-text" :title="identityTitle">
          {{ displayTitle }}
        </span>
        <span
          v-if="metaPosition === 'title' && (showPlan || $slots.meta)"
          class="inline-flex shrink-0 items-center justify-end"
          :class="metaGapClass"
        >
          <slot name="meta" />
          <AccountPlanBadge v-if="showPlan" :upstream="account.upstream" :authentication-kind="account.authenticationKind" :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" :size="metaSize" />
        </span>
      </div>
      <div
        v-if="metaPosition === 'secondary' && (showPlan || $slots.meta)"
        class="mt-0.5 inline-flex min-w-0 items-center"
        :class="metaGapClass"
      >
        <slot name="meta" />
        <AccountPlanBadge v-if="showPlan" :upstream="account.upstream" :authentication-kind="account.authenticationKind" :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" :size="metaSize" />
      </div>
      <AccountSecondaryText
        v-else-if="secondaryText"
        :text="secondaryText"
        :label="visibleNotes ? t('accounts.identity.notes') : t('accounts.identity.email')"
        :class="secondaryClass"
      />
    </div>
  </div>
</template>
