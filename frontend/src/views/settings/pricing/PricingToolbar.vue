<script setup lang="ts">
import { ZButton, ZIconButton, ZInput, ZPopover, ZSegmented, ZSelect } from '@codex-proxy/ui'

import { CircleAlert, Download, Percent, Plus, RotateCcw, Search, X } from '@lucide/vue'
import { computed, shallowRef, useId } from 'vue'
import { useI18n } from 'vue-i18n'
import { formatProviderLabel, providerIcon } from '@/utils/providers'
import { sourceLabels } from './model'

const props = defineProps<{
  selectedCount: number
  disabled: boolean
  saving: boolean
  syncing: boolean
  syncedAtDisplay?: string | null
  providers: string[]
}>()

defineEmits<{ add: [], sync: [], setMultiplier: [], reset: [], clear: [] }>()

const { t } = useI18n({ useScope: 'global' })

const search = defineModel<string>('search', { required: true })
const provider = defineModel<string>('provider', { required: true })
const source = defineModel<string>('source', { required: true })
const syncInfoOpen = shallowRef(false)
const syncInfoId = useId()
const syncTime = computed(() => props.syncedAtDisplay ?? t('settings.pricing.notSyncedYet'))
const providerOptions = computed(() => props.providers.map(value => ({
  label: formatProviderLabel(value),
  value,
  icon: providerIcon(value),
})))
</script>

<template>
  <div class="mb-3 shrink-0">
    <div class="flex flex-wrap items-center gap-2">
      <ZInput v-model="search" class="w-full sm:w-72" :placeholder="t('settings.pricing.searchModel')" :aria-label="t('settings.pricing.searchModelId')">
        <template #prefix>
          <Search class="size-4" />
        </template>
      </ZInput>
      <div v-if="providerOptions.length" class="min-w-0 max-w-full overflow-x-auto">
        <ZSegmented
          v-model="provider"
          :aria-label="t('settings.pricing.modelProvider')"
          display="icon"
          :style="{ width: `${providerOptions.length * 40 + 4}px` }"
          :options="providerOptions"
          :disabled="saving"
        />
      </div>
      <ZSelect v-model="source" class="w-32" :aria-label="t('settings.pricing.priceSource')" :options="[{ label: t('settings.pricing.allSources'), value: 'all' }, { label: sourceLabels.custom, value: 'custom' }, { label: sourceLabels.synced, value: 'synced' }, { label: sourceLabels.builtin, value: 'builtin' }]" />
      <div v-if="selectedCount" class="ml-auto flex w-full flex-wrap items-center justify-end gap-2 sm:w-auto" role="group" :aria-label="t('settings.pricing.batchOperation')">
        <span class="inline-flex h-z-control w-full items-center gap-1 whitespace-nowrap text-z text-z-text-secondary sm:mr-1 sm:w-auto" aria-live="polite">
          {{ t('settings.pricing.selectedCountLabel') }} <span class="font-mono tabular-nums">{{ selectedCount }}</span> {{ t('settings.pricing.item') }}
        </span>
        <ZButton :disabled="disabled || selectedCount > 500" @click="$emit('setMultiplier')">
          <template #icon>
            <Percent class="size-4" />
          </template>
          {{ t('settings.pricing.setMultiplier') }}
        </ZButton>
        <ZButton :disabled="disabled || selectedCount > 500" @click="$emit('reset')">
          <template #icon>
            <RotateCcw class="size-4" />
          </template>
          {{ t('settings.pricing.clearOverwrite') }}
        </ZButton>
        <ZIconButton :aria-label="t('settings.pricing.deselect')" variant="plain" :disabled="disabled" @click="$emit('clear')">
          <X class="size-4" />
        </ZIconButton>
      </div>
      <div v-else class="ml-auto flex items-center gap-2">
        <div class="flex items-center gap-1">
          <ZPopover v-model="syncInfoOpen" placement="bottom-end" trigger="hover-click">
            <template #reference>
              <ZIconButton variant="link" :aria-label="t('settings.pricing.synchronizationInstructions')" size="small" :aria-expanded="syncInfoOpen" :aria-controls="syncInfoOpen ? syncInfoId : undefined">
                <CircleAlert class="size-3.5" />
              </ZIconButton>
            </template>
            <div :id="syncInfoId" class="grid w-64 gap-2 p-3 text-z-xs leading-relaxed text-z-text-secondary">
              <p class="m-0">
                {{ t('settings.pricing.sourceHelp') }}
              </p>
              <p class="m-0 text-z-text-tertiary">
                {{ t('settings.pricing.lastSynced', { syncTime }) }}
              </p>
            </div>
          </ZPopover>
          <ZButton :loading="syncing" :disabled="disabled" :aria-label="t('settings.pricing.syncPriceList')" @click="$emit('sync')">
            <template #icon>
              <Download class="size-4" />
            </template>
            {{ t('settings.pricing.sync.action') }}
          </ZButton>
        </div>
        <ZButton type="primary" :disabled="disabled" @click="$emit('add')">
          <template #icon>
            <Plus class="size-4" />
          </template>
          {{ t('settings.pricing.addModel') }}
        </ZButton>
      </div>
    </div>
  </div>
</template>
