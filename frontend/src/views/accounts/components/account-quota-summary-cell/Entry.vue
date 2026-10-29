<script setup lang="ts">
import type { AccountQuotaWindow } from '@/api'

import { ZPopover } from '@codex-proxy/ui'

import { computed } from 'vue'

import { useI18n } from 'vue-i18n'
import AccountUsageWindow from '@/components/account/account-usage-window/index.vue'
import { quotaWindowLabel } from '@/components/account/account-usage-window/presenter'
import AccountQuotaWindowDetails from '@/components/account/AccountQuotaWindowDetails.vue'
import AccountQuotaWindowGroup from './WindowGroup.vue'

const props = withDefaults(defineProps<{
  label: string | null
  windows: AccountQuotaWindow[]
  showPercentage?: boolean
}>(), {
  showPercentage: true,
})

const { t } = useI18n({ useScope: 'global' })

const detailHeading = computed(() => props.label)
const detailTitle = computed(() => detailHeading.value ?? (props.windows[0]
  ? quotaWindowLabel(props.windows[0])
  : t('accounts.quota.quotaDetails')))
const summaryLabel = computed(() => props.label ?? (props.windows[0]
  ? quotaWindowLabel(props.windows[0])
  : t('usage.quota')))
const hasQuotaWindow = computed(() => props.windows.some(window => typeof window.usedPercent === 'number' || window.amount))
</script>

<template>
  <ZPopover class="grid! w-full grid-cols-subgrid" trigger="hover-click" placement="right" :hover-delay="240">
    <template #reference="{ open }">
      <button
        type="button"
        class="col-span-full grid w-full min-w-0 grid-cols-subgrid cursor-pointer rounded-sm border-0 bg-transparent p-0 text-left outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
        :aria-label="t('accounts.quota.viewDetails', { detailTitle })"
        :aria-expanded="open"
        aria-haspopup="dialog"
      >
        <AccountQuotaWindowGroup
          v-if="hasQuotaWindow"
          :label="summaryLabel"
          :windows="windows"
          :show-percentage="showPercentage"
        />
        <AccountUsageWindow
          v-else
          class="col-span-full grid! grid-cols-subgrid"
          :window="windows[0]"
          variant="compact"
          :show-local-value="false"
          :show-percentage="false"
          :show-native-tooltip="false"
        />
      </button>
    </template>

    <AccountQuotaWindowDetails :label="detailHeading" :windows="windows" />
  </ZPopover>
</template>
