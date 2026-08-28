<script setup lang="ts">
import type { AccountRow } from '../../constants'
import type { AccountProfileStatisticsResponse } from '@/api'
import { UserRound } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'

import { formatCompactNumber, formatInteger } from '@/utils/number'
import AccountPlanBadge from '../AccountPlanBadge.vue'

const props = defineProps<{
  account: AccountRow
  profile: AccountProfileStatisticsResponse
}>()

const imageFailed = shallowRef(false)
const displayName = computed(() =>
  props.profile.displayName
  || props.profile.username
  || props.account.email
  || props.account.name
  || 'Codex 用户',
)
const username = computed(() => props.profile.username
  ? `@${props.profile.username.replace(/^@/, '')}`
  : null)
const accountIdentity = computed(() => props.account.email?.trim() || props.account.accountId?.trim())
const initial = computed(() => Array.from(displayName.value.trim())[0]?.toUpperCase() || null)
const metrics = computed(() => [
  {
    label: '累计 Token 数',
    value: formatMetric(props.profile.summary.totalTextTokens),
    title: formatExact(props.profile.summary.totalTextTokens),
  },
  {
    label: '峰值 Token 数',
    value: formatMetric(props.profile.summary.peakTokens),
    title: formatExact(props.profile.summary.peakTokens),
  },
  {
    label: '最长聊天时长',
    value: formatDuration(props.profile.summary.longestTaskDurationMs),
  },
  {
    label: '当前连续天数',
    value: formatDays(props.profile.summary.currentStreakDays),
  },
  {
    label: '最长连续天数',
    value: formatDays(props.profile.summary.longestStreakDays),
  },
])

watch(() => props.profile.imageUrl, () => {
  imageFailed.value = false
})

function formatMetric(value: number | null) {
  return value === null ? '—' : formatCompactNumber(value)
}

function formatExact(value: number | null) {
  return value === null ? undefined : `${formatInteger(value)} Tokens`
}

function formatDays(value: number | null) {
  return value === null ? '—' : `${formatInteger(value)} 天`
}

function formatDuration(value: number | null) {
  if (value === null)
    return '—'
  const totalMinutes = Math.floor(value / 60_000)
  const days = Math.floor(totalMinutes / (24 * 60))
  const hours = Math.floor(totalMinutes % (24 * 60) / 60)
  const minutes = totalMinutes % 60
  if (days > 0)
    return hours > 0 ? `${days} 天 ${hours} 小时` : `${days} 天`
  if (hours > 0)
    return minutes > 0 ? `${hours} 小时 ${minutes} 分` : `${hours} 小时`
  if (totalMinutes > 0)
    return `${totalMinutes} 分`
  return `${Math.max(0, Math.floor(value / 1000))} 秒`
}
</script>

<template>
  <section
    class="grid gap-4 rounded-cp-lg bg-cp-fill-alter p-4 sm:p-5 lg:grid-cols-[minmax(310px,1.05fr)_minmax(0,1.95fr)] lg:items-center lg:gap-4"
    aria-labelledby="profile-display-name"
  >
    <div class="flex min-w-0 items-center gap-3.5">
      <span
        class="grid size-14 shrink-0 place-items-center overflow-hidden rounded-full bg-cp-error-bg text-xl font-heavy text-cp-error-text"
      >
        <img
          v-if="profile.imageUrl && !imageFailed"
          :src="profile.imageUrl"
          :alt="`${displayName} 的头像`"
          class="size-full object-cover"
          referrerpolicy="no-referrer"
          @error="imageFailed = true"
        >
        <span v-else-if="initial" aria-hidden="true">{{ initial }}</span>
        <UserRound v-else class="size-6" aria-hidden="true" />
      </span>

      <div class="min-w-0 flex-1">
        <div class="flex min-w-0 items-center gap-2">
          <h3 id="profile-display-name" class="m-0 min-w-0 truncate text-cp-lg leading-tight font-heavy text-cp-text">
            {{ displayName }}
          </h3>
          <AccountPlanBadge v-if="account.planType" :plan-type="account.planType" size="sm" />
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

    <div class="min-w-0 overflow-x-auto py-1">
      <dl class="grid min-w-130 grid-cols-5">
        <div
          v-for="metric in metrics"
          :key="metric.label"
          class="min-w-0 px-2 text-center sm:px-3"
        >
          <dd
            class="m-0 font-mono text-cp-lg leading-tight font-heavy tabular-nums text-cp-text"
            :title="metric.title"
          >
            {{ metric.value }}
          </dd>
          <dt class="mt-1.5 text-cp-xs font-semibold text-cp-text-secondary">
            {{ metric.label }}
          </dt>
        </div>
      </dl>
    </div>
  </section>
</template>
