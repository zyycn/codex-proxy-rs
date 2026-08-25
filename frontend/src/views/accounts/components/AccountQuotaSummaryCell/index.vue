<script setup lang="ts">
import type { AccountRow } from '../../constants'

import { computed } from 'vue'
import { groupedAccountQuotaWindows, visibleSummaryQuotaWindows } from '../../constants'
import AccountUsageWindow from '../AccountUsageWindow/index.vue'
import AccountQuotaSummaryEntry from './Entry.vue'

const props = defineProps<{
  account: AccountRow
}>()

const quotaWindows = computed(() => props.account.quota.windows)
const visibleQuotaWindows = computed(() => visibleSummaryQuotaWindows(quotaWindows.value))
const summaryEntries = computed(() => groupedAccountQuotaWindows(visibleQuotaWindows.value))
const summaryClass = computed(
  () =>
    `box-border grid w-full min-w-0 gap-1 py-1.5 ${
      summaryEntries.value.length <= 1
        ? 'min-h-13 content-center'
        : 'min-h-16.5 content-center'
    }`,
)
</script>

<template>
  <div :class="summaryClass">
    <AccountQuotaSummaryEntry
      v-for="entry in summaryEntries"
      :key="entry.key"
      :label="entry.label"
      :windows="entry.windows"
    />
    <AccountUsageWindow v-if="summaryEntries.length === 0" variant="compact" />
  </div>
</template>
