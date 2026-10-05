<script setup lang="ts">
import type { Account, AccountProfileStatisticsResponse } from '@/api'

import { UserRound } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'

import { accountProfileAvatarUrl } from '@/api'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import { stablePresetVisualToneClass } from '@/utils/color'

const props = defineProps<{
  account: Account
  profile: AccountProfileStatisticsResponse | null
}>()

const imageFailed = shallowRef(false)
const displayName = computed(
  () => props.profile?.displayName || props.profile?.username || props.account.email || props.account.name || '账号用户',
)
const username = computed(() => (props.profile?.username ? `@${props.profile?.username.replace(/^@/, '')}` : null))
const accountIdentity = computed(() => props.account.email?.trim() || props.account.accountId?.trim())
const initial = computed(() => Array.from(displayName.value.trim())[0]?.toUpperCase() || null)
const avatarToneClass = computed(() =>
  stablePresetVisualToneClass(props.account.id || accountIdentity.value || displayName.value),
)
const avatarUrl = computed(() => {
  const sourceUrl = props.profile?.imageUrl?.trim()
  return props.account.capabilities.avatar && sourceUrl ? accountProfileAvatarUrl(props.account.id, sourceUrl) : null
})
watch(
  avatarUrl,
  () => {
    imageFailed.value = false
  },
)
</script>

<template>
  <section
    class="flex min-w-0 items-center"
    aria-labelledby="profile-display-name"
  >
    <div class="flex min-w-0 items-center gap-3.5">
      <span
        class="grid size-14 shrink-0 place-items-center overflow-hidden rounded-full text-xl font-heavy"
        :class="avatarToneClass"
      >
        <img
          v-if="avatarUrl && !imageFailed"
          :src="avatarUrl"
          :alt="`${displayName} 的头像`"
          class="size-full object-cover"
          referrerpolicy="no-referrer"
          @error="imageFailed = true"
        >
        <span v-else-if="initial">{{ initial }}</span>
        <UserRound v-else class="size-6" />
      </span>

      <div class="min-w-0 flex-1">
        <div class="flex min-w-0 items-center gap-2">
          <h3 id="profile-display-name" class="m-0 min-w-0 truncate text-cp-lg leading-tight font-heavy text-cp-text">
            {{ displayName }}
          </h3>
          <AccountPlanBadge v-if="account.planType" :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" size="sm" />
        </div>
        <div
          v-if="username || accountIdentity"
          class="mt-1.5 grid min-w-0 gap-0.5 text-cp-xs font-semibold text-cp-text-secondary"
        >
          <span v-if="username" class="truncate">{{ username }}</span>
          <span v-if="accountIdentity" class="truncate" :title="accountIdentity">{{ accountIdentity }}</span>
        </div>
      </div>
    </div>
  </section>
</template>
