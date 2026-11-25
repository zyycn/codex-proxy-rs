<script setup lang="ts">
import { ZButton, ZCard, ZIconButton, ZMotionIcon } from '@codex-proxy/ui'

import { Copy, KeyRound, Trash2 } from '@lucide/vue'

import { useI18n } from 'vue-i18n'

defineProps<{
  status: AdminApiKeyStatus
  loading: boolean
  regenerating: boolean
  deleting: boolean
  generatedKey: string
}>()

const emit = defineEmits<{
  regenerate: []
  requestDelete: []
  copy: []
}>()

const { t } = useI18n({ useScope: 'global' })

interface AdminApiKeyStatus {
  exists: boolean
}
</script>

<template>
  <ZCard :title="t('settings.adminApiKey.adminApiKey')" :description="t('settings.adminApiKey.description')">
    <template #actions>
      <div class="flex flex-wrap items-center gap-2">
        <ZButton
          :loading="regenerating"
          :disabled="loading || deleting"
          @click="emit('regenerate')"
        >
          <template #icon>
            <KeyRound class="size-4" />
          </template>
          {{ status.exists ? t('settings.adminApiKey.regenerate') : t('settings.adminApiKey.generate') }}
        </ZButton>
        <ZButton
          type="danger" variant="plain"
          :disabled="loading || regenerating || !status.exists"
          @click="emit('requestDelete')"
        >
          <template #icon>
            <Trash2 class="size-4" />
          </template>
          {{ t('common.actions.delete') }}
        </ZButton>
      </div>
    </template>

    <div class="grid gap-4">
      <div class="flex min-h-16 items-center justify-between gap-4 rounded-z bg-z-fill-quaternary px-4 py-3">
        <div class="flex min-w-0 items-center gap-3">
          <ZMotionIcon
            class="inline-flex size-9 shrink-0 items-center justify-center rounded-z bg-z-bg-container text-z-cyan-solid shadow-z-tertiary"
          >
            <KeyRound class="size-4" />
          </ZMotionIcon>
          <div class="min-w-0">
            <p class="m-0 text-z leading-[1.15] font-bold text-z-text">
              {{ status.exists ? t('common.status.enabled') : t('settings.adminApiKey.notGenerated') }}
            </p>
            <p class="mt-1.5 mb-0 truncate text-z-sm leading-[1.15] font-emphasis text-z-text-secondary">
              {{
                loading
                  ? t('common.status.loading')
                  : status.exists
                    ? t('settings.adminApiKey.enabledDescription')
                    : t('settings.adminApiKey.disabledDescription')
              }}
            </p>
          </div>
        </div>
      </div>

      <div v-if="generatedKey" class="grid gap-2">
        <p class="m-0 text-z leading-[1.15] font-emphasis text-z-text-secondary">
          {{ t('settings.adminApiKey.generatedNotice') }}
        </p>
        <div class="flex min-w-0 items-center gap-2">
          <code
            class="min-w-0 flex-1 rounded-z bg-z-fill-quaternary px-3 py-2.5 font-mono text-z-sm leading-normal font-emphasis break-all text-z-text"
          >
            {{ generatedKey }}
          </code>
          <ZIconButton size="default" :aria-label="t('common.actions.copy')" @click="emit('copy')">
            <Copy class="size-4" />
          </ZIconButton>
        </div>
      </div>
    </div>
  </ZCard>
</template>
