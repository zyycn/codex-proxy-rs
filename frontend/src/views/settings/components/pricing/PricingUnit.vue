<script setup lang="ts">
import { ZIconButton, ZPopover } from '@codex-proxy/ui'

import { Info } from '@lucide/vue'
import { shallowRef, useId } from 'vue'
import { useI18n } from 'vue-i18n'

const { t } = useI18n({ useScope: 'global' })

const infoOpen = shallowRef(false)
const infoId = useId()
</script>

<template>
  <ZPopover v-model="infoOpen" placement="bottom-start" trigger="hover-click">
    <template #reference>
      <ZIconButton
        variant="link"
        size="small"
        :aria-label="t('settings.pricing.unitHelp')"
        :aria-expanded="infoOpen"
        :aria-controls="infoOpen ? infoId : undefined"
      >
        <Info class="size-3.5" aria-hidden="true" />
      </ZIconButton>
    </template>
    <div :id="infoId" class="grid w-72 gap-2 p-3 text-z-xs leading-relaxed text-z-text-secondary">
      <p class="m-0 font-emphasis text-z-text">
        {{ t('settings.pricing.unitPriceUnitUsdMTokens') }}
      </p>
      <p class="m-0">
        {{ t('settings.pricing.priorityHelp') }}
      </p>
      <p class="m-0">
        {{ t('settings.pricing.tableHelp') }}
      </p>
    </div>
  </ZPopover>
</template>
