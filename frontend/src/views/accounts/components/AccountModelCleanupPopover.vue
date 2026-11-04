<script setup lang="ts">
import { ZButton, ZCheckbox, ZIconButton, ZPopover, ZScrollbar } from '@codex-proxy/ui'
import { BrushCleaning } from '@lucide/vue'
import { useEventListener } from '@vueuse/core'
import { computed, ref, shallowRef, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  models: string[]
  disabled: boolean
}>()
const emit = defineEmits<{
  remove: [ids: string[]]
}>()
const { t } = useI18n({ useScope: 'global' })
const open = shallowRef(false)
const content = useTemplateRef<HTMLDivElement>('content')
useEventListener(content, 'keydown', handleKeydown)
const selected = ref<string[]>([])
const selectedModels = computed(() => props.models.filter(id => selected.value.includes(id)))

watch(open, () => {
  selected.value = []
})
watch([
  () => props.models,
  () => props.disabled,
], () => {
  open.value = false
})

function select(id: string, checked: boolean) {
  selected.value = checked
    ? [...selected.value, id]
    : selected.value.filter(value => value !== id)
}

function remove() {
  if (props.disabled || !selectedModels.value.length)
    return
  emit('remove', selectedModels.value)
  open.value = false
}

function handleKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape' && open.value) {
    // 先关闭清理浮层，避免丢弃外层账号表单的未保存修改
    event.preventDefault()
    event.stopPropagation()
    open.value = false
  }
}

function focusSelection() {
  content.value?.querySelector<HTMLInputElement>('input')?.focus()
}
</script>

<template>
  <ZPopover v-model="open" placement="bottom-end" :disabled="disabled" @opened="focusSelection">
    <template #reference="{ id }">
      <ZIconButton
        variant="plain"
        :aria-label="t('accounts.modelAccess.cleanupModels')"
        :aria-expanded="open"
        :aria-controls="open ? id : undefined"
        aria-haspopup="dialog"
        :pressed="open"
        :disabled="disabled"
        @keydown="handleKeydown"
      >
        <BrushCleaning class="size-3.5" aria-hidden="true" />
      </ZIconButton>
    </template>
    <div ref="content" class="w-72 max-w-[calc(100vw-2rem)] space-y-3 p-3" role="dialog" :aria-label="t('accounts.modelAccess.cleanupModels')" tabindex="-1">
      <p class="m-0 text-z-sm font-emphasis text-z-text">
        {{ t('accounts.modelAccess.cleanupModels') }}
      </p>
      <p class="m-0 text-z-xs text-z-text-secondary">
        {{ t('accounts.modelAccess.cleanupCatalogHint') }}
      </p>
      <ZScrollbar max-height="12rem">
        <div class="grid gap-2" role="group" :aria-label="t('accounts.modelAccess.cleanupSelection')">
          <ZCheckbox
            v-for="id in models"
            :key="id"
            :model-value="selected.includes(id)"
            :label="id"
            :title="id"
            class="min-w-0"
            @update:model-value="select(id, $event)"
          >
            <template #label>
              <span class="block break-all">{{ id }}</span>
            </template>
          </ZCheckbox>
        </div>
      </ZScrollbar>
      <p class="m-0 text-z-xs text-z-text-secondary">
        {{ t('accounts.modelAccess.cleanupSaveHint') }}
      </p>
      <div class="flex justify-end gap-2">
        <ZButton size="small" @click="open = false">
          {{ t('common.actions.cancel') }}
        </ZButton>
        <ZButton size="small" type="primary" :disabled="disabled || !selectedModels.length" @click="remove">
          {{ t('accounts.modelAccess.cleanupSelected', { count: selectedModels.length }) }}
        </ZButton>
      </div>
    </div>
  </ZPopover>
</template>
