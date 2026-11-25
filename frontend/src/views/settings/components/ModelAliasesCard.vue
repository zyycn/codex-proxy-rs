<script setup lang="ts">
import type { FormRule } from '@codex-proxy/ui'
import { ZButton, ZCard, ZEmpty, ZFormItem, ZIconButton, ZInput } from '@codex-proxy/ui'

import { Plus, Trash2 } from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = withDefaults(defineProps<{
  mappings: Array<{ requestedModel: string, upstreamModel: string }>
  loading?: boolean
}>(), {
  loading: false,
})

const emit = defineEmits<{
  addMapping: []
  updateMapping: [index: number, key: 'requestedModel' | 'upstreamModel', value: string]
  removeMapping: [index: number]
}>()

const { t } = useI18n({ useScope: 'global' })
const requiredRule = computed<FormRule>(() => ({
  required: true,
  whitespace: true,
  trigger: ['blur', 'change'],
  message: t('settings.form.fillInModelMappingCompletely'),
}))
const uniqueModelRule = computed<FormRule>(() => ({
  trigger: ['blur', 'change'],
  validator: (_rule, value: string) => {
    const requested = value.trim()
    return !requested || props.mappings.filter(row => row.requestedModel.trim() === requested).length < 2
      || new Error(t('settings.form.duplicateClientModel', { requested }))
  },
}))
</script>

<template>
  <ZCard
    class="@container"
    :title="t('settings.modelMapping')"
    :description="t('settings.routing.modelMapHelp')"
  >
    <template #actions>
      <ZButton :disabled="loading" @click="emit('addMapping')">
        <template #icon>
          <Plus class="size-4" />
        </template>
        {{ t('settings.addMapping') }}
      </ZButton>
    </template>
    <template #body>
      <p
        v-if="loading"
        role="status"
        class="m-0 px-4 py-4 text-center text-z-sm text-z-text-tertiary"
      >
        {{ t('settings.loadingModelMapping') }}
      </p>
      <ZEmpty v-else-if="!mappings.length" :title="t('settings.emptyModelMapping')" size="small" surface="none" role="status" class="flex-1 content-center py-3!" />
      <div v-else class="grid gap-3">
        <div
          v-for="(row, index) in mappings"
          :key="index"
          class="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-4.5 rounded-z-card bg-z-fill-quaternary p-3 @lg:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)_auto]"
        >
          <ZFormItem :prop="`modelMappings.${index}.requestedModel`" :rules="[requiredRule, uniqueModelRule]" :show-message="Boolean(row.requestedModel.trim())">
            <ZInput
              :model-value="row.requestedModel"
              :placeholder="t('settings.requestModel')"
              :aria-label="t('settings.requestModel')"
              @update:model-value="emit('updateMapping', index, 'requestedModel', $event)"
            />
          </ZFormItem>
          <span class="hidden text-z-text-quaternary @lg:block" aria-hidden="true">→</span>
          <ZFormItem :prop="`modelMappings.${index}.upstreamModel`" :rules="requiredRule" :show-message="false" class="col-start-1 row-start-2 @lg:col-start-auto @lg:row-start-auto">
            <ZInput
              :model-value="row.upstreamModel"
              :placeholder="t('settings.upstreamModel')"
              :aria-label="t('settings.upstreamModelName')"
              @update:model-value="emit('updateMapping', index, 'upstreamModel', $event)"
            />
          </ZFormItem>
          <ZIconButton
            class="col-start-2 row-span-2 row-start-1 @lg:col-start-auto @lg:row-span-1 @lg:row-start-auto"
            :aria-label="t('settings.deleteMapping')"
            @click="emit('removeMapping', index)"
          >
            <Trash2 class="size-4 text-z-error" />
          </ZIconButton>
        </div>
      </div>
    </template>
  </ZCard>
</template>
