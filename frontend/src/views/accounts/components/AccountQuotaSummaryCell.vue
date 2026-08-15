<script setup lang="ts">
import type { AccountRow } from '../constants'

import { computed } from 'vue'
import { visibleSummaryQuotaWindows } from '../constants'
import AccountUsageWindow from './AccountUsageWindow.vue'

const props = defineProps<{
  account: AccountRow
}>()

const quotaWindows = computed(() => props.account.quota.windows)
const visibleQuotaWindows = computed(() => visibleSummaryQuotaWindows(quotaWindows.value))
const summaryClass = computed(
  () =>
    `grid w-full min-w-0 gap-1.5 py-0.5 ${
      visibleQuotaWindows.value.length <= 1
        ? 'min-h-13 content-center'
        : 'min-h-16.5 content-center'
    }`,
)
</script>

<template>
  <div :class="summaryClass">
    <AccountUsageWindow
      v-for="window in visibleQuotaWindows"
      :key="window.key"
      :window="window"
      variant="compact"
    />
    <AccountUsageWindow v-if="quotaWindows.length === 0" variant="compact" />
  </div>
</template>
