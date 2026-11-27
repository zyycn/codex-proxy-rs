<script setup lang="ts">
import { ZCard, ZFormItem, ZInput } from '@codex-proxy/ui'

import { useI18n } from 'vue-i18n'

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<string>({ required: true })
</script>

<template>
  <ZCard :title="t('settings.responseLimit.requestBodyLimit')" :description="t('settings.responseLimit.description')">
    <div class="grid min-w-0 gap-4.5">
      <ZFormItem :show-message="Boolean(model.trim())" prop="responsesMaxDecompressedBodyMiB" :label="t('settings.responseLimit.maximumSize')" :description="t('settings.responseLimit.sizeHelp')">
        <ZInput v-model="model" :aria-label="t('settings.responseLimit.maxSizeLabel')" type="number" min="1" step="1">
          <template #suffix>
            MiB
          </template>
        </ZInput>
      </ZFormItem>
    </div>
  </ZCard>
</template>
